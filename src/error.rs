//! Errors and stuff sent to juiceback

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;

const NOT_FOUND_HTML_TEMPLATE: &str = include_str!("templates/not_found.html");
const TEAPOT_HTML_TEMPLATE: &str = include_str!("templates/teapot_uploading.html");

/// Standard error response body returned by all juicehost endpoints on failure.
#[derive(Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Machine-readable error code (e.g. "FILE_NOT_FOUND", "FORBIDDEN").
    pub error: String,
    /// Human-readable description of what went wrong.
    pub message: String,
}

/// Errors returned by storage backends.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The requested file does not exist.
    #[error("file not found")]
    NotFound,
    /// The target file already exists (conflict on create/rename).
    #[error("conflict: file already exists")]
    Conflict,
    /// The uploaded file exceeds the size limit.
    #[error("payload too large")]
    PayloadTooLarge,
    /// A stream ended at a size other than its required exact length.
    #[error("payload size mismatch")]
    SizeMismatch,
    /// The storage backend has run out of space.
    #[error("insufficient storage")]
    InsufficientStorage,
    /// An I/O or backend-specific error occurred.
    #[error("{0}")]
    Io(String),
}

impl From<StorageError> for JuicehostError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => JuicehostError::NotFound,
            StorageError::Conflict => JuicehostError::Conflict,
            StorageError::PayloadTooLarge => JuicehostError::PayloadTooLarge,
            StorageError::SizeMismatch => JuicehostError::SizeMismatch,
            StorageError::InsufficientStorage => JuicehostError::InsufficientStorage,
            StorageError::Io(_) => JuicehostError::InternalServerError,
        }
    }
}

/// Build a styled HTML 404 response with the Juicebox color scheme.
pub fn not_found_html() -> (StatusCode, Html<String>) {
    (
        StatusCode::NOT_FOUND,
        Html(NOT_FOUND_HTML_TEMPLATE.to_string()),
    )
}

