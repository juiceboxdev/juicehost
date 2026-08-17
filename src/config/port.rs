use std::fmt::Display;

/// The port environment variable that failed to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPort {
    Public,
    Quic,
}

impl Display for ConfigPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Public => "PUBLIC_PORT",
            Self::Quic => "QUIC_PORT",
        })
    }
}
