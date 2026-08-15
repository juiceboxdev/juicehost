//! Configuration loaded from environment variables.

use std::path::PathBuf;

use juiceutils::file_validation::ProtectionLevel;
use juiceutils::proxy::parse_trusted_proxy_cidrs;
use thiserror::Error;

/// The port environment variable that failed to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPort {
    Public,
    Quic,
}

impl ConfigPort {
    pub fn env_var(self) -> &'static str {
        match self {
            ConfigPort::Public => "PUBLIC_PORT",
            ConfigPort::Quic => "QUIC_PORT",
        }
    }
}

impl std::fmt::Display for ConfigPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.env_var())
    }
}

/// Errors that could happen while loading environment configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{name} must be a number")]
    InvalidNumber { name: &'static str },
    #[error("{name} must be between {min} and {max}")]
    OutOfRange {
        name: &'static str,
        min: String,
        max: String,
    },
    #[error("{port} must be a valid port")]
    InvalidPort { port: ConfigPort },
    #[error("QUIC_PORT must be set when PUBLIC_PORT is 65535")]
    MissingQuicPort,
    #[error("{name} must be true, false, 1, or 0")]
    InvalidBoolean { name: &'static str },
    #[error("ALLOWED_TTL_HOURS contains an invalid number: {0}")]
    InvalidAllowedTtl(#[source] std::num::ParseFloatError),
    #[error("DEFAULT_TTL_HOURS must be a number: {0}")]
    InvalidDefaultTtl(#[from] std::num::ParseFloatError),
    #[error("TTL values must be finite, positive, and include DEFAULT_TTL_HOURS")]
    InvalidTtlConfiguration,
    #[error("{0}")]
    InvalidTrustedProxyCidrs(String),
}

impl ConfigError {
    pub fn invalid_quic_port() -> Self {
        ConfigError::InvalidPort {
            port: ConfigPort::Quic,
        }
    }
}

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
    pub danger_level: ProtectionLevel, // <- (none, low, medium, high)
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

fn bounded_env<T>(name: &'static str, default: T, min: T, max: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr + PartialOrd + Copy + std::fmt::Display,
{
    let value = match std::env::var(name) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|_| ConfigError::InvalidNumber { name })?,
        Err(_) => default,
    };
    if value < min || value > max {
        return Err(ConfigError::OutOfRange {
            name,
            min: min.to_string(),
            max: max.to_string(),
        });
    }
    Ok(value)
}

// We say thank you to orng.
fn env_bool(name: &'static str, default: bool) -> Result<bool, ConfigError> {
    match std::env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(ConfigError::InvalidBoolean { name }),
        },
        Err(_) => Ok(default),
    }
}

/// Public HTTP listener settings.
#[derive(Debug)]
struct PublicSettings {
    host: String,
    port: u16,
}

impl PublicSettings {
    fn from_env() -> Result<Self, ConfigError> {
        let host = std::env::var("PUBLIC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = std::env::var("PUBLIC_PORT")
            .ok()
            .map(|p| {
                p.parse::<u16>().map_err(|_| ConfigError::InvalidPort {
                    port: ConfigPort::Public,
                })
            })
            .transpose()?
            .unwrap_or(6402);
        Ok(Self { host, port })
    }
}

/// QUIC/HTTP/3 listener and limit settings.
#[derive(Debug)]
struct QuicSettings {
    host: String,
    port: u16,
    cert_path: PathBuf,
    max_connections: usize,
    max_requests: usize,
    handshake_seconds: u64,
    idle_seconds: u64,
    request_total_seconds: u64,
}

impl QuicSettings {
    fn from_env(public: &PublicSettings) -> Result<Self, ConfigError> {
        let host = std::env::var("QUIC_HOST").unwrap_or_else(|_| public.host.clone());
        let port = std::env::var("QUIC_PORT")
            .ok()
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| ConfigError::invalid_quic_port())
            })
            .transpose()?
            .unwrap_or(
                public
                    .port
                    .checked_add(1)
                    .ok_or(ConfigError::MissingQuicPort)?,
            );
        let cert_path = std::env::var("QUIC_CERT_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./quic-cert.der"));
        let max_connections = bounded_env("QUIC_MAX_CONNECTIONS", 256usize, 1, 65_536)?;
        let max_requests = bounded_env("QUIC_MAX_REQUESTS", 256usize, 1, 65_536)?;
        let handshake_seconds = bounded_env("QUIC_HANDSHAKE_SECONDS", 10u64, 1, 120)?;
        let idle_seconds = bounded_env("QUIC_IDLE_SECONDS", 30u64, 1, 600)?;
        let request_total_seconds = bounded_env("QUIC_REQUEST_TOTAL_SECONDS", 600u64, 1, 86_400)?;
        Ok(Self {
            host,
            port,
            cert_path,
            max_connections,
            max_requests,
            handshake_seconds,
            idle_seconds,
            request_total_seconds,
        })
    }
}

