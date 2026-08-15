//! Configuration loaded from environment variables.

use std::path::PathBuf;

/// Holds every setting juicehost needs to run.
// If any missing in .env.example, tell me.
#[derive(Debug, Clone)]
pub struct Config {
    /// Public settings
    pub public_host: String,
    pub public_port: u16,
    /// QUIC settings
    pub quic_host: String,
    pub quic_port: u16,
    pub quic_cert_path: Option<std::path::PathBuf>,
    pub quic_max_connections: usize,
    pub quic_max_requests: usize,
    pub quic_handshake_seconds: u64,
    pub quic_idle_seconds: u64,
    pub quic_request_total_seconds: u64,
    /// Thread Settings
    pub worker_threads: usize,
    /// Directory Settings
    pub files_dir: PathBuf,
    pub backend_url: Option<String>,
    // Frontend Settings
    pub frontend_url: Option<String>, // juicefront URL that GET / redirects to
    /// Security Settings
    pub api_key: String, // Optional!
    pub allowed_origins: Vec<String>, // Optional!
    pub danger_level: juiceutils::file_validation::ProtectionLevel, // <- (none, low, medium, high)
    pub trusted_proxy_cidrs: Vec<juiceutils::proxy::IpCidr>, // Super important if under a rev proxy.
    /// S3 Settings
    pub s3_bucket: Option<String>,
    pub s3_region: Option<String>,
    pub s3_endpoint: Option<String>,
    pub s3_allow_http: bool,
    pub s3_access_key: Option<String>,
    pub s3_secret_key: Option<String>,
    /// Limits Settings
    pub min_free_space_bytes: u64,
    pub max_file_size_bytes: u64,
    pub max_range_response_bytes: u64,
    pub max_concurrent_uploads: usize,
    pub max_concurrent_downloads: usize,
    pub max_concat_parts: usize,
    pub tcp_body_inactivity_seconds: u64,
    pub tcp_request_total_seconds: u64,
    pub tcp_max_concurrent_requests: usize,
    /// Features Settings
    pub quick_link: bool,
    pub custom_id: bool,
    pub default_ttl_hours: f64,
    pub allowed_ttl_hours: Vec<f64>,
    /// Secrets
    pub ticket_jwt_secret: String,
    pub ip_pepper: String,
    /// Ban Settings
    pub ban_list_file: Option<PathBuf>,
    pub ban_sync_url: Option<String>,
    pub ban_sync_interval: u64,
}

