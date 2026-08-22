// This was getting too big so i gave it a file.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use std::sync::Arc;
use tower::ServiceExt;

use juicehost::config::Config;
use juicehost::server::build_router;
use juicehost::state::AppState;
use juicehost::storage::{LocalBackend, StorageBackend};

fn test_config(ticket_secret: &str, frontend_url: Option<String>) -> Config {
    Config {
        public_host: "127.0.0.1".into(),
        public_port: 6402,
        quic_host: "127.0.0.1".into(),
        quic_port: 6403,
        quic_cert_path: None,
        quic_max_connections: 256,
        quic_max_requests: 256,
        quic_handshake_seconds: 10,
        quic_idle_seconds: 30,
        quic_request_total_seconds: 600,
        worker_threads: 2,
        files_dir: "unused".into(),
        backend_url: None,
        frontend_url,
        api_key: "test-api-key".into(),
        allowed_origins: vec![],
        danger_level: juiceutils::file_validation::ProtectionLevel::High,
        trusted_proxy_cidrs: vec![],
        s3_bucket: None,
        s3_region: None,
        s3_endpoint: None,
        s3_allow_http: false,
        s3_access_key: None,
        s3_secret_key: None,
        min_free_space_bytes: 0,
        max_file_size_bytes: 10 * 1024 * 1024,
        max_range_response_bytes: 16 * 1024 * 1024,
        max_concurrent_uploads: 16,
        max_concurrent_downloads: 64,
        max_concat_parts: 128,
        tcp_body_inactivity_seconds: 30,
        tcp_request_total_seconds: 600,
        tcp_max_concurrent_requests: 512,
        quick_link: false,
        custom_id: false,
        default_ttl_hours: 24.0,
        allowed_ttl_hours: vec![1.0, 24.0, 168.0],
        ticket_jwt_secret: ticket_secret.into(),
        ip_pepper: "".into(),
        ban_list_file: None,
        ban_sync_url: None,
        ban_sync_interval: 30,
    }
}

async fn test_state(dir: &std::path::Path) -> Arc<AppState> {
    let backend = Arc::new(LocalBackend::new(dir.to_path_buf(), 0).unwrap());
    backend.init_cache().await.unwrap();
    Arc::new(AppState::new(&test_config("", None), backend))
}

async fn frontend_state(dir: &std::path::Path) -> Arc<AppState> {
    let backend = Arc::new(LocalBackend::new(dir.to_path_buf(), 0).unwrap());
    backend.init_cache().await.unwrap();
    Arc::new(AppState::new(
        &test_config("", Some("https://juicefront.example".into())),
        backend,
    ))
}

fn api_key_header() -> (&'static str, &'static str) {
    ("x-juicehost-api-key", "test-api-key")
}

fn ticket(file_id: &str, filename: &str, file_size: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize;
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &serde_json::json!({
            "sub": "dev-1", "user_id": "user-1", "iss": "juiceback-ticket",
            "file_id": file_id, "filename": filename, "mime_type": "text/plain",
            "file_size": file_size, "iat": now, "exp": now + 300,
        }),
        &jsonwebtoken::EncodingKey::from_secret(b"test-ticket-secret"),
    )
    .unwrap()
}

async fn ticket_state(dir: &std::path::Path) -> Arc<AppState> {
    Arc::new(AppState::new(
        &test_config("test-ticket-secret", None),
        Arc::new(LocalBackend::new(dir.to_path_buf(), 0).unwrap()),
    ))
}

#[tokio::test]
async fn health_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn store_and_serve() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let boundary = "----TestBoundary";
    let body_str = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"id\"\r\n\r\n\
         my-file\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"filename\"\r\n\r\n\
         test.txt\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         hello world\r\n\
         --{boundary}--\r\n"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let app = test_state(dir.path()).await;
    let app = build_router(app);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/my-file")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"hello world");
}

#[tokio::test]
async fn serve_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(resp.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn index_redirects_to_frontend_url() {
    let dir = tempfile::tempdir().unwrap();
    let state = frontend_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "https://juicefront.example"
    );
}

