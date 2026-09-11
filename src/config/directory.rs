use std::path::PathBuf;

/// Local directory and peer URL settings.
#[derive(Debug)]
pub struct DirectorySettings {
    files_dir: PathBuf,
    backend_url: Option<String>,
    frontend_url: Option<String>,
}

impl DirectorySettings {
    pub const fn files_dir(&self) -> &PathBuf {
        &self.files_dir
    }

    pub const fn backend_url(&self) -> Option<&String> {
        self.backend_url.as_ref()
    }

    pub const fn frontend_url(&self) -> Option<&String> {
        self.frontend_url.as_ref()
    }

    pub fn from_env() -> Self {
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