/// Return 418 while a file is still uploading so social previews can retry.
// hehehe teapot
pub fn teapot_html(
    filename: &str,
    public_url: &str,
    public_base_url: &str,
) -> (StatusCode, HeaderMap, Html<String>) {
    let filename = escape_html(filename);
    let public_url = escape_html(public_url);
    let public_base_url = escape_html(public_base_url);
    let og_title = format!("{} is still uploading...", filename);
    let og_description = "This file is being uploaded and will be available soon.";
    let og_image = format!("{}/placeholder-og.png", public_base_url);
    let retry_after = "30";

    let html = TEAPOT_HTML_TEMPLATE
        .replace("__TITLE_FILENAME__", &filename)
        .replace("__OG_TITLE__", &og_title)
        .replace("__OG_DESCRIPTION__", og_description)
        .replace("__OG_IMAGE__", &og_image)
        .replace("__PUBLIC_URL__", &public_url)
        .replace("__RETRY_AFTER__", retry_after);

    let mut headers = HeaderMap::new();
    headers.insert("retry-after", HeaderValue::from_static(retry_after));

    (StatusCode::IM_A_TEAPOT, headers, Html(html))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Errors that could maybe occur during request handling.
#[derive(Debug, Error)]
pub enum JuicehostError {
    /// The requested file does not exist.
    #[error("file not found")]
    NotFound,
    /// The request was malformed or missing required fields.
    #[error("bad request")]
    BadRequest,
    /// The target file already exists (conflict on create/rename).
    #[error("target file already exists")]
    Conflict,
    /// The uploaded file exceeds the size limit.
    #[error("file too large")]
    PayloadTooLarge,
    /// The storage backend has run out of space.
    #[error("insufficient storage")]
    InsufficientStorage,
    /// An unexpected internal error occurred.
    #[error("internal server error")]
    InternalServerError,
    /// The request lacked valid authentication credentials.
    #[error("forbidden")]
    Forbidden,
    /// The uploaded file type was blocked by validation (dangerous extension or magic bytes).
    #[error("{0}")]
    BlockedFileType(String),
    /// A declared or signed request size did not match the body.
    #[error("request body size does not match the signed file size")]
    SizeMismatch,
    /// A configured concurrency limit is currently exhausted.
    #[error("server concurrency limit reached")]
    ServiceUnavailable,
}

impl IntoResponse for JuicehostError {
    fn into_response(self) -> Response {
        let (status, error_code) = match &self {
            JuicehostError::NotFound => (StatusCode::NOT_FOUND, "FILE_NOT_FOUND"),
            JuicehostError::BadRequest => (StatusCode::BAD_REQUEST, "BAD_REQUEST"),
            JuicehostError::Conflict => (StatusCode::CONFLICT, "CONFLICT"),
            JuicehostError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "FILE_TOO_LARGE"),
            JuicehostError::InsufficientStorage => {
                (StatusCode::INSUFFICIENT_STORAGE, "INSUFFICIENT_STORAGE")
            }
            JuicehostError::InternalServerError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR")
            }
            JuicehostError::Forbidden => (StatusCode::FORBIDDEN, "FORBIDDEN"),
            JuicehostError::BlockedFileType(_) => (StatusCode::BAD_REQUEST, "BLOCKED_FILE_TYPE"),
            JuicehostError::SizeMismatch => (StatusCode::BAD_REQUEST, "SIZE_MISMATCH"),
            JuicehostError::ServiceUnavailable => (StatusCode::SERVICE_UNAVAILABLE, "BUSY"),
        };
        let message: String = match &self {
            JuicehostError::NotFound => "File not found".into(),
            JuicehostError::BadRequest => "Bad request".into(),
            JuicehostError::Conflict => "Target file already exists".into(),
            JuicehostError::PayloadTooLarge => "File too large".into(),
            JuicehostError::InsufficientStorage => {
                "This instance is out of storage! Try again later.".into()
            }
            JuicehostError::InternalServerError => "Internal server error".into(),
            JuicehostError::Forbidden => "Forbidden".into(),
            JuicehostError::BlockedFileType(msg) => msg.clone(),
            JuicehostError::SizeMismatch => {
                "Request body size does not match the signed file size".into()
            }
            JuicehostError::ServiceUnavailable => "Server concurrency limit reached".into(),
        };
        (
            status,
            Json(ErrorResponse {
                error: error_code.to_string(),
                message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::response::IntoResponse;

    fn status_for(error: JuicehostError) -> StatusCode {
        let resp: axum::http::Response<Body> = error.into_response();
        resp.status()
    }

    async fn body_for(error: JuicehostError) -> serde_json::Value {
        let resp: axum::http::Response<Body> = error.into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn not_found_returns_404() {
        assert_eq!(status_for(JuicehostError::NotFound), StatusCode::NOT_FOUND);
    }

    #[test]
    fn bad_request_returns_400() {
        assert_eq!(
            status_for(JuicehostError::BadRequest),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn conflict_returns_409() {
        assert_eq!(status_for(JuicehostError::Conflict), StatusCode::CONFLICT);
    }

    #[test]
    fn payload_too_large_returns_413() {
        assert_eq!(
            status_for(JuicehostError::PayloadTooLarge),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn insufficient_storage_returns_507() {
        assert_eq!(
            status_for(JuicehostError::InsufficientStorage),
            StatusCode::INSUFFICIENT_STORAGE
        );
    }

    #[test]
    fn internal_server_error_returns_500() {
        assert_eq!(
            status_for(JuicehostError::InternalServerError),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn forbidden_returns_403() {
        assert_eq!(status_for(JuicehostError::Forbidden), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn blocked_file_type_returns_400_with_message() {
        let body = body_for(JuicehostError::BlockedFileType(
            "Executable files are not allowed".into(),
        ))
        .await;
        assert_eq!(body["error"], "BLOCKED_FILE_TYPE");
        assert_eq!(body["message"], "Executable files are not allowed");
    }

    #[tokio::test]
    async fn not_found_body() {
        let body = body_for(JuicehostError::NotFound).await;
        assert_eq!(body["error"], "FILE_NOT_FOUND");
        assert_eq!(body["message"], "File not found");
    }

    #[tokio::test]
    async fn insufficient_storage_body() {
        let body = body_for(JuicehostError::InsufficientStorage).await;
        assert_eq!(body["error"], "INSUFFICIENT_STORAGE");
    }

    #[test]
    fn not_found_html_contains_404() {
        let (status, html) = not_found_html();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(html.0.contains("404"));
    }

    #[test]
    fn teapot_html_renders_template() {
        let (status, headers, html) = teapot_html(
            "my-file.png",
            "https://juicebox.example/files/id123",
            "https://juicebox.example",
        );
        assert_eq!(status, StatusCode::IM_A_TEAPOT);
        assert_eq!(headers["retry-after"], "30");
        assert!(html.0.contains("Uploading - my-file.png"));
        assert!(html.0.contains("my-file.png is still uploading..."));
        assert!(html.0.contains("https://juicebox.example/files/id123"));
        assert!(!html.0.contains("__OG_TITLE__"));
        assert!(!html.0.contains("__PUBLIC_URL__"));
    }

    #[test]
    fn teapot_html_escapes_dynamic_values() {
        let (_, _, html) = teapot_html(
            "<script>alert(1)</script>",
            "https://example.test/\" onload=\"alert(1)",
            "https://example.test/\" onload=\"alert(1)",
        );
        assert!(!html.0.contains("<script>"));
        assert!(!html.0.contains("\" onload=\""));
        assert!(html.0.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.0.contains("&quot; onload=&quot;"));
    }

    #[test]
    fn storage_error_display() {
        assert_eq!(StorageError::NotFound.to_string(), "file not found");
        assert_eq!(
            StorageError::Conflict.to_string(),
            "conflict: file already exists"
        );
        assert_eq!(
            StorageError::PayloadTooLarge.to_string(),
            "payload too large"
        );
        assert_eq!(
            StorageError::InsufficientStorage.to_string(),
            "insufficient storage"
        );
        assert_eq!(
            StorageError::Io("disk error".into()).to_string(),
            "disk error"
        );
    }

    #[test]
    fn storage_error_to_juicehost_error() {
        let mapping = [
            (StorageError::NotFound, "FILE_NOT_FOUND"),
            (StorageError::Conflict, "CONFLICT"),
            (StorageError::PayloadTooLarge, "FILE_TOO_LARGE"),
            (StorageError::InsufficientStorage, "INSUFFICIENT_STORAGE"),
            (StorageError::Io("x".into()), "INTERNAL_ERROR"),
        ];
        for (se, expected_code) in mapping {
            let je: JuicehostError = se.into();
            let resp: axum::http::Response<Body> = je.into_response();
            let body_bytes = tokio::runtime::Runtime::new().unwrap().block_on(async {
                axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap()
            });
            let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
            assert_eq!(body["error"], expected_code);
        }
    }
}
