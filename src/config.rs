use std::path::Path;

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
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
/// Log output format for the application logger.
///
/// - COMPACT: human-readable, single-line logs suited for local development.
/// - JSON: structured JSON logs suitable for log aggregation systems.
pub enum LogFormat {
    /// Human-readable, single-line compact format.
    COMPACT,
    /// Structured JSON format for machine processing.
    JSON,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
/// Logging verbosity level. Maps 1:1 to `tracing` LevelFilter.
///
/// Use higher levels (ERROR/WARN) in production to reduce noise, and lower
/// levels (INFO/DEBUG/TRACE) during development or troubleshooting.
pub enum LogLevel {
    /// Extremely verbose, includes fine-grained diagnostic events.
    TRACE,
    /// Verbose information helpful during debugging.
    DEBUG,
    /// General operational information about application progress.
    INFO,
    /// Potentially harmful situations that warrant attention.
    WARN,
    /// Error events that might still allow the application to continue.
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
/// Logger configuration section.
///
/// Controls how logs are formatted and the minimum severity captured.
/// Can be provided via config file or environment variables.
pub struct LogConfig {
    /// Minimum severity to record (e.g., INFO, DEBUG). Higher severity means fewer logs.
    pub level: LogLevel,
    /// Output format for emitted logs (compact or JSON).
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
    pub fn load(config_path: &Path) -> Result<Self, Box<figment::Error>> {
        let mut figment = Figment::from(Serialized::defaults(AppConfig::default()));

        if config_path.exists() {
            figment = figment.merge(Toml::file(config_path));
        }
        figment = figment.merge(Env::prefixed("CHORTKE_").split("_"));

        let cfg = figment.extract()?;
        Ok(cfg)
    }
}
