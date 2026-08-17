use crate::config::{env_bool, ConfigError};

/// Feature toggle and TTL settings.
#[derive(Debug)]
pub struct FeatureSettings {
    quick_link: bool,
    custom_id: bool,
    default_ttl_hours: f64,
    allowed_ttl_hours: Vec<f64>,
}

impl FeatureSettings {
    pub const fn quick_link(&self) -> bool {
        self.quick_link
    }

    pub const fn custom_id(&self) -> bool {
        self.custom_id
    }

    pub const fn default_ttl_hours(&self) -> f64 {
        self.default_ttl_hours
    }

    pub fn allowed_ttl_hours(&self) -> &[f64] {
        &self.allowed_ttl_hours
    }

    pub fn from_env() -> Result<Self, ConfigError> {
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
