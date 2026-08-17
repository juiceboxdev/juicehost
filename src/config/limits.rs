use crate::config::{bounded_env, ConfigError};

/// Concurrency and timeout limit settings.
#[derive(Debug)]
pub struct LimitsSettings {
    min_free_space_bytes: u64,
    max_file_size_bytes: u64,
    max_range_response_bytes: u64,
    max_concurrent_uploads: usize,
    max_concurrent_downloads: usize,
    max_concat_parts: usize,
    tcp_body_inactivity_seconds: u64,
    tcp_request_total_seconds: u64,
    tcp_max_concurrent_requests: usize,
}

impl LimitsSettings {
    pub const fn min_free_space_bytes(&self) -> u64 {
        self.min_free_space_bytes
    }

    pub const fn max_file_size_bytes(&self) -> u64 {
        self.max_file_size_bytes
    }

    pub const fn max_range_response_bytes(&self) -> u64 {
        self.max_range_response_bytes
    }

    pub const fn max_concurrent_uploads(&self) -> usize {
        self.max_concurrent_uploads
    }

    pub const fn max_concurrent_downloads(&self) -> usize {
        self.max_concurrent_downloads
    }

    pub const fn max_concat_parts(&self) -> usize {
        self.max_concat_parts
    }

    pub const fn tcp_body_inactivity_seconds(&self) -> u64 {
        self.tcp_body_inactivity_seconds
    }

    pub const fn tcp_request_total_seconds(&self) -> u64 {
        self.tcp_request_total_seconds
    }

    pub const fn tcp_max_concurrent_requests(&self) -> usize {
        self.tcp_max_concurrent_requests
    }

    // I KNOW THERE'S A BETTER WAY TO DO THIS DON'T BLAME ME FOR THIS.
    pub fn from_env() -> Result<Self, ConfigError> {
        let min_free_space_bytes = std::env::var("MIN_FREE_SPACE_GB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5)
            * 1024
            * 1024
            * 1024;
        let max_file_size_bytes =
            bounded_env("MAX_FILE_SIZE_MB", 500u64, 1, 1024 * 1024)? * 1024 * 1024;
        let max_range_response_bytes =
            bounded_env("MAX_RANGE_RESPONSE_MB", 16u64, 1, 1024)? * 1024 * 1024;
        let max_concurrent_uploads = bounded_env("MAX_CONCURRENT_UPLOADS", 16usize, 1, 4096)?;
        let max_concurrent_downloads = bounded_env("MAX_CONCURRENT_DOWNLOADS", 64usize, 1, 4096)?;
        let max_concat_parts = bounded_env("MAX_CONCAT_PARTS", 128usize, 1, 4096)?;
        let tcp_body_inactivity_seconds =
            bounded_env("TCP_BODY_INACTIVITY_SECONDS", 30u64, 1, 3600)?;
        let tcp_request_total_seconds =
            bounded_env("TCP_REQUEST_TOTAL_SECONDS", 600u64, 1, 86_400)?;
        let tcp_max_concurrent_requests =
            bounded_env("TCP_MAX_CONCURRENT_REQUESTS", 512usize, 1, 65_536)?;
        Ok(Self {
            min_free_space_bytes,
            max_file_size_bytes,
            max_range_response_bytes,
            max_concurrent_uploads,
            max_concurrent_downloads,
            max_concat_parts,
            tcp_body_inactivity_seconds,
            tcp_request_total_seconds,
            tcp_max_concurrent_requests,
        })
    }
}
