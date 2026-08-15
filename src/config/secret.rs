use crate::config::security::SecuritySettings;

/// Signing secrets and ban pepper settings.
#[derive(Debug)]
pub struct SecretSettings {
    ticket_jwt_secret: String,
    ip_pepper: String,
}

impl SecretSettings {
    pub fn ticket_jwt_secret(&self) -> &str {
        &self.ticket_jwt_secret
    }

    pub fn ip_pepper(&self) -> &str {
        &self.ip_pepper
    }

    pub fn from_env(security: &SecuritySettings) -> Self {
        let ticket_jwt_secret = std::env::var("TICKET_JWT_SECRET").unwrap_or_else(|_| {
            std::env::var("JWT_SECRET").unwrap_or_else(|_| security.api_key().to_owned())
        });
        let ip_pepper = std::env::var("IP_PEPPER").unwrap_or_default();
        Self {
            ticket_jwt_secret,
            ip_pepper,
        }
    }
}