/// Tokio worker thread settings.
#[derive(Debug)]
struct ThreadSettings {
    worker_threads: usize,
}

impl ThreadSettings {
    fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            worker_threads: bounded_env("WORKER_THREADS", 3usize, 1, 256)?,
        })
    }
}

/// Local directory and peer URL settings.
#[derive(Debug)]
struct DirectorySettings {
    files_dir: PathBuf,
    backend_url: Option<String>,
    frontend_url: Option<String>,
}

impl DirectorySettings {
    fn from_env() -> Self {
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
        Self {
            files_dir,
            backend_url,
            frontend_url,
        }
    }
}

/// Internal API authentication, origin, and validation settings.
#[derive(Debug)]
struct SecuritySettings {
    api_key: String,
    allowed_origins: Vec<String>,
    danger_level: ProtectionLevel,
    trusted_proxy_cidrs: Vec<juiceutils::proxy::IpCidr>,
}

impl SecuritySettings {
    fn from_env(directories: &DirectorySettings) -> Result<Self, ConfigError> {
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
                directories
                    .backend_url
                    .as_ref()
                    .map(|b| vec![b.clone()])
                    .unwrap_or_default()
            });
        let danger_level = ProtectionLevel::parse(
            &std::env::var("DANGER_LEVEL").unwrap_or_else(|_| "high".to_string()),
        );
        let trusted_proxy_cidrs =
            parse_trusted_proxy_cidrs(&std::env::var("TRUSTED_PROXY_CIDRS").unwrap_or_default())
                .map_err(ConfigError::InvalidTrustedProxyCidrs)?;
        Ok(Self {
            api_key,
            allowed_origins,
            danger_level,
            trusted_proxy_cidrs,
        })
    }
}

/// S3-compatible backend settings.
#[derive(Debug)]
struct S3Settings {
    bucket: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
    allow_http: bool,
    access_key: Option<String>,
    secret_key: Option<String>,
}

