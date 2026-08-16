//! router builder and TCP server starter for juicehost.
//! wires routes and middleware, then starts the server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, Request},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};
use sentry::integrations::tower::NewSentryLayer;
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::api_doc::ApiDoc;
use crate::config::Config;
use crate::error::JuicehostError;
use crate::handlers;
use crate::state::AppState;
use crate::ticket::verify_ticket_jwt;
use juiceutils::add_security_headers;
use juiceutils::shutdown_signal;
use utoipa::OpenApi;

pub const BANNER_ART: &str = r#"
 ▄▄ ▄▄ ▄▄ ▄▄  ▄▄▄▄  ▄▄▄▄ ▄▄ ▄▄  ▄▄▄   ▄▄▄▄ ▄▄▄▄▄
 ██ ██ ██ ▄▄ ██▀██ ▄█▀▀▀ ██▄▄█ ▄█▀██ ▒█▀▀▀ ▀██▀▀
▄▄█ ▓▓ █▀ ▓▓ ██ ▀▀ ▓▓▀▀  █▀▀██ ▓▓ ▓▓ ▀▀▓▓▄  ▒▒
▀▀▀ ▀▀▀▀  ▀▀ ▀▀▀▀▀ ▀▀▀▀▀ ▀▀ ▀▀  ▀▀▀  ▀▀▀▀▀  ▀▀
"#;

async fn fallback_404() -> impl IntoResponse {
    crate::error::not_found_html()
}

use juiceutils::constant_time_eq;

/// Ban check, check config.rs
async fn ban_check_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request<Body>,
    next: middleware::Next,
) -> Response {
    if state.ban_list.enabled() {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or_else(|| "0.0.0.0".parse().unwrap());
        let ip = juiceutils::proxy::client_ip(&headers, peer, &state.trusted_proxy_cidrs);
        if state.ban_list.is_banned(&ip.to_string()) {
            tracing::info!("banned ip blocked from file serving");
            return JuicehostError::Forbidden.into_response();
        }
    }
    next.run(req).await
}

/// Authenticate juiceback requests with the API key and optional origin whitelist.
/// When no API key is configured, internal authentication is disabled.
async fn require_api_key(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    if state.api_key.is_empty() {
        return next.run(req).await;
    }

    if req.headers().contains_key("x-juicehost-file-capability") {
        return next.run(req).await;
    }

    let provided_key = req
        .headers()
        .get("x-juicehost-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !constant_time_eq(&state.api_key, provided_key) {
        tracing::warn!("auth: invalid api key attempt from [redacted]");
        return JuicehostError::Forbidden.into_response();
    }

    if !state.allowed_origins.is_empty() {
        let origin = req
            .headers()
            .get("x-juiceback-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !state.allowed_origins.iter().any(|a| a == origin) {
            tracing::warn!(
                "auth: rejected origin '{}' from [redacted] (allowed: {:?})",
                origin,
                state.allowed_origins,
            );
            return JuicehostError::Forbidden.into_response();
        }
    }

    next.run(req).await
}

/// Middleware that authenticates internal requests with EITHER an API key OR a ticket JWT.
async fn require_api_key_or_ticket(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    if !state.api_key.is_empty() {
        if let Some(provided_key) = req
            .headers()
            .get("x-juicehost-api-key")
            .and_then(|v| v.to_str().ok())
        {
            if constant_time_eq(&state.api_key, provided_key) {
                return next.run(req).await;
            }
        }
    }

    // Try ticket JWT (new flow from juicebox-plus)
    if let Some(token) = juiceutils::extract_bearer_token(req.headers()) {
        if verify_ticket_jwt(token, &state.ticket_jwt_secret).is_ok() {
            return next.run(req).await;
        }
    }

    tracing::warn!("auth: rejected request (no valid API key or ticket JWT)");
    JuicehostError::Forbidden.into_response()
}

async fn request_body_deadline(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: middleware::Next,
) -> Response {
    let (parts, body) = req.into_parts();
    let body = handlers::deadline_body(body, state.tcp_body_inactivity, state.tcp_request_total);
    next.run(Request::from_parts(parts, body)).await
}

