//! Shared state used by request handlers.

use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::storage::StorageBackend;

/// Shared state injected into every axum handler.
///
/// Created once at startup and shared (via `Arc`) across all request handlers.
#[derive(Clone)]
pub struct AppState {
    /// Pluggable storage backend (local disk or S3-compatible).
    pub storage: Arc<dyn StorageBackend>,
    /// Shared secret for authenticating internal API calls from juiceback.
    pub api_key: String,
    /// Allowed values for the `X-Juiceback-Origin` header. Empty means allow all.
    pub allowed_origins: Vec<String>,
    /// Minimum free disk space (bytes) before rejecting writes.
    pub min_free_space_bytes: u64,
    /// Maximum allowed file size in bytes.
    pub max_file_size_bytes: u64,
    /// juiceback URL (used to check upload status on storage miss).
    pub backend_url: Option<String>,
    /// juicefront URL that the index page redirects to (empty disables the redirect).
    pub frontend_url: Option<String>,
    /// File type danger level: none, low, medium, high.
    pub danger_level: juiceutils::file_validation::ProtectionLevel,
    /// Default TTL in hours for file retention.
    pub default_ttl_hours: f64,
    /// Exact list of allowed TTL values in hours.
    pub allowed_ttl_hours: Vec<f64>,
    /// Quick Link two-phase uploads enabled.
    pub quick_link: bool,
    /// Custom file ID slugs enabled.
    pub custom_id: bool,
    /// JWT signing secret for validating upload tickets.
    pub ticket_jwt_secret: String,
    /// Optional IP ban list shared with the middleware.
    pub ban_list: Arc<juiceutils::ban::BanList>,
    /// Path to the optional local ban list JSON file.
    pub ban_list_file: Option<std::path::PathBuf>,
    /// Optional juiceback URL to sync the ban list from.
    pub ban_sync_url: Option<String>,
    /// Seconds between ban list file reloads / backend syncs.
    pub ban_sync_interval: u64,
    pub trusted_proxy_cidrs: Vec<juiceutils::proxy::IpCidr>,
    pub max_range_response_bytes: u64,
    pub max_concat_parts: usize,
    pub tcp_body_inactivity: std::time::Duration,
    pub tcp_request_total: std::time::Duration,
    pub upload_semaphore: Arc<tokio::sync::Semaphore>,
    pub download_semaphore: Arc<tokio::sync::Semaphore>,
    /// Reused client for bounded juiceback status, alias, and health probes.
    pub backend_client: reqwest::Client,
}

impl AppState {
    /// Create a new `AppState` from a loaded `Config` and the storage backend.
    pub fn new(config: &Config, storage: Arc<dyn StorageBackend>) -> Self {
        Self {
            storage,
            api_key: config.api_key.clone(),
            allowed_origins: config.allowed_origins.clone(),
            min_free_space_bytes: config.min_free_space_bytes,
            max_file_size_bytes: config.max_file_size_bytes,
            backend_url: config.backend_url.clone(),
            frontend_url: config.frontend_url.clone(),
            danger_level: config.danger_level,
            default_ttl_hours: config.default_ttl_hours,
            allowed_ttl_hours: config.allowed_ttl_hours.clone(),
            quick_link: config.quick_link,
            custom_id: config.custom_id,
            ticket_jwt_secret: config.ticket_jwt_secret.clone(),
            ban_list: Arc::new(juiceutils::ban::BanList::new(config.ip_pepper.clone())),
            ban_list_file: config.ban_list_file.clone(),
            ban_sync_url: config.ban_sync_url.clone(),
            ban_sync_interval: config.ban_sync_interval,
            trusted_proxy_cidrs: config.trusted_proxy_cidrs.clone(),
            max_range_response_bytes: config.max_range_response_bytes,
            max_concat_parts: config.max_concat_parts,
            tcp_body_inactivity: Duration::from_secs(config.tcp_body_inactivity_seconds),
            tcp_request_total: Duration::from_secs(config.tcp_request_total_seconds),
            upload_semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent_uploads)),
            download_semaphore: Arc::new(tokio::sync::Semaphore::new(
                config.max_concurrent_downloads,
            )),
            backend_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("fixed backend HTTP client configuration must be valid"),
        }
    }
}
