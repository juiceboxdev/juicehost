use std::path::PathBuf;

use crate::config::{bounded_env, public::PublicSettings, ConfigError};

/// QUIC/HTTP/3 listener and limit settings.
#[derive(Debug)]
pub struct QuicSettings {
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
    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub const fn cert_path(&self) -> &PathBuf {
        &self.cert_path
    }

    pub const fn max_connections(&self) -> usize {
        self.max_connections
    }

    pub const fn max_requests(&self) -> usize {
        self.max_requests
    }

    pub const fn handshake_seconds(&self) -> u64 {
        self.handshake_seconds
    }

    pub const fn idle_seconds(&self) -> u64 {
        self.idle_seconds
    }

    pub const fn request_total_seconds(&self) -> u64 {
        self.request_total_seconds
    }

    pub fn from_env(public: &PublicSettings) -> Result<Self, ConfigError> {
        let host = std::env::var("QUIC_HOST").unwrap_or_else(|_| public.host().to_owned());
        let port = std::env::var("QUIC_PORT")
            .ok()
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| ConfigError::invalid_quic_port())
            })
            .transpose()?
            .unwrap_or(
                public
                    .port()
                    .checked_add(1)
                    .ok_or(ConfigError::MissingQuicPort)?,
            );
        let cert_path = std::env::var("QUIC_CERT_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| PathBuf::from("./quic-cert.der"), PathBuf::from);
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