impl S3Settings {
    fn from_env() -> Result<Self, ConfigError> {
        let bucket = std::env::var("S3_BUCKET")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let region = std::env::var("S3_REGION")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let endpoint = std::env::var("S3_ENDPOINT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let allow_http = env_bool("S3_ALLOW_HTTP", false)?;
        let access_key = std::env::var("S3_ACCESS_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let secret_key = std::env::var("S3_SECRET_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Ok(Self {
            bucket,
            region,
            endpoint,
            allow_http,
            access_key,
            secret_key,
        })
    }
}

/// Concurrency and timeout limit settings.
#[derive(Debug)]
struct LimitsSettings {
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
// I KNOW THERE'S A BETTER WAY TO DO THIS DON'T BLAME ME FOR THIS.
impl LimitsSettings {
    fn from_env() -> Result<Self, ConfigError> {
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

/// Feature toggle and TTL settings.
#[derive(Debug)]
struct FeatureSettings {
    quick_link: bool,
    custom_id: bool,
    default_ttl_hours: f64,
    allowed_ttl_hours: Vec<f64>,
}

impl FeatureSettings {
    fn from_env() -> Result<Self, ConfigError> {
        let quick_link = env_bool("QUICK_LINK", true)?;
        let custom_id = env_bool("CUSTOM_ID", true)?;
        let allowed_ttl_hours: Vec<f64> = std::env::var("ALLOWED_TTL_HOURS")
            .unwrap_or_else(|_| "0.5,1,6,12,24,72,168".into())
            .split(',')
            .map(|s| {
                s.trim()
                    .parse::<f64>()
                    .map_err(ConfigError::InvalidAllowedTtl)
            })
            .collect::<Result<_, _>>()?;
        let default_ttl_hours = std::env::var("DEFAULT_TTL_HOURS")
            .ok()
            .map(|v| v.parse::<f64>())
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
            return Err(ConfigError::InvalidTtlConfiguration);
        }

        Ok(Self {
            quick_link,
            custom_id,
            default_ttl_hours,
            allowed_ttl_hours,
        })
    }
}

/// Signing secrets and ban pepper settings.
#[derive(Debug)]
struct SecretSettings {
    ticket_jwt_secret: String,
    ip_pepper: String,
}

impl SecretSettings {
    fn from_env(security: &SecuritySettings) -> Self {
        let ticket_jwt_secret = std::env::var("TICKET_JWT_SECRET").unwrap_or_else(|_| {
            std::env::var("JWT_SECRET").unwrap_or_else(|_| security.api_key.clone())
        });
        let ip_pepper = std::env::var("IP_PEPPER").unwrap_or_default();
        Self {
            ticket_jwt_secret,
            ip_pepper,
        }
    }
}

/// IP ban list and backend sync settings.
#[derive(Debug)]
struct BanSettings {
    list_file: Option<PathBuf>,
    sync_url: Option<String>,
    sync_interval: u64,
}

impl BanSettings {
    fn from_env() -> Self {
        let list_file = std::env::var("BAN_LIST_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        let sync_url = std::env::var("BAN_SYNC_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim_end_matches('/').to_string());
        let sync_interval = std::env::var("BAN_SYNC_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);
        Self {
            list_file,
            sync_url,
            sync_interval,
        }
    }
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self, ConfigError> {
        let public = PublicSettings::from_env()?;
        let quic = QuicSettings::from_env(&public)?;
        let threads = ThreadSettings::from_env()?;
        let directories = DirectorySettings::from_env();
        let security = SecuritySettings::from_env(&directories)?;
        let s3 = S3Settings::from_env()?;
        let limits = LimitsSettings::from_env()?;
        let features = FeatureSettings::from_env()?;
        let secrets = SecretSettings::from_env(&security);
        let ban = BanSettings::from_env();

        Ok(Self {
            public_host: public.host,
            public_port: public.port,
            quic_host: quic.host,
            quic_port: quic.port,
            quic_cert_path: Some(quic.cert_path),
            quic_max_connections: quic.max_connections,
            quic_max_requests: quic.max_requests,
            quic_handshake_seconds: quic.handshake_seconds,
            quic_idle_seconds: quic.idle_seconds,
            quic_request_total_seconds: quic.request_total_seconds,
            worker_threads: threads.worker_threads,
            files_dir: directories.files_dir,
            backend_url: directories.backend_url,
            frontend_url: directories.frontend_url,
            api_key: security.api_key,
            allowed_origins: security.allowed_origins,
            danger_level: security.danger_level,
            trusted_proxy_cidrs: security.trusted_proxy_cidrs,
            s3_bucket: s3.bucket,
            s3_region: s3.region,
            s3_endpoint: s3.endpoint,
            s3_allow_http: s3.allow_http,
            s3_access_key: s3.access_key,
            s3_secret_key: s3.secret_key,
            min_free_space_bytes: limits.min_free_space_bytes,
            max_file_size_bytes: limits.max_file_size_bytes,
            max_range_response_bytes: limits.max_range_response_bytes,
            max_concurrent_uploads: limits.max_concurrent_uploads,
            max_concurrent_downloads: limits.max_concurrent_downloads,
            max_concat_parts: limits.max_concat_parts,
            tcp_body_inactivity_seconds: limits.tcp_body_inactivity_seconds,
            tcp_request_total_seconds: limits.tcp_request_total_seconds,
            tcp_max_concurrent_requests: limits.tcp_max_concurrent_requests,
            quick_link: features.quick_link,
            custom_id: features.custom_id,
            default_ttl_hours: features.default_ttl_hours,
            allowed_ttl_hours: features.allowed_ttl_hours,
            ticket_jwt_secret: secrets.ticket_jwt_secret,
            ip_pepper: secrets.ip_pepper,
            ban_list_file: ban.list_file,
            ban_sync_url: ban.sync_url,
            ban_sync_interval: ban.sync_interval,
        })
    }
    pub fn is_s3_mode(&self) -> bool {
        self.s3_bucket.is_some()
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
