use std::{env, str::FromStr};

use crate::config::ConfigError;

/// Public HTTP listener settings.
#[derive(Debug)]
pub struct PublicSettings {
    host: String,
    port: u16,
}

impl PublicSettings {
    pub const DEFAULT_IP: &str = "127.0.0.1";
    pub const DEFAULT_PORT: u16 = 6402;

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let host = env::var("PUBLIC_HOST").unwrap_or_else(|_| Self::DEFAULT_IP.to_owned());
        let port = env::var("PUBLIC_PORT").map_or(Ok(Self::DEFAULT_PORT), |p| {
            u16::from_str(&p).map_err(|_| ConfigError::invalid_public_port())
        })?;

        Ok(Self { host, port })
    }
}
