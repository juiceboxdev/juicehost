//! Request handlers for serving, storing, renaming, and deleting files.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Redirect, Response},
};
use bytes::Bytes;
use futures::StreamExt;

use crate::error::StorageError;
use crate::error::{not_found_html, teapot_html, JuicehostError};
use crate::state::AppState;
use crate::storage;

fn optional_file_capability(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-juicehost-file-capability")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn required_file_capability(
    headers: &HeaderMap,
    api_key: &str,
) -> Result<Option<String>, JuicehostError> {
    let has_api_key = headers
        .get("x-juicehost-api-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|provided| juiceutils::constant_time_eq(api_key, provided));
    if !api_key.is_empty() && has_api_key {
        return Ok(None);
    }
    optional_file_capability(headers)
        .ok_or(JuicehostError::Forbidden)
        .map(Some)
}

/// Maximum Cache-Control max-age for immutable file responses (1 year in seconds).
const CACHE_MAX_AGE: &str = "public, max-age=31536000, immutable";

/// Header sent on peer health probes. The receiving side skips probing back so
/// juiceback and juicehost don't recurse into each other's /api/health forever.
//                                                            ^ yes this happened.
const HEALTH_PROBE_HEADER: &str = "x-health-probe";

/// Serve the index page. Redirects to the configured juicefront URL, or 404s
/// when no frontend is configured.
#[utoipa::path(
    get,
    path = "/",
    responses(
        (status = 308, description = "Permanent redirect to FRONTEND_URL"),
        (status = 404, description = "No frontend configured"),
    ),
    tag = "General",
)]
pub async fn index_handler(State(state): State<Arc<AppState>>) -> Response {
    match &state.frontend_url {
        Some(url) => Redirect::permanent(url).into_response(),
        None => not_found_html().into_response(),
    }
}

/// Validate a file ID: must be non-empty and contain only alphanumeric, `-`, or `_`.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Serve `/f/*path`; the optional extension is ignored for lookup.
#[tracing::instrument(skip_all)]
pub async fn serve_file_wildcard(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<Response<Body>, JuicehostError> {
    let id = path.split('.').next().unwrap_or(&path).to_string();
    serve_file_inner(state, headers, id).await
}

async fn serve_file_inner(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: String,
) -> Result<Response<Body>, JuicehostError> {
    if !is_valid_id(&id) {
        return Err(JuicehostError::BadRequest);
    }

    let file_meta = match state.storage.stat(&id).await {
        Ok(meta) => meta,
        Err(StorageError::NotFound) => {
            // File not on disk. Check juiceback to see if it's still uploading.
            if let Some(ref backend_url) = state.backend_url {
                let status_url = format!("{}/internal/file/{}/status", backend_url, id);
                match reqwest::get(&status_url).await {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            if body.get("status").and_then(|s| s.as_str()) == Some("uploading") {
                                let filename = body
                                    .get("filename")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("upload");
                                return Ok(teapot_html(filename, "", "").into_response());
                            }
                        }
                    }
                    _ => {} // Fall through to alias check
                }
            }

            // Check whether this is an old ID that was renamed.
            if let Some(ref backend_url) = state.backend_url {
                let alias_url = format!("{}/internal/alias/{}", backend_url, id);
                if let Ok(resp) = reqwest::get(&alias_url).await {
                    if resp.status().is_success() {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            if let Some(new_url) = body.get("url").and_then(|u| u.as_str()) {
                                return Response::builder()
                                    .status(StatusCode::MOVED_PERMANENTLY)
                                    .header(header::LOCATION, new_url)
                                    .body(Body::empty())
                                    .map_err(|_| JuicehostError::InternalServerError);
                            }
                        }
                    }
                }
            }

            return Ok(not_found_html().into_response());
        }
        Err(_) => return Ok(not_found_html().into_response()),
    };

    let etag = &file_meta.etag;

    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
        if let Ok(val) = if_none_match.to_str() {
            if val.trim_matches('"') == etag.trim_matches('"') {
                return Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(header::CACHE_CONTROL, CACHE_MAX_AGE)
                    .header(header::ETAG, etag)
                    .body(Body::empty())
                    .map_err(|_| JuicehostError::InternalServerError);
            }
        }
    }

    let mime_str = storage::guess_mime(&file_meta.extension);
    let total_size = file_meta.size;

    let permit = Arc::clone(&state.download_semaphore)
        .try_acquire_owned()
        .map_err(|_| JuicehostError::ServiceUnavailable)?;
    if let Some(range_header) = headers.get(header::RANGE) {
        if let Ok(range_val) = range_header.to_str() {
            match parse_range(range_val, total_size, state.max_range_response_bytes) {
                RangeResult::Satisfiable(start, end) => {
                    let range_stream = state
                        .storage
                        .get_range_stream(&id, start, end)
                        .await
                        .map_err(JuicehostError::from)?;
                    let content_len = end - start + 1;
                    let stream = range_stream.map(move |item| {
                        let _ = &permit;
                        item
                    });
                    return Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_TYPE, &mime_str)
                        .header(header::CONTENT_LENGTH, content_len)
                        .header(
                            header::CONTENT_RANGE,
                            format!("bytes {}-{}/{}", start, end, total_size),
                        )
                        .header(header::CACHE_CONTROL, CACHE_MAX_AGE)
                        .header(header::ETAG, etag)
                        .header(header::ACCEPT_RANGES, "bytes")
                        .body(Body::from_stream(stream))
                        .map_err(|_| JuicehostError::InternalServerError);
                }
                RangeResult::Unsatisfiable => {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{total_size}"))
                        .header(header::ACCEPT_RANGES, "bytes")
                        .body(Body::empty())
                        .map_err(|_| JuicehostError::InternalServerError);
                }
                RangeResult::Ignore => {}
            }
        }
    }

    let stream = state
        .storage
        .get_stream(&id)
        .await
        .map_err(JuicehostError::from)?;
    let stream = stream.map(move |item| {
        let _ = &permit;
        item
    });
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &mime_str)
        .header(header::CONTENT_LENGTH, total_size)
        .header(header::CACHE_CONTROL, CACHE_MAX_AGE)
        .header(header::ETAG, etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .body(body)
        .map_err(|_| JuicehostError::InternalServerError)
}

