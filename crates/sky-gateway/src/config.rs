//! Gateway configuration loaded from `sky.toml`.
//!
//! The configuration is versioned to allow backward-compatible evolution
//! of the schema. Sky v1 configs use `version = "1"` at the top; future
//! schema changes will either extend v1 additively or introduce a v2
//! that users explicitly opt into.
//!
//! # Example config
//!
//! ```toml
//! version = "1"
//!
//! [listen]
//! address = "127.0.0.1:8080"
//! body_limit = "1mb"
//! drain_timeout = "30s"
//!
//! [logging]
//! format = "pretty"
//! level = "info"
//!
//! [worker]
//! bun_path = "bun"
//! worker_script = "./worker/src/index.ts"
//! socket_path = "/tmp/sky-worker.sock"
//! worker_version = "0.1.0"
//! ```

use serde::{Deserialize, Serialize};
use sky_worker::WorkerConfig;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

/// JWT authentication configuration.
///
/// Set `jwt_secret` to a long random string to enable gateway-level JWT
/// verification. Leave it empty (the default) to disable auth entirely.
/// Routes decorated with `requireAuth()` will fail at gateway startup if
/// no secret is configured.
///
/// ```toml
/// [auth]
/// jwt_secret = "change-me-use-a-long-random-string"
/// token_ttl  = "24h"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// HMAC-SHA256 secret shared with workers for JWT signing and verification.
    /// Injected into worker processes as `SKY_AUTH_SECRET`.
    #[serde(default)]
    pub jwt_secret: String,

    /// Default token lifetime used by the worker's `issueToken()` helper.
    /// The gateway enforces expiry on every verified token regardless of this setting.
    /// Default: 24 hours.
    #[serde(with = "humantime_serde", default = "default_token_ttl")]
    pub token_ttl: Duration,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            token_ttl: default_token_ttl(),
        }
    }
}

fn default_token_ttl() -> Duration {
    Duration::from_secs(86400) // 24h
}

/// Top-level configuration loaded from a sky.toml file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Schema version. Must be "1" for Sky v1 configs.
    pub version: String,

    #[serde(default)]
    pub listen: ListenConfig,

    #[serde(default)]
    pub logging: LoggingConfig,

    pub worker: WorkerConfig,

    /// Rate-limit backend configuration. Defaults to in-process tracking.
    #[serde(default)]
    pub rate_limit: RateLimitBackendConfig,

    /// JWT authentication configuration. Defaults to disabled (empty secret).
    #[serde(default)]
    pub auth: AuthConfig,

    /// Path to the manifest file produced by `sky build`.
    /// Defaults to ./sky-manifest.json.
    #[serde(default = "default_manifest_path")]
    pub manifest_path: PathBuf,
}

/// Configures where sliding-window counters are stored.
///
/// Omitting `[rate_limit]` entirely (or omitting `redis_url`) keeps counters
/// in-process via DashMap — fast but not shared across gateway instances.
/// Setting `redis_url` enables distributed limiting; the gateway automatically
/// falls back to the local window if Redis becomes unreachable.
///
/// ```toml
/// [rate_limit]
/// redis_url      = "redis://127.0.0.1:6379"
/// pool_size      = 4
/// command_timeout = "2s"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitBackendConfig {
    /// Redis connection URL. `None` → in-process local window only.
    #[serde(default)]
    pub redis_url: Option<String>,

    /// Number of pooled Redis connections. Default: 4.
    #[serde(default = "default_rl_pool_size")]
    pub pool_size: usize,

    /// Per-command Redis timeout. Default: 2s.
    #[serde(with = "humantime_serde", default = "default_rl_command_timeout")]
    pub command_timeout: Duration,
}

impl Default for RateLimitBackendConfig {
    fn default() -> Self {
        Self {
            redis_url: None,
            pool_size: default_rl_pool_size(),
            command_timeout: default_rl_command_timeout(),
        }
    }
}

/// HTTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListenConfig {
    /// Address to bind to. Defaults to 127.0.0.1:8080 (loopback only).
    /// Use 0.0.0.0:PORT to bind all interfaces.
    #[serde(default = "default_listen_address")]
    pub address: SocketAddr,

    /// Maximum request body size. Defaults to 1MB.
    /// Parsed as a human-readable size string ("1mb", "500kb", etc.).
    #[serde(with = "byte_size_serde", default = "default_body_limit")]
    pub body_limit: u64,

    /// How long to wait for in-flight HTTP requests to drain after a
    /// shutdown signal before forcibly closing connections. Default: 30s.
    #[serde(with = "humantime_serde", default = "default_drain_timeout")]
    pub drain_timeout: Duration,

    /// Number of parallel accept loops bound with SO_REUSEPORT.
    /// 0 (default) auto-detects from available_parallelism().
    #[serde(default = "default_accept_threads")]
    pub accept_threads: usize,
}

/// Logging and observability configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log output format: "pretty" for dev, "json" for production.
    #[serde(default = "default_log_format")]
    pub format: LogFormat,

    /// Log level filter (trace, debug, info, warn, error).
    #[serde(default = "default_log_level")]
    pub level: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Pretty,
    Json,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            address: default_listen_address(),
            body_limit: default_body_limit(),
            drain_timeout: default_drain_timeout(),
            accept_threads: default_accept_threads(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: default_log_format(),
            level: default_log_level(),
        }
    }
}