#[tokio::test]
async fn index_404_without_frontend_url() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serve_etag_304() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LocalBackend::new(dir.path().to_path_buf(), 0).unwrap();
    backend
        .put("etag1", "test.txt", Bytes::from("data"))
        .await
        .unwrap();

    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/etag1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let etag = resp.headers().get("etag").unwrap().clone();

    let app = test_state(dir.path()).await;
    let app = build_router(app);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/etag1")
                .header("if-none-match", etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(resp.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn ranges_clamp_and_return_416() {
    let dir = tempfile::tempdir().unwrap();
    let backend = LocalBackend::new(dir.path().to_path_buf(), 0).unwrap();
    backend
        .put("range", "file.txt", Bytes::from_static(b"0123456789"))
        .await
        .unwrap();

    let app = build_router(test_state(dir.path()).await);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/f/range")
                .header("range", "bytes=7-99")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-range"], "bytes 7-9/10");
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 32)
            .await
            .unwrap()
            .as_ref(),
        b"789"
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/f/range")
                .header("range", "bytes=10-")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-range"], "bytes */10");
}

#[tokio::test]
async fn ticket_enforces_content_length_and_actual_size() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(ticket_state(dir.path()).await);
    let token = ticket("short", "file.txt", 5);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/upload/short")
                .header("authorization", format!("Bearer {token}"))
                .header("content-length", "4")
                .body(Body::from("data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let token = ticket("short", "file.txt", 5);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/upload/short")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from("data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!dir.path().join("short.txt").exists());
}

#[tokio::test]
async fn ticket_is_not_reusable_for_an_existing_logical_id() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(ticket_state(dir.path()).await);
    let token = ticket("once", "file.txt", 4);
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/internal/file/upload/once")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from("data"))
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(request()).await.unwrap().status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn concat_rejects_invalid_and_duplicate_parts() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(test_state(dir.path()).await);
    for parts in [
        serde_json::json!(["ok", "bad/id"]),
        serde_json::json!(["same", "same"]),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/file/concat")
                    .header(api_key_header().0, api_key_header().1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"target_id":"target","filename":"x.txt","parts":parts})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn delete_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let boundary = "----TestBoundary";
    let body_str = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"id\"\r\n\r\n\
         del-me\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"filename\"\r\n\r\n\
         f.txt\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"f.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         data\r\n\
         --{boundary}--\r\n"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let app = test_state(dir.path()).await;
    let app = build_router(app);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/file/del-me")
                .header(api_key_header().0, api_key_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn rename_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let boundary = "----TestBoundary";
    let body_str = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"id\"\r\n\r\n\
         old-id\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"filename\"\r\n\r\n\
         f.txt\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"f.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         content\r\n\
         --{boundary}--\r\n"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let app = test_state(dir.path()).await;
    let app = build_router(app);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/old-id/rename")
                .header(api_key_header().0, api_key_header().1)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"new_id":"new-id"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_key_required() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let boundary = "----TestBoundary";
    let body_str = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"id\"\r\n\r\n\
         x\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"filename\"\r\n\r\n\
         f.txt\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"f.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         data\r\n\
         --{boundary}--\r\n"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn invalid_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let boundary = "----TestBoundary";
    let body_str = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"id\"\r\n\r\n\
         x\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"filename\"\r\n\r\n\
         f.txt\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"f.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         data\r\n\
         --{boundary}--\r\n"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("x-juicehost-api-key", "wrong-key")
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn storage_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/storage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn multipart_blocks_exe() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let boundary = "----TestBoundary";
    let mut body_bytes = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"id\"\r\n\r\n\
         evil1\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"filename\"\r\n\r\n\
         malware.exe\r\n\
         --{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"malware.exe\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body_bytes.extend_from_slice(b"MZ\x90\x00\x03\x00\x00\x00");
    body_bytes.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "BLOCKED_FILE_TYPE");
}

#[tokio::test]
async fn streaming_blocks_exe_extension() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/evil2/malware.exe")
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(b"MZ\x90\x00\x03\x00\x00\x00".as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "BLOCKED_FILE_TYPE");
}