/// Parse a `Range: bytes=START-END` header.
/// Returns (start, end) where both are inclusive byte offsets.
#[derive(Debug, PartialEq)]
enum RangeResult {
    Satisfiable(u64, u64),
    Unsatisfiable,
    Ignore,
}

fn parse_range(range_val: &str, total_size: u64, max_len: u64) -> RangeResult {
    let range_val = range_val.trim();
    let Some(range_val) = range_val.strip_prefix("bytes=") else {
        return RangeResult::Ignore;
    };
    let range_val = range_val.trim();
    if range_val.contains(',') {
        return RangeResult::Ignore;
    }
    if total_size == 0 {
        return RangeResult::Unsatisfiable;
    }

    if let Some((start_str, end_str)) = range_val.split_once('-') {
        let start_str = start_str.trim();
        let end_str = end_str.trim();

        if start_str.is_empty() {
            // Suffix range: bytes=-500 (last 500 bytes)
            let Ok(suffix_len) = end_str.parse::<u64>() else {
                return RangeResult::Ignore;
            };
            if suffix_len == 0 {
                return RangeResult::Unsatisfiable;
            }
            let suffix_len = suffix_len.min(total_size).min(max_len);
            RangeResult::Satisfiable(total_size - suffix_len, total_size - 1)
        } else {
            let Ok(start) = start_str.parse::<u64>() else {
                return RangeResult::Ignore;
            };
            if start >= total_size {
                return RangeResult::Unsatisfiable;
            }
            let requested_end = if end_str.is_empty() {
                total_size - 1
            } else {
                let Ok(end) = end_str.parse::<u64>() else {
                    return RangeResult::Ignore;
                };
                if end < start {
                    return RangeResult::Ignore;
                }
                end.min(total_size - 1)
            };
            let end = requested_end.min(start.saturating_add(max_len - 1));
            RangeResult::Satisfiable(start, end)
        }
    } else {
        RangeResult::Ignore
    }
}