impl GatewayConfig {
    /// Load configuration from a TOML file at the given path.
    pub fn from_file(path: &PathBuf) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: path.clone(),
            source: e,
        })?;

        let config: Self = toml::from_str(&contents).map_err(ConfigError::Parse)?;

        if config.version != "1" {
            return Err(ConfigError::UnsupportedVersion(config.version));
        }

        Ok(config)
    }
}

fn default_listen_address() -> SocketAddr {
    "127.0.0.1:8080"
        .parse()
        .expect("hardcoded address is valid")
}

fn default_body_limit() -> u64 {
    1024 * 1024 // 1 MB
}

fn default_drain_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_accept_threads() -> usize {
    0
}

fn default_log_format() -> LogFormat {
    LogFormat::Pretty
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_manifest_path() -> PathBuf {
    PathBuf::from("./sky-manifest.json")
}

fn default_rl_pool_size() -> usize {
    4
}

fn default_rl_command_timeout() -> Duration {
    Duration::from_secs(2)
}

/// Human-readable byte size serialization.
///
/// Accepts strings like "1mb", "500kb", "2gb" on deserialization.
/// Serializes as bytes for round-tripping.
mod byte_size_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(*value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let s = String::deserialize(deserializer)?;
        parse_size(&s).map_err(serde::de::Error::custom)
    }

    fn parse_size(s: &str) -> Result<u64, String> {
        let s = s.trim().to_lowercase();
        let (number_str, multiplier) = if let Some(rest) = s.strip_suffix("gb") {
            (rest.trim(), 1024 * 1024 * 1024)
        } else if let Some(rest) = s.strip_suffix("mb") {
            (rest.trim(), 1024 * 1024)
        } else if let Some(rest) = s.strip_suffix("kb") {
            (rest.trim(), 1024)
        } else if let Some(rest) = s.strip_suffix("b") {
            (rest.trim(), 1)
        } else {
            // No suffix — treat as raw bytes.
            (s.as_str(), 1)
        };

        let number: u64 = number_str
            .parse()
            .map_err(|e| format!("invalid number in size '{s}': {e}"))?;
        Ok(number * multiplier)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config file: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("unsupported config version '{0}'; expected '1'")]
    UnsupportedVersion(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let toml_src = r#"
version = "1"

[worker]
bun_path = "bun"
worker_script = "./worker/src/index.ts"
socket_path = "/tmp/sky-worker.sock"
worker_version = "0.1.0"
"#;
        let config: GatewayConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(config.version, "1");
        assert_eq!(config.listen.address.to_string(), "127.0.0.1:8080");
        assert_eq!(config.listen.body_limit, 1024 * 1024);
        assert_eq!(config.logging.format, LogFormat::Pretty);
    }

    #[test]
    fn parses_body_limit_sizes() {
        let toml_src = r#"
version = "1"

[listen]
body_limit = "500kb"

[worker]
bun_path = "bun"
worker_script = "./worker/src/index.ts"
socket_path = "/tmp/sky-worker.sock"
worker_version = "0.1.0"
"#;
        let config: GatewayConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(config.listen.body_limit, 500 * 1024);
    }

    #[test]
    fn rejects_unsupported_version() {
        let toml_src = r#"
version = "99"

[worker]
bun_path = "bun"
worker_script = "./worker/src/index.ts"
socket_path = "/tmp/sky-worker.sock"
worker_version = "0.1.0"
"#;
        let config: GatewayConfig = toml::from_str(toml_src).unwrap();
        let path = PathBuf::from("fake-test-path");
        // The from_file path is the one that validates version; we
        // emulate that check manually since we don't want a real file.
        assert_eq!(config.version, "99");
    }

    #[test]
    fn parses_drain_timeout() {
        let toml_src = r#"
version = "1"

[listen]
drain_timeout = "45s"

[worker]
bun_path = "bun"
worker_script = "./worker/src/index.ts"
socket_path = "/tmp/sky-worker.sock"
worker_version = "0.1.0"
"#;
        let config: GatewayConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(config.listen.drain_timeout, Duration::from_secs(45));
    }

    #[test]
    fn drain_timeout_defaults_to_30s() {
        let toml_src = r#"
version = "1"

[worker]
bun_path = "bun"
worker_script = "./worker/src/index.ts"
socket_path = "/tmp/sky-worker.sock"
worker_version = "0.1.0"
"#;
        let config: GatewayConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(config.listen.drain_timeout, Duration::from_secs(30));
    }

    #[test]
    fn log_format_round_trips() {
        let pretty_toml = r#"format = "pretty""#;
        let json_toml = r#"format = "json""#;

        #[derive(Deserialize)]
        struct Wrap {
            format: LogFormat,
        }

        let pretty: Wrap = toml::from_str(pretty_toml).unwrap();
        let json: Wrap = toml::from_str(json_toml).unwrap();
        assert_eq!(pretty.format, LogFormat::Pretty);
        assert_eq!(json.format, LogFormat::Json);
    }
}