#[tokio::test]
async fn streaming_allows_safe_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/good1/photo.jpg")
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(b"\xFF\xD8\xFF\xE0jpeg data".as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn streaming_serves_mime_from_magic_bytes_not_extension() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    // PNG bytes uploaded under a .txt name; the sniffed magic bytes must win.
    let png_magic: &[u8] = b"\x89PNG\r\n\x1a\n";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/pngtxt/notes.txt")
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(png_magic))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Fresh backend so the extension cache is rebuilt from the stored filename.
    let app = build_router(test_state(dir.path()).await);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/pngtxt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "image/png"
    );
}

#[tokio::test]
async fn streaming_plain_text_without_magic_keeps_original_extension() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/plaintxt/notes.txt")
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(b"just some plain text".as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let app = build_router(test_state(dir.path()).await);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/plaintxt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/plain; charset=utf-8"
    );
}

#[tokio::test]
async fn internal_endpoints_open_when_no_api_key_configured() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config("", None);
    config.api_key = String::new();
    let backend = Arc::new(LocalBackend::new(dir.path().to_path_buf(), 0).unwrap());
    backend.init_cache().await.unwrap();
    let app = build_router(Arc::new(AppState::new(&config, backend)));

    // Streaming upload with NO api key header must not be 403.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/open1/notes.txt")
                .body(Body::from(b"open data".as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Serve it back.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/open1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

async fn capability_state(dir: &std::path::Path) -> Arc<AppState> {
    let mut config = test_config("", None);
    config.api_key.clear();
    let backend = Arc::new(LocalBackend::new(dir.to_path_buf(), 0).unwrap());
    backend.init_cache().await.unwrap();
    Arc::new(AppState::new(&config, backend))
}

#[tokio::test]
async fn unpaired_host_requires_file_capability_for_delete() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(capability_state(dir.path()).await);
    let upload = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/cap-delete/file.txt")
                .header("x-juicehost-file-capability", "owner-secret")
                .body(Body::from("data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);

    for capability in [None, Some("wrong-secret")] {
        let mut request = Request::builder()
            .method("DELETE")
            .uri("/internal/file/cap-delete");
        if let Some(capability) = capability {
            request = request.header("x-juicehost-file-capability", capability);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/file/cap-delete")
                .header("x-juicehost-file-capability", "owner-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn file_capability_survives_rename() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(capability_state(dir.path()).await);
    let upload = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/cap-old/file.txt")
                .header("x-juicehost-file-capability", "rename-secret")
                .body(Body::from("data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);

    let rename = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/cap-old/rename")
                .header("content-type", "application/json")
                .header("x-juicehost-file-capability", "rename-secret")
                .body(Body::from(r#"{"new_id":"cap-new"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rename.status(), StatusCode::OK);

    let delete = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/file/cap-new")
                .header("x-juicehost-file-capability", "rename-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn concat_requires_shared_part_capability() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(capability_state(dir.path()).await);
    for (id, body) in [("cap-part-a", "a"), ("cap-part-b", "b")] {
        let upload = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/internal/file/stream/{id}/file.txt"))
                    .header("x-juicehost-file-capability", "concat-secret")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(upload.status(), StatusCode::OK);
    }

    let request_body =
        r#"{"target_id":"cap-merged","filename":"file.txt","parts":["cap-part-a","cap-part-b"]}"#;
    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/concat")
                .header("content-type", "application/json")
                .header("x-juicehost-file-capability", "wrong-secret")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let accepted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/concat")
                .header("content-type", "application/json")
                .header("x-juicehost-file-capability", "concat-secret")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let delete = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/file/cap-merged")
                .header("x-juicehost-file-capability", "concat-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn streaming_blocks_magic_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path()).await;
    let app = build_router(state);

    // Renamed .txt that's actually a PE executable -> magic byte check catches it.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/stream/evil3/renamed.txt")
                .header(api_key_header().0, api_key_header().1)
                .body(Body::from(b"MZ\x90\x00\x03\x00\x00\x00".as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "BLOCKED_FILE_TYPE");
}

#[tokio::test]
async fn ticket_upload_uses_only_signed_filename() {
    let dir = tempfile::tempdir().unwrap();
    // Need a ticket JWT secret to exercise the ticket path.
    use juicehost::server::build_router;
    let secret = "test-ticket-secret".to_string();
    let state = Arc::new(AppState::new(
        &test_config(&secret, None),
        Arc::new(LocalBackend::new(dir.path().to_path_buf(), 0).unwrap()),
    ));
    state
        .storage
        .put("_", "seed.txt", Bytes::from("x"))
        .await
        .unwrap();
    let app = build_router(state);

    // Build a signed ticket JWT claiming a safe filename.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize;
    let claims = serde_json::json!({
        "sub": "dev-1",
        "user_id": "user-1",
        "iss": "juiceback-ticket",
        "file_id": "tic-evil",
        "filename": "safe.txt",
        "mime_type": "application/octet-stream",
        "file_size": 9,
        "iat": now,
        "exp": now + 300,
    });
    let header = jsonwebtoken::Header::default();
    let token = jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(b"test-ticket-secret"),
    )
    .unwrap();

    // An unsigned filename header cannot override the signed filename.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/upload/tic-evil")
                .header("authorization", format!("Bearer {token}"))
                .header("x-juicebox-file-name", "malware.exe")
                .body(Body::from(b"safe data".as_slice()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn ticket_upload_uses_signed_filename_extension() {
    let dir = tempfile::tempdir().unwrap();
    use juicehost::server::build_router;
    let secret = "test-ticket-secret".to_string();
    let state = Arc::new(AppState::new(
        &test_config(&secret, None),
        Arc::new(LocalBackend::new(dir.path().to_path_buf(), 0).unwrap()),
    ));
    state
        .storage
        .put("_", "seed.txt", Bytes::from("x"))
        .await
        .unwrap();
    let app = build_router(state);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize;
    let claims = serde_json::json!({
        "sub": "dev-1",
        "user_id": "user-1",
        "iss": "juiceback-ticket",
        "file_id": "tic-good",
        "filename": "report.pdf",
        "mime_type": "application/octet-stream",
        "file_size": 13,
        "iat": now,
        "exp": now + 300,
    });
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(b"test-ticket-secret"),
    )
    .unwrap();

    // The unsigned header is ignored; the signed filename determines extension.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/file/upload/tic-good")
                .header("authorization", format!("Bearer {token}"))
                .header("x-juicebox-file-name", "wrong.txt")
                .body(Body::from("%PDF-1.4 data"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Served with the PDF MIME type from the signed ticket.
    let app = test_state(dir.path()).await;
    let app = build_router(app);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/tic-good")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.starts_with("application/pdf"));
}

#[tokio::test]
async fn banned_ip_blocked_from_file_serving() {
    let dir = tempfile::tempdir().unwrap();
    let banned_ip = "203.0.113.9";
    let mut config = test_config("", None);
    config.ip_pepper = "test-pepper".into();
    config.trusted_proxy_cidrs = juiceutils::proxy::parse_trusted_proxy_cidrs("0.0.0.0/0").unwrap();
    let state = Arc::new(juicehost::state::AppState::new(
        &config,
        Arc::new(LocalBackend::new(dir.path().to_path_buf(), 0).unwrap()),
    ));
    state
        .ban_list
        .merge_hashes([juiceutils::ban::hash_ip_for_ban(banned_ip, "test-pepper")]);
    let app = build_router(state);

    // Banned client (via X-Forwarded-For, as juicefront would send it) gets 403.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/f/some-file")
                .header("x-forwarded-for", banned_ip)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // A different client is allowed through (file doesn't exist -> not 403).
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/f/some-file")
                .header("x-forwarded-for", "198.51.100.7")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
}