#[utoipa::path(
    post,
    path = "/internal/file",
    request_body(content_type = "multipart/form-data", description = "Multipart form with id, filename, and file fields"),
    responses(
        (status = 200, description = "File stored successfully", body = serde_json::Value),
        (status = 400, description = "Missing or invalid file ID"),
        (status = 403, description = "Invalid or missing API key"),
        (status = 409, description = "File with this ID already exists"),
        (status = 413, description = "File exceeds size limit"),
        (status = 507, description = "Insufficient storage space"),
    ),
    tag = "Internal",
)]
#[tracing::instrument(skip_all)]
pub async fn store_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, JuicehostError> {
    let _permit = Arc::clone(&state.upload_semaphore)
        .try_acquire_owned()
        .map_err(|_| JuicehostError::ServiceUnavailable)?;
    let mut file_id = String::new();
    let mut filename = String::new();
    let mut file_data: Option<Bytes> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| JuicehostError::BadRequest)?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "id" => {
                file_id = field.text().await.map_err(|_| JuicehostError::BadRequest)?;
            }
            "filename" => {
                filename = field.text().await.map_err(|_| JuicehostError::BadRequest)?;
            }
            "file" => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|_| JuicehostError::BadRequest)?;
                if data.len() as u64 > state.max_file_size_bytes {
                    return Err(JuicehostError::PayloadTooLarge);
                }
                file_data = Some(data);
            }
            _ => {}
        }
    }

    let data = match file_data {
        Some(d) => d,
        None => return Err(JuicehostError::BadRequest),
    };

    if file_id.is_empty() || !is_valid_id(&file_id) || filename.is_empty() {
        return Err(JuicehostError::BadRequest);
    }

    // Validate file type using magic bytes.
    validate_or_block(
        &filename,
        &data,
        state.danger_level,
        &format!("(id={file_id})"),
    )?;

    let capability = optional_file_capability(&headers);

    match capability {
        Some(cap) => {
            state
                .storage
                .put_with_capability(
                    &file_id,
                    &content_filename(&filename, infer::get(&data).as_ref()),
                    data,
                    &cap,
                )
                .await
        }
        None => {
            state
                .storage
                .put(
                    &file_id,
                    &content_filename(&filename, infer::get(&data).as_ref()),
                    data,
                )
                .await
        }
    }
    .map_err(JuicehostError::from)?;

    tracing::info!("stored file: {} ({})", file_id, filename);

    Ok(Json(serde_json::json!({"status": "ok", "id": file_id})))
}

/// Rename a file (new ID). body: {"new_id": "..."}. local = rename, s3 = copy+delete.
#[utoipa::path(
    post,
    path = "/internal/file/{id}/rename",
    params(
        ("id" = String, Path, description = "Current file ID"),
    ),
    request_body(content = serde_json::Value, description = "JSON with `new_id` field"),
    responses(
        (status = 200, description = "File renamed", body = serde_json::Value),
        (status = 400, description = "Invalid new ID or missing body"),
        (status = 403, description = "Invalid or missing API key"),
        (status = 404, description = "Source file not found"),
        (status = 409, description = "Target file ID already exists"),
    ),
    tag = "Internal",
)]
pub async fn rename_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, JuicehostError> {
    let new_id = payload
        .get("new_id")
        .and_then(|v| v.as_str())
        .ok_or(JuicehostError::BadRequest)?;

    if !is_valid_id(new_id) {
        return Err(JuicehostError::BadRequest);
    }

    let capability = required_file_capability(&headers, &state.api_key)?;

    match capability {
        Some(cap) => {
            state
                .storage
                .rename_with_capability(&id, new_id, &cap)
                .await
        }
        None => state.storage.rename(&id, new_id).await,
    }
    .map_err(JuicehostError::from)?;

    tracing::info!("renamed file: {} -> {}", id, new_id);

    Ok(Json(
        serde_json::json!({"status": "ok", "old_id": id, "new_id": new_id}),
    ))
}

