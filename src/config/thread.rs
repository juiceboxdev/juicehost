use crate::config::{bounded_env, ConfigError};

/// Tokio worker thread settings.
#[derive(Debug)]
pub struct ThreadSettings {
    worker_threads: usize,
}

impl ThreadSettings {
    pub const fn worker_threads(&self) -> usize {
        self.worker_threads
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            worker_threads: bounded_env("WORKER_THREADS", 3usize, 1, 256)?,
        })
    }
}
