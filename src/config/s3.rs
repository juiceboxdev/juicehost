use crate::config::{env_bool, ConfigError};

/// S3-compatible backend settings.
#[derive(Debug)]
pub struct S3Settings {
    bucket: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
    allow_http: bool,
    access_key: Option<String>,
    secret_key: Option<String>,
}

impl S3Settings {
    pub const fn bucket(&self) -> Option<&String> {
        self.bucket.as_ref()
    }

    pub const fn region(&self) -> Option<&String> {
        self.region.as_ref()
    }

    pub const fn endpoint(&self) -> Option<&String> {
        self.endpoint.as_ref()
    }

    pub const fn allow_http(&self) -> bool {
        self.allow_http
    }

    pub const fn access_key(&self) -> Option<&String> {
        self.access_key.as_ref()
    }

    pub const fn secret_key(&self) -> Option<&String> {
        self.secret_key.as_ref()
    }
}

impl S3Settings {
    pub fn from_env() -> Result<Self, ConfigError> {
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
