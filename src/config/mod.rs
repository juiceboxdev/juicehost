//! Configuration loaded from environment variables.

use std::path::PathBuf;

use juiceutils::file_validation::ProtectionLevel;

mod ban;
mod directory;
mod error;
mod feature;
mod limits;
mod port;
mod public;
mod quic;
mod s3;
mod secret;
mod security;
mod thread;

use ban::BanSettings;
use directory::DirectorySettings;
use feature::FeatureSettings;
use limits::LimitsSettings;
use port::ConfigPort;
use public::PublicSettings;
use quic::QuicSettings;
use s3::S3Settings;
use secret::SecretSettings;
use security::SecuritySettings;
use thread::ThreadSettings;

pub use error::ConfigError;

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
            public_host: public.host().to_owned(),
            public_port: public.port(),
            quic_host: quic.host().to_owned(),
            quic_port: quic.port(),
            quic_cert_path: Some(quic.cert_path().to_owned()),
            quic_max_connections: quic.max_connections(),
            quic_max_requests: quic.max_requests(),
            quic_handshake_seconds: quic.handshake_seconds(),
            quic_idle_seconds: quic.idle_seconds(),
            quic_request_total_seconds: quic.request_total_seconds(),
            worker_threads: threads.worker_threads(),
            files_dir: directories.files_dir().to_owned(),
            backend_url: directories.backend_url().cloned(),
            frontend_url: directories.frontend_url().cloned(),
            api_key: security.api_key().to_owned(),
            allowed_origins: security.allowed_origins().to_owned(),
            danger_level: security.danger_level(),
            trusted_proxy_cidrs: security.trusted_proxy_cidrs().to_owned(),
            s3_bucket: s3.bucket().cloned(),
            s3_region: s3.region().cloned(),
            s3_endpoint: s3.endpoint().cloned(),
            s3_allow_http: s3.allow_http(),
            s3_access_key: s3.access_key().cloned(),
            s3_secret_key: s3.secret_key().cloned(),
            min_free_space_bytes: limits.min_free_space_bytes(),
            max_file_size_bytes: limits.max_file_size_bytes(),
            max_range_response_bytes: limits.max_range_response_bytes(),
            max_concurrent_uploads: limits.max_concurrent_uploads(),
            max_concurrent_downloads: limits.max_concurrent_downloads(),
            max_concat_parts: limits.max_concat_parts(),
            tcp_body_inactivity_seconds: limits.tcp_body_inactivity_seconds(),
            tcp_request_total_seconds: limits.tcp_request_total_seconds(),
            tcp_max_concurrent_requests: limits.tcp_max_concurrent_requests(),
            quick_link: features.quick_link(),
            custom_id: features.custom_id(),
            default_ttl_hours: features.default_ttl_hours(),
            allowed_ttl_hours: features.allowed_ttl_hours().to_owned(),
            ticket_jwt_secret: secrets.ticket_jwt_secret().to_owned(),
            ip_pepper: secrets.ip_pepper().to_owned(),
            ban_list_file: ban.list_file().cloned(),
            ban_sync_url: ban.sync_url().cloned(),
            ban_sync_interval: ban.sync_interval(),
        })
    }
    pub fn is_s3_mode(&self) -> bool {
        self.s3_bucket.is_some()
    }
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
    std::env::var(name).map_or(Ok(default), |value| {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(ConfigError::InvalidBoolean { name }),
        }
    })
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