/// Delete a stored file by its ID.
///
/// Returns `204 No Content` on success. Returns 404 if the file does not exist.
#[utoipa::path(
    delete,
    path = "/internal/file/{id}",
    params(
        ("id" = String, Path, description = "The file ID to delete"),
    ),
    responses(
        (status = 204, description = "File deleted"),
        (status = 403, description = "Invalid or missing API key"),
        (status = 404, description = "File not found"),
    ),
    tag = "Internal",
)]
pub async fn delete_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, JuicehostError> {
    let capability = required_file_capability(&headers, &state.api_key)?;

    let deleted = match capability {
        Some(cap) => state.storage.delete_with_capability(&id, &cap).await,
        None => state.storage.delete(&id).await,
    }
    .map_err(JuicehostError::from)?;

    if !deleted {
        return Err(JuicehostError::NotFound);
    }

    tracing::info!("deleted file: {}", id);
    Ok(StatusCode::NO_CONTENT)
}

// sniff sniff
/// Validate a file (extension + magic bytes) against the configured danger level
/// and return a `BlockedFileType` error when it must be rejected. `context` is
/// appended to the rejection log line (e.g. `"(id=abc)"`).
fn validate_or_block(
    filename: &str,
    bytes: &[u8],
    danger_level: juiceutils::file_validation::ProtectionLevel,
    context: &str,
) -> Result<(), JuicehostError> {
    use juiceutils::file_validation::{friendly_block_reason, FileValidation};
    match juiceutils::file_validation::validate_file(filename, bytes, danger_level) {
        FileValidation::Allowed => Ok(()),
        FileValidation::BlockedExtension { ext, tier } => {
            tracing::warn!(
                "blocked file upload: extension .{} ({:?} tier) {}",
                ext,
                tier,
                context
            );
            Err(JuicehostError::BlockedFileType(friendly_block_reason(tier)))
        }
        FileValidation::BlockedMagic { description, tier } => {
            tracing::warn!(
                "blocked file upload: {} ({:?} tier) {}",
                description,
                tier,
                context
            );
            Err(JuicehostError::BlockedFileType(friendly_block_reason(tier)))
        }
        FileValidation::Empty => Err(JuicehostError::BlockedFileType(
            "Empty files are not allowed".into(),
        )),
    }
}

// https://c.tenor.com/Z6MpB_quDXoAAAAC/tenor.gif
/// Sniff the first up-to-512 bytes of a streaming body and validate the file type
/// (extension + magic bytes) against the configured danger level.
async fn sniff_and_validate(
    body: axum::body::Body,
    filename: &str,
    danger_level: juiceutils::file_validation::ProtectionLevel,
) -> Result<(axum::body::Body, Option<infer::Type>), JuicehostError> {
    use futures::StreamExt;

    const SNIFF_BYTES: usize = 512;

    let mut stream = body.into_data_stream();
    let mut prefix: Vec<u8> = Vec::with_capacity(SNIFF_BYTES);
    let mut tail: Option<Bytes> = None;

    while prefix.len() < SNIFF_BYTES {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if chunk.len() >= SNIFF_BYTES - prefix.len() {
                    let take = SNIFF_BYTES - prefix.len();
                    prefix.extend_from_slice(&chunk[..take]);
                    tail = Some(chunk.slice(take..));
                    break;
                }
                prefix.extend_from_slice(&chunk);
            }
            Some(Err(_)) => return Err(JuicehostError::InternalServerError),
            None => break,
        }
    }

    validate_or_block(filename, &prefix, danger_level, "")?;

    // Rebuild the body from the sniffed prefix + the remaining stream.
    let mut chunks: Vec<Result<Bytes, axum::Error>> = vec![Ok(Bytes::from(prefix.clone()))];
    if let Some(t) = tail {
        chunks.push(Ok(t));
    }
    let body = axum::body::Body::from_stream(futures::stream::iter(chunks).chain(stream));

    // Detect the MIME type from the magic bytes so the stored extension (and thus
    // the served Content-Type) reflects the actual content, not the client-chosen
    // filename. Detection is best-effort: text and unknown formats return None.
    let detected = infer::get(&prefix);
    Ok((body, detected))
}

