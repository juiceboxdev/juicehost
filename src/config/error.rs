use crate::config::ConfigPort;

/// Errors that could happen while loading environment configuration.
#[derive(Debug, thiserror::Error)]
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
    #[must_use]
    pub const fn invalid_quic_port() -> Self {
        Self::InvalidPort {
            port: ConfigPort::Quic,
        }
    }

    #[must_use]
    pub const fn invalid_public_port() -> Self {
        Self::InvalidPort {
            port: ConfigPort::Public,
        }
    }
}