/// Serves the OpenAPI spec as JSON with the correct content type.
async fn openapi_json_handler() -> (axum::http::header::HeaderMap, String) {
    let json = serde_json::to_string_pretty(&ApiDoc::openapi()).unwrap_or_default();
    let mut headers = axum::http::header::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    (headers, json)
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let concat = Router::new()
        .route("/internal/file/concat", post(handlers::concat_files))
        .layer(DefaultBodyLimit::max(64 * 1024));

    let internal = Router::new()
        .route("/internal/file", post(handlers::store_file))
        .route(
            "/internal/file/stream/:id/:filename",
            post(handlers::store_file_streaming),
        )
        .route("/internal/file/:id", delete(handlers::delete_file))
        .route("/internal/file/:id/rename", post(handlers::rename_file))
        .route("/internal/file/:id/stat", get(handlers::stat_file))
        .merge(concat)
        .layer(DefaultBodyLimit::max(state.max_file_size_bytes as usize))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            require_api_key,
        ));

    let ticket_upload = Router::new()
        .route(
            "/internal/file/upload/:id",
            post(handlers::store_file_ticket),
        )
        .layer(DefaultBodyLimit::max(state.max_file_size_bytes as usize))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            require_api_key_or_ticket,
        ));

    let files_router = Router::new()
        .route("/f/*path", get(handlers::serve_file_wildcard))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            ban_check_middleware,
        ));

    let public = Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/storage", get(handlers::storage_handler))
        .route("/api/config", get(handlers::config_handler))
        .route("/api/openapi.json", get(openapi_json_handler))
        .layer(TimeoutLayer::new(Duration::from_secs(300)));

    Router::new()
        .route("/", get(handlers::index_handler))
        .merge(public)
        .merge(files_router)
        .merge(internal)
        .merge(ticket_upload)
        .fallback(fallback_404)
        .layer(TimeoutLayer::new(state.tcp_request_total))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            request_body_deadline,
        ))
        .layer({
            if state.allowed_origins.is_empty() {
                CorsLayer::permissive()
            } else {
                let origins: Vec<axum::http::HeaderValue> = state
                    .allowed_origins
                    .iter()
                    .filter_map(|o| o.parse().ok())
                    .collect();
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods([
                        axum::http::Method::GET,
                        axum::http::Method::POST,
                        axum::http::Method::DELETE,
                        axum::http::Method::OPTIONS,
                    ])
                    .allow_headers([
                        axum::http::header::CONTENT_TYPE,
                        axum::http::header::AUTHORIZATION,
                        axum::http::header::HeaderName::from_static("x-juicehost-api-key"),
                        axum::http::header::HeaderName::from_static("x-juiceback-origin"),
                        axum::http::header::HeaderName::from_static("x-mime-type"),
                    ])
            }
        })
        .layer(middleware::from_fn(add_security_headers))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
                tracing::info_span!(
                    "http.request",
                    method = %request.method(),
                    path = request.uri().path(),
                    version = ?request.version(),
                )
            }),
        )
        .layer(NewSentryLayer::<Request<Body>>::new_from_top())
        .with_state(state)
}

/// Start the TCP server and avoids fucking up when Ctrl + C is pressed.
pub async fn start_server(app: Router, addr: SocketAddr, max_concurrent_requests: usize) {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind server");

    tracing::info!("juicehost listening on {}", addr);

    axum::serve(
        listener,
        app.layer(tower::limit::ConcurrencyLimitLayer::new(
            max_concurrent_requests,
        ))
        .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal("juicehost"))
    .tcp_nodelay(true)
    .await
    .expect("Server error");
}

pub fn print_startup_banner(config: &Config) {
    println!("{}", BANNER_ART);
    tracing::info!("juicehost v{} starting", env!("CARGO_PKG_VERSION"));
    if config.api_key.is_empty() {
        tracing::warn!(
            "JUICEHOST_API_KEY is not set: internal API authentication is disabled; use per-file capabilities for destructive operations"
        );
    }
    if config.is_s3_mode() {
        tracing::info!(
            "storage: S3 (bucket={})",
            config.s3_bucket.as_deref().unwrap_or("?")
        );
    } else {
        tracing::info!("files: {:?}", config.files_dir);
    }
    if let Some(ref backend) = config.backend_url {
        tracing::info!("backend: {}", backend);
    } else {
        tracing::info!("backend: none (backendless mode)");
    }
    let min_gb = config.min_free_space_bytes / (1024 * 1024 * 1024);
    tracing::info!("min free space: {} GB", min_gb);
    if !config.allowed_origins.is_empty() {
        tracing::info!("allowed origins: {:?}", config.allowed_origins);
    }
}