/// Rewrite a filename so storage uses the content-detected extension. When magic-byte
/// detection found a signature (e.g. a PNG uploaded as `notes.txt`), the stored file
/// gets `.png` and is served as `image/png` and it falls back to the original filename
/// when nothing was detected.
fn content_filename(filename: &str, detected: Option<&infer::Type>) -> String {
    match detected {
        Some(typ) => {
            let stem = std::path::Path::new(filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("file");
            format!("{}.{}", stem, typ.extension())
        }
        None => filename.to_string(),
    }
}

#[utoipa::path(
    post,
    path = "/internal/file/stream/{id}/{filename}",
    params(
        ("id" = String, Path, description = "The file ID"),
        ("filename" = String, Path, description = "The original filename"),
    ),
    request_body(content_type = "application/octet-stream", description = "Raw file bytes"),
    responses(
        (status = 200, description = "File stored successfully", body = serde_json::Value),
        (status = 400, description = "Invalid file ID or blocked file type"),
        (status = 403, description = "Invalid or missing API key"),
        (status = 409, description = "File with this ID already exists"),
        (status = 413, description = "File exceeds size limit"),
        (status = 507, description = "Insufficient storage space"),
    ),
    tag = "Internal",
)]
#[tracing::instrument(skip_all)]
pub async fn store_file_streaming(
    State(state): State<Arc<AppState>>,
    Path((id, filename)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<serde_json::Value>, JuicehostError> {
    if !is_valid_id(&id) {
        return Err(JuicehostError::BadRequest);
    }

    let _permit = Arc::clone(&state.upload_semaphore)
        .try_acquire_owned()
        .map_err(|_| JuicehostError::ServiceUnavailable)?;
    let handler_start = std::time::Instant::now();

    let capability = optional_file_capability(&headers);

    let (body, detected) = sniff_and_validate(body, &filename, state.danger_level).await?;
    let max_size = state.max_file_size_bytes;
    let stream = sized_stream(body, max_size, None);
    let storage_filename = content_filename(&filename, detected.as_ref());

    let total = match capability {
        Some(cap) => {
            state
                .storage
                .put_stream_with_capability(&id, &storage_filename, Box::pin(stream), &cap)
                .await
        }
        None => {
            state
                .storage
                .put_stream(&id, &storage_filename, Box::pin(stream))
                .await
        }
    }
    .map_err(JuicehostError::from)?;

    let total_time = handler_start.elapsed();
    let bytes_per_sec = if total_time.as_secs_f64() > 0.0 {
        total as f64 / total_time.as_secs_f64()
    } else {
        0.0
    };
    tracing::info!(
        "stored file (streaming): {} ({}) bytes={} {:.2} MB/s",
        id,
        filename,
        total,
        bytes_per_sec / (1024.0 * 1024.0),
    );

    Ok(Json(serde_json::json!({"status": "ok", "id": id})))
}

#[utoipa::path(
    get,
    path = "/api/health",
    responses(
        (status = 200, description = "Health status. Storage metrics live at /api/storage", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn health(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let mut body = serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "juicehost": "ok",
    });

    // Skip the peer probe when this request was itself a health probe.
    if !headers.contains_key(HEALTH_PROBE_HEADER) {
        if let Some(ref backend_url) = state.backend_url {
            body["juiceback"] = serde_json::json!(if check_backend_health(backend_url).await {
                "ok"
            } else {
                "unreachable"
            });
        }
    }

    let mut resp = Response::new(Body::from(serde_json::to_string(&body).unwrap_or_default()));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Probes the juiceback health endpoint if it is configured
async fn check_backend_health(backend_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let url = format!("{}/api/health", backend_url.trim_end_matches('/'));
    let Ok(resp) = client
        .get(&url)
        .header(HEALTH_PROBE_HEADER, "1")
        .send()
        .await
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<serde_json::Value>()
        .await
        .map(|v| {
            v.get("status")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s == "ok")
        })
        .unwrap_or(false)
}

#[utoipa::path(
    get,
    path = "/api/storage",
    responses(
        (status = 200, description = "Storage metrics", body = storage::StorageMetrics),
    ),
    tag = "General",
)]
pub async fn storage_handler(State(state): State<Arc<AppState>>) -> Json<storage::StorageMetrics> {
    Json(state.storage.storage_metrics(state.min_free_space_bytes))
}

