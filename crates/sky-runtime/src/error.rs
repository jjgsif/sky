//! Typed error hierarchy for the Sky framework.
//!
//! Errors are organized into four categories matching architectural
//! concerns: worker, client, gateway, and config. The top-level
//! `FrameworkError` wraps any category, allowing uniform propagation;
//! inner types are used when callers want to handle a specific
//! category.

use thiserror::Error;

/// Top-level framework error. Any error produced within Sky's runtime
/// will be one of these variants. Prefer returning the inner types
/// (e.g., `WorkerError`) when the failure category is known at the
/// call site.
#[derive(Debug, Error)]
pub enum FrameworkError {
    #[error(transparent)]
    Worker(#[from] WorkerError),

    #[error(transparent)]
    Client(#[from] ClientError),

    #[error(transparent)]
    Gateway(#[from] GatewayError),

    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// Errors related to worker communication and lifecycle.
#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("worker pool '{pool}' unreachable: {reason}")]
    Unreachable { pool: String, reason: String },

    #[error("worker pool '{pool}' timed out after {elapsed_ms}ms")]
    Timeout { pool: String, elapsed_ms: u64 },

    #[error("worker pool '{pool}' returned protocol violation: {detail}")]
    ProtocolViolation { pool: String, detail: String },

    #[error("worker pool '{pool}' returned error status: {message}")]
    WorkerReturnedError { pool: String, message: String },

    #[error("worker pool '{pool}' failed to become ready within {timeout_ms}ms")]
    ReadinessTimeout { pool: String, timeout_ms: u64 },
}

/// Errors produced by invalid client requests.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid request body: {0}")]
    InvalidBody(String),

    #[error("payload too large: limit {limit} bytes, got {actual} bytes")]
    PayloadTooLarge { limit: u64, actual: u64 },

    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),
}

/// Errors produced by the gateway itself, independent of client or worker.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("gateway resource exhaustion")]
    ResourceExhaustion,

    #[error("gateway is shutting down")]
    ShutdownInProgress,

    #[error("internal gateway error: {0}")]
    Internal(String),
}

/// Errors produced during gateway configuration and startup.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required configuration field: {0}")]
    MissingField(String),

    #[error("invalid value for field '{field}': {value}")]
    InvalidValue { field: String, value: String },

    #[error("worker binary not found at path: {0}")]
    WorkerBinaryNotFound(String),

    #[error("worker binary not executable at path: {0}")]
    WorkerBinaryNotExecutable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_error_displays_context() {
        let e = WorkerError::Unreachable {
            pool: "default".to_string(),
            reason: "connection refused".to_string(),
        };
        let s = format!("{}", e);
        assert!(s.contains("default"));
        assert!(s.contains("connection refused"));
    }

    #[test]
    fn client_error_payload_too_large_includes_sizes() {
        let e = ClientError::PayloadTooLarge {
            limit: 1024,
            actual: 2048,
        };
        let s = format!("{}", e);
        assert!(s.contains("1024"));
        assert!(s.contains("2048"));
    }

    #[test]
    fn framework_error_wraps_inner_error_transparently() {
        let inner = WorkerError::Timeout {
            pool: "jobs".to_string(),
            elapsed_ms: 5000,
        };
        let wrapped: FrameworkError = inner.into();
        let s = format!("{}", wrapped);
        // Transparent wrapping means the message equals the inner error.
        assert!(s.contains("jobs"));
        assert!(s.contains("5000"));
    }

    #[test]
    fn config_error_missing_field_is_specific() {
        let e = ConfigError::MissingField("worker_binary".to_string());
        let s = format!("{}", e);
        assert!(s.contains("worker_binary"));
    }
}