fn bounded_env<T>(name: &str, default: T, min: T, max: T) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + Copy + std::fmt::Display,
{
    let value = match std::env::var(name) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|_| format!("{name} must be a number"))?,
        Err(_) => default,
    };
    if value < min || value > max {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    Ok(value)
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self, String> {
        let public_host = std::env::var("PUBLIC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let public_port = std::env::var("PUBLIC_PORT")
            .ok()
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| "PUBLIC_PORT must be a valid port")
            })
            .transpose()?
            .unwrap_or(6402);
        let quic_host = std::env::var("QUIC_HOST").unwrap_or_else(|_| public_host.clone());
        let quic_port = std::env::var("QUIC_PORT")
            .ok()
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| "QUIC_PORT must be a valid port")
            })
            .transpose()?
            .unwrap_or(
                public_port
                    .checked_add(1)
                    .ok_or("QUIC_PORT must be set when PUBLIC_PORT is 65535")?,
            );
        let worker_threads = bounded_env("WORKER_THREADS", 3usize, 1, 256)?;
        let files_dir = std::env::var("FILES_DIR")
            .unwrap_or_else(|_| "./files".to_string())
            .into();
        let backend_url = std::env::var("BACKEND_URL")
            .ok()
            .filter(|s| !s.trim().is_empty() && s.trim() != "none")
            .map(|s| s.trim_end_matches('/').to_string());

        let frontend_url = std::env::var("FRONTEND_URL")
            .ok()
            .filter(|s| !s.trim().is_empty() && s.trim() != "none")
            .map(|s| s.trim_end_matches('/').to_string());
        let api_key = std::env::var("JUICEHOST_API_KEY")
            .unwrap_or_default()
            .trim()
            .to_string();
        let allowed_origins = std::env::var("ALLOWED_ORIGINS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| {
                backend_url
                    .as_ref()
                    .map(|b| vec![b.clone()])
                    .unwrap_or_default()
            });

        let min_free_space_gb = std::env::var("MIN_FREE_SPACE_GB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5);
        let min_free_space_bytes = min_free_space_gb * 1024 * 1024 * 1024;

        let s3_bucket = std::env::var("S3_BUCKET")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let s3_region = std::env::var("S3_REGION")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let s3_endpoint = std::env::var("S3_ENDPOINT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let s3_allow_http = env_bool("S3_ALLOW_HTTP", false)?;
        let s3_access_key = std::env::var("S3_ACCESS_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let s3_secret_key = std::env::var("S3_SECRET_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let max_file_size_mb = bounded_env("MAX_FILE_SIZE_MB", 500u64, 1, 1024 * 1024)?;
        let max_file_size_bytes = max_file_size_mb * 1024 * 1024;

        let quick_link = env_bool("QUICK_LINK", true)?;
        let custom_id = env_bool("CUSTOM_ID", true)?;

        let danger_level = juiceutils::file_validation::ProtectionLevel::parse(
            &std::env::var("DANGER_LEVEL").unwrap_or_else(|_| "high".to_string()),
        );

        let quic_cert_path = Some(
            std::env::var("QUIC_CERT_PATH")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("./quic-cert.der")),
        );

        let allowed_ttl_hours: Vec<f64> = std::env::var("ALLOWED_TTL_HOURS")
            .unwrap_or_else(|_| "0.5,1,6,12,24,72,168".into())
            .split(',')
            .map(|s| {
                s.trim()
                    .parse::<f64>()
                    .map_err(|_| "ALLOWED_TTL_HOURS contains an invalid number")
            })
            .collect::<Result<_, _>>()?;

        let default_ttl_hours = std::env::var("DEFAULT_TTL_HOURS")
            .ok()
            .map(|v| {
                v.parse::<f64>()
                    .map_err(|_| "DEFAULT_TTL_HOURS must be a number")
            })
            .transpose()?
            .unwrap_or(24.0);

        if allowed_ttl_hours.is_empty()
            || allowed_ttl_hours
                .iter()
                .any(|ttl| !ttl.is_finite() || *ttl <= 0.0)
            || !default_ttl_hours.is_finite()
            || default_ttl_hours <= 0.0
            || !allowed_ttl_hours.contains(&default_ttl_hours)
        {
            return Err(
                "TTL values must be finite, positive, and include DEFAULT_TTL_HOURS".into(),
            );
        }

        let ticket_jwt_secret = std::env::var("TICKET_JWT_SECRET")
            .unwrap_or_else(|_| std::env::var("JWT_SECRET").unwrap_or_else(|_| api_key.clone()));

        let ip_pepper = std::env::var("IP_PEPPER").unwrap_or_default();

        let ban_list_file = std::env::var("BAN_LIST_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);

        let ban_sync_url = std::env::var("BAN_SYNC_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim_end_matches('/').to_string());

        let ban_sync_interval = std::env::var("BAN_SYNC_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);

        let trusted_proxy_cidrs = juiceutils::proxy::parse_trusted_proxy_cidrs(
            &std::env::var("TRUSTED_PROXY_CIDRS").unwrap_or_default(),
        )?;

        let max_range_response_mb = bounded_env("MAX_RANGE_RESPONSE_MB", 16u64, 1, 1024)?;
        let max_range_response_bytes = max_range_response_mb * 1024 * 1024;
        let max_concurrent_uploads = bounded_env("MAX_CONCURRENT_UPLOADS", 16usize, 1, 4096)?;
        let max_concurrent_downloads = bounded_env("MAX_CONCURRENT_DOWNLOADS", 64usize, 1, 4096)?;
        let max_concat_parts = bounded_env("MAX_CONCAT_PARTS", 128usize, 1, 4096)?;
        let tcp_body_inactivity_seconds =
            bounded_env("TCP_BODY_INACTIVITY_SECONDS", 30u64, 1, 3600)?;
        let tcp_request_total_seconds =
            bounded_env("TCP_REQUEST_TOTAL_SECONDS", 600u64, 1, 86_400)?;
        let tcp_max_concurrent_requests =
            bounded_env("TCP_MAX_CONCURRENT_REQUESTS", 512usize, 1, 65_536)?;
        let quic_max_connections = bounded_env("QUIC_MAX_CONNECTIONS", 256usize, 1, 65_536)?;
        let quic_max_requests = bounded_env("QUIC_MAX_REQUESTS", 256usize, 1, 65_536)?;
        let quic_handshake_seconds = bounded_env("QUIC_HANDSHAKE_SECONDS", 10u64, 1, 120)?;
        let quic_idle_seconds = bounded_env("QUIC_IDLE_SECONDS", 30u64, 1, 600)?;
        let quic_request_total_seconds =
            bounded_env("QUIC_REQUEST_TOTAL_SECONDS", 600u64, 1, 86_400)?;

        Ok(Self {
            public_host,
            public_port,
            quic_host,
            quic_port,
            worker_threads,
            files_dir,
            backend_url,
            frontend_url,
            api_key,
            allowed_origins,
            min_free_space_bytes,
            s3_bucket,
            s3_region,
            s3_endpoint,
            s3_allow_http,
            s3_access_key,
            s3_secret_key,
            max_file_size_bytes,
            quick_link,
            custom_id,
            danger_level,
            quic_cert_path,
            default_ttl_hours,
            allowed_ttl_hours,
            ticket_jwt_secret,
            ip_pepper,
            ban_list_file,
            ban_sync_url,
            ban_sync_interval,
            trusted_proxy_cidrs,
            max_range_response_bytes,
            max_concurrent_uploads,
            max_concurrent_downloads,
            max_concat_parts,
            tcp_body_inactivity_seconds,
            tcp_request_total_seconds,
            tcp_max_concurrent_requests,
            quic_max_connections,
            quic_max_requests,
            quic_handshake_seconds,
            quic_idle_seconds,
            quic_request_total_seconds,
        })
    }
    pub fn is_s3_mode(&self) -> bool {
        self.s3_bucket.is_some()
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool, String> {
    match std::env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(format!("{name} must be true, false, 1, or 0")),
        },
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn missing_api_key_is_optional() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("JUICEHOST_API_KEY");
        std::env::remove_var("JUICEHOST_API_KEY");
        let result = Config::from_env();
        match previous {
            Some(value) => std::env::set_var("JUICEHOST_API_KEY", value),
            None => std::env::remove_var("JUICEHOST_API_KEY"),
        }
        assert!(result.is_ok());
        assert!(result.unwrap().api_key.is_empty());
    }

    #[test]
    fn whitespace_api_key_becomes_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("JUICEHOST_API_KEY");
        std::env::set_var("JUICEHOST_API_KEY", "   ");
        let result = Config::from_env();
        match previous {
            Some(value) => std::env::set_var("JUICEHOST_API_KEY", value),
            None => std::env::remove_var("JUICEHOST_API_KEY"),
        }
        assert!(result.is_ok());
        assert!(result.unwrap().api_key.is_empty());
    }

    #[test]
    fn bounded_env_rejects_invalid_and_out_of_range_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        const NAME: &str = "JUICEHOST_TEST_BOUNDED_ENV";
        let previous = std::env::var_os(NAME);

        std::env::set_var(NAME, "not-a-number");
        assert!(bounded_env(NAME, 5usize, 1, 10).is_err());
        std::env::set_var(NAME, "0");
        assert!(bounded_env(NAME, 5usize, 1, 10).is_err());
        std::env::set_var(NAME, "11");
        assert!(bounded_env(NAME, 5usize, 1, 10).is_err());
        std::env::set_var(NAME, "10");
        assert_eq!(bounded_env(NAME, 5usize, 1, 10).unwrap(), 10);

        match previous {
            Some(value) => std::env::set_var(NAME, value),
            None => std::env::remove_var(NAME),
        }
    }
}