/// Concatenate multiple part files into a single target file, then delete the parts.

#[utoipa::path(
    post,
    path = "/internal/file/concat",
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Files concatenated", body = serde_json::Value),
        (status = 400, description = "Invalid target or parts"),
        (status = 403, description = "Invalid or missing API key"),
        (status = 413, description = "Combined file exceeds size limit"),
    ),
    tag = "Internal",
)]
pub async fn concat_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, JuicehostError> {
    let target_id = payload
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or(JuicehostError::BadRequest)?;

    let filename = payload
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("upload.bin");

    let parts: Vec<&str> = payload
        .get("parts")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.iter().map(|v| v.as_str()).collect())
        .ok_or(JuicehostError::BadRequest)?;

    let unique: std::collections::HashSet<_> = parts.iter().copied().collect();
    if parts.is_empty()
        || parts.len() > state.max_concat_parts
        || unique.len() != parts.len()
        || !is_valid_id(target_id)
        || parts.iter().any(|id| !is_valid_id(id) || *id == target_id)
    {
        return Err(JuicehostError::BadRequest);
    }

    let mut aggregate = 0u64;
    for part in &parts {
        let size = state
            .storage
            .stat(part)
            .await
            .map_err(JuicehostError::from)?
            .size;
        aggregate = aggregate
            .checked_add(size)
            .ok_or(JuicehostError::PayloadTooLarge)?;
        if aggregate > state.max_file_size_bytes {
            return Err(JuicehostError::PayloadTooLarge);
        }
    }
    let _permit = Arc::clone(&state.upload_semaphore)
        .try_acquire_owned()
        .map_err(|_| JuicehostError::ServiceUnavailable)?;

    let capability = required_file_capability(&headers, &state.api_key)?;

    match capability {
        Some(cap) => {
            state
                .storage
                .concat_with_capability(target_id, filename, &parts, &cap)
                .await
        }
        None => state.storage.concat(target_id, filename, &parts).await,
    }
    .map_err(JuicehostError::from)?;

    tracing::info!("concat: {} <- {:?} ({})", target_id, parts, filename);

    Ok(Json(serde_json::json!({"status": "ok", "id": target_id})))
}

