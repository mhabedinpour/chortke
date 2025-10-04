use std::path::Path;

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use tracing::level_filters::LevelFilter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// Interface to bind the API server to (e.g., "127.0.0.1").
    pub host: String,
    /// TCP port for the API server (e.g., 8080).
    pub port: u16,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogFormat {
    COMPACT,
    JSON,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LogLevel {
    TRACE,
    DEBUG,
    INFO,
    WARN,
    ERROR,
}

impl From<LogLevel> for LevelFilter {
    fn from(val: LogLevel) -> Self {
        match val {
            LogLevel::TRACE => LevelFilter::TRACE,
            LogLevel::DEBUG => LevelFilter::DEBUG,
            LogLevel::INFO => LevelFilter::INFO,
            LogLevel::WARN => LevelFilter::WARN,
            LogLevel::ERROR => LevelFilter::ERROR,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    pub level: LogLevel,
    pub format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::TRACE,
            format: LogFormat::COMPACT,
        }
    }
}

/// Top-level application configuration wrapper.
///
/// This struct groups all configuration sections used by the application.
/// Loaded with the following precedence (lowest to highest):
/// 1) Built-in defaults
/// 2) Optional config file (if present)
/// 3) Environment variables
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub api: ApiConfig,
    pub logger: LogConfig,
}

impl AppConfig {
    pub fn load(config_path: &Path) -> Result<Self, figment::Error> {
        let mut figment = Figment::from(Serialized::defaults(AppConfig::default()));

        if config_path.exists() {
            figment = figment.merge(Toml::file(config_path));
        }
        figment = figment.merge(Env::prefixed("CHORTKE_").split("_"));

        let cfg = figment.extract()?;
        Ok(cfg)
    }
}