/// Return the instance's upload configuration as JSON.
#[utoipa::path(
    get,
    path = "/api/config",
    responses(
        (status = 200, description = "Instance configuration", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn config_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let danger_level_str = state.danger_level.as_str();
    Json(serde_json::json!({
        "max_file_size_bytes": state.max_file_size_bytes,
        "default_ttl_hours": state.default_ttl_hours,
        "allowed_ttl_hours": state.allowed_ttl_hours,
        "danger_level": danger_level_str,
        "quick_link": state.quick_link,
        "custom_id": state.custom_id,
        "ultrafast": !state.ticket_jwt_secret.is_empty(),
    }))
}

/// Store a file from juicebox-plus, ticket JWT in Authorization header and the body as octet-stream.
#[utoipa::path(
    post,
    path = "/internal/file/upload/{id}",
    params(
        ("id" = String, Path, description = "The file ID from the ticket"),
    ),
    request_body(content_type = "application/octet-stream", description = "Raw file bytes"),
    responses(
        (status = 200, description = "File stored successfully", body = serde_json::Value),
        (status = 400, description = "Invalid file ID"),
        (status = 403, description = "Invalid or expired ticket JWT"),
        (status = 409, description = "File with this ID already exists"),
        (status = 413, description = "File exceeds size limit"),
        (status = 507, description = "Insufficient storage space"),
    ),
    tag = "Internal",
)]
#[tracing::instrument(skip_all)]
pub async fn store_file_ticket(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<serde_json::Value>, JuicehostError> {
    if !is_valid_id(&id) {
        return Err(JuicehostError::BadRequest);
    }

    let token = juiceutils::extract_bearer_token(&headers).ok_or(JuicehostError::Forbidden)?;

    #[derive(serde::Deserialize)]
    struct TicketClaims {
        sub: String,
        file_id: String,
        filename: String,
        file_size: u64,
        file_capability: Option<String>,
    }

    use jsonwebtoken::{decode, DecodingKey};

    let ticket = decode::<TicketClaims>(
        token,
        &DecodingKey::from_secret(state.ticket_jwt_secret.as_bytes()),
        &crate::ticket::ticket_validation(),
    )
    .map_err(|e| {
        tracing::warn!("ticket JWT validation failed: {}", e);
        JuicehostError::Forbidden
    })?;

    if ticket.claims.file_id != id {
        tracing::warn!(
            "ticket file_id mismatch: ticket={} path={}",
            ticket.claims.file_id,
            id
        );
        return Err(JuicehostError::Forbidden);
    }
    if ticket.claims.file_size > state.max_file_size_bytes {
        return Err(JuicehostError::PayloadTooLarge);
    }
    if let Some(content_length) = headers.get(header::CONTENT_LENGTH) {
        let declared = content_length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(JuicehostError::BadRequest)?;
        if declared != ticket.claims.file_size {
            return Err(JuicehostError::SizeMismatch);
        }
    }
    let real_filename = ticket.claims.filename.clone();
    let _permit = Arc::clone(&state.upload_semaphore)
        .try_acquire_owned()
        .map_err(|_| JuicehostError::ServiceUnavailable)?;

    let handler_start = std::time::Instant::now();

    let capability = ticket
        .claims
        .file_capability
        .clone()
        .or_else(|| optional_file_capability(&headers));

    let (body, detected) = sniff_and_validate(body, &real_filename, state.danger_level).await?;
    let stream = sized_stream(body, ticket.claims.file_size, Some(ticket.claims.file_size));
    let storage_filename = content_filename(&real_filename, detected.as_ref());

    let total = match capability {
        Some(cap) => {
            state
                .storage
                .put_stream_with_capability(&id, &storage_filename, Box::pin(stream), &cap)
                .await
        }
        None => {
            state
                .storage
                .put_stream(&id, &storage_filename, Box::pin(stream))
                .await
        }
    }
    .map_err(JuicehostError::from)?;

    let total_time = handler_start.elapsed();
    let bytes_per_sec = if total_time.as_secs_f64() > 0.0 {
        total as f64 / total_time.as_secs_f64()
    } else {
        0.0
    };
    tracing::info!(
        "stored file (ticket): {} ({}) bytes={} {:.2} MB/s device={}",
        id,
        real_filename,
        total,
        bytes_per_sec / (1024.0 * 1024.0),
        ticket.claims.sub,
    );

    Ok(Json(serde_json::json!({"status": "ok", "id": id})))
}

pub(crate) fn deadline_body(
    body: Body,
    inactivity: std::time::Duration,
    total: std::time::Duration,
) -> Body {
    let deadline = tokio::time::Instant::now() + total;
    let stream = futures::stream::unfold(Some(body.into_data_stream()), move |state| async move {
        let mut stream = state?;
        let wait = inactivity.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        if wait.is_zero() {
            return Some((
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request body total deadline exceeded",
                )),
                None,
            ));
        }
        match tokio::time::timeout(wait, stream.next()).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk), Some(stream))),
            Ok(Some(Err(error))) => Some((Err(std::io::Error::other(error)), None)),
            Ok(None) => None,
            Err(_) => Some((
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request body inactivity deadline exceeded",
                )),
                None,
            )),
        }
    })
    .fuse();
    Body::from_stream(stream)
}

fn sized_stream(body: Body, max_size: u64, exact_size: Option<u64>) -> storage::ByteStream {
    let stream = body.into_data_stream();
    Box::pin(futures::stream::try_unfold(
        (stream, 0u64),
        move |(mut stream, total)| async move {
            match stream.next().await {
                Some(Ok(chunk)) => {
                    let next = total
                        .checked_add(chunk.len() as u64)
                        .ok_or(StorageError::PayloadTooLarge)?;
                    if next > max_size {
                        return Err(if exact_size.is_some() {
                            StorageError::SizeMismatch
                        } else {
                            StorageError::PayloadTooLarge
                        });
                    }
                    Ok(Some((chunk, (stream, next))))
                }
                Some(Err(error)) => Err(StorageError::Io(format!("request body failed: {error}"))),
                None if exact_size.is_some_and(|expected| expected != total) => {
                    Err(StorageError::SizeMismatch)
                }
                None => Ok(None),
            }
        },
    ))
}

/// Report whether a file exists in the configured storage backend and its size.
#[utoipa::path(
    get,
    path = "/internal/file/{id}/stat",
    params(("id" = String, Path, description = "The file ID")),
    responses(
        (status = 200, description = "File status", body = serde_json::Value),
        (status = 400, description = "Invalid file ID"),
        (status = 403, description = "Invalid or missing API key"),
    ),
    tag = "Internal",
)]
#[tracing::instrument(skip_all)]
pub async fn stat_file(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, JuicehostError> {
    if !is_valid_id(&id) {
        return Err(JuicehostError::BadRequest);
    }

    match state.storage.stat(&id).await {
        Ok(meta) => Ok(Json(serde_json::json!({
            "exists": true,
            "id": id,
            "size_bytes": meta.size,
            "extension": meta.extension,
        }))),
        Err(StorageError::NotFound) => Ok(Json(serde_json::json!({
            "exists": false,
            "id": id,
        }))),
        Err(_) => Err(JuicehostError::InternalServerError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_id_valid() {
        assert!(is_valid_id("abc-123"));
        assert!(is_valid_id("test_file"));
        assert!(is_valid_id("ABC123"));
    }

    #[test]
    fn is_valid_id_empty() {
        assert!(!is_valid_id(""));
    }

    #[test]
    fn is_valid_id_with_spaces() {
        assert!(!is_valid_id("has space"));
    }

    #[test]
    fn is_valid_id_with_special_chars() {
        assert!(!is_valid_id("file@name"));
        assert!(!is_valid_id("file.txt"));
        assert!(!is_valid_id("file/here"));
    }

    #[test]
    fn is_valid_id_with_underscores_and_dashes() {
        assert!(is_valid_id("my_file-123"));
    }

    #[test]
    fn parse_range_basic() {
        assert_eq!(
            parse_range("bytes=0-99", 1000, 1000),
            RangeResult::Satisfiable(0, 99)
        );
    }

    #[test]
    fn parse_range_suffix() {
        assert_eq!(
            parse_range("bytes=-500", 1000, 1000),
            RangeResult::Satisfiable(500, 999)
        );
    }

    #[test]
    fn parse_range_open_ended() {
        assert_eq!(
            parse_range("bytes=500-", 1000, 1000),
            RangeResult::Satisfiable(500, 999)
        );
    }

    #[test]
    fn parse_range_out_of_bounds() {
        assert_eq!(
            parse_range("bytes=999-1999", 1000, 1000),
            RangeResult::Satisfiable(999, 999)
        );
    }

    #[test]
    fn parse_range_invalid_format() {
        assert_eq!(parse_range("bytes=", 1000, 1000), RangeResult::Ignore);
        assert_eq!(parse_range("nope", 1000, 1000), RangeResult::Ignore);
    }

    #[test]
    fn parse_range_start_beyond_end() {
        assert_eq!(
            parse_range("bytes=500-100", 1000, 1000),
            RangeResult::Ignore
        );
    }

    #[test]
    fn parse_range_clamps_end_suffix_and_response_size() {
        assert_eq!(
            parse_range("bytes=10-9999", 1000, 20),
            RangeResult::Satisfiable(10, 29)
        );
        assert_eq!(
            parse_range("bytes=-5000", 1000, 2000),
            RangeResult::Satisfiable(0, 999)
        );
        assert_eq!(
            parse_range("bytes=1000-", 1000, 1000),
            RangeResult::Unsatisfiable
        );
        assert_eq!(
            parse_range("bytes=0-1,4-5", 1000, 1000),
            RangeResult::Ignore
        );
    }

    #[tokio::test]
    async fn deadline_body_terminates_after_timeout() {
        let body = Body::from_stream(futures::stream::pending::<Result<Bytes, std::io::Error>>());
        let mut stream = deadline_body(
            body,
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(1),
        )
        .into_data_stream();

        assert!(stream.next().await.unwrap().is_err());
        assert!(stream.next().await.is_none());
    }
}
