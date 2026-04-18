//! HTTP response mapping for errors produced during request handling.
//!
//! The gateway produces two broad kinds of errors: client errors (bad
//! request, payload too large) and upstream errors (worker unreachable,
//! worker timed out). This module maps Sky's typed error hierarchy to
//! appropriate HTTP status codes and JSON error bodies.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sky_runtime::{ClientError, FrameworkError, GatewayError, WorkerError};

/// A serializable error body returned to HTTP clients.
///
/// Kept intentionally minimal — we expose the error message and a
/// machine-readable code, but nothing about internal structure.
/// Callers can use the `X-Request-Id` header to correlate with
/// gateway-side logs for deeper diagnosis.
#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

/// Wrapper type that lets us implement `IntoResponse` for `FrameworkError`
/// without orphan-rule violations.
///
/// Using this type in a handler's `Result<T, HttpError>` signature causes
/// axum to automatically convert errors into appropriate responses.
pub struct HttpError(pub FrameworkError);

impl From<FrameworkError> for HttpError {
    fn from(value: FrameworkError) -> Self {
        HttpError(value)
    }
}

impl From<WorkerError> for HttpError {
    fn from(value: WorkerError) -> Self {
        HttpError(FrameworkError::Worker(value))
    }
}

impl From<ClientError> for HttpError {
    fn from(value: ClientError) -> Self {
        HttpError(FrameworkError::Client(value))
    }
}

impl From<GatewayError> for HttpError {
    fn from(value: GatewayError) -> Self {
        HttpError(FrameworkError::Gateway(value))
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let (status, code) = classify(&self.0);

        // Log server-side errors — clients see the status code and
        // a sanitized message, but operators need the full detail.
        if status.is_server_error() {
            tracing::error!(error = %self.0, code, "request failed");
        } else {
            tracing::warn!(error = %self.0, code, "request rejected");
        }

        let body = ErrorBody {
            code,
            message: self.0.to_string(),
        };

        (status, Json(body)).into_response()
    }
}

/// Assign an HTTP status code and machine-readable error code to a framework error.
fn classify(err: &FrameworkError) -> (StatusCode, &'static str) {
    match err {
        FrameworkError::Worker(WorkerError::Unreachable { .. }) => {
            (StatusCode::SERVICE_UNAVAILABLE, "worker_unreachable")
        }
        FrameworkError::Worker(WorkerError::Timeout { .. }) => {
            (StatusCode::GATEWAY_TIMEOUT, "worker_timeout")
        }
        FrameworkError::Worker(WorkerError::ReadinessTimeout { .. }) => {
            (StatusCode::SERVICE_UNAVAILABLE, "worker_not_ready")
        }
        FrameworkError::Worker(WorkerError::ProtocolViolation { .. }) => {
            (StatusCode::BAD_GATEWAY, "worker_protocol_violation")
        }
        FrameworkError::Worker(WorkerError::WorkerReturnedError { .. }) => {
            (StatusCode::BAD_GATEWAY, "worker_returned_error")
        }

        FrameworkError::Worker(WorkerError::PermanentFailure { .. }) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "worker_permananent_failure",
        ),

        FrameworkError::Client(ClientError::InvalidBody(_)) => {
            (StatusCode::BAD_REQUEST, "invalid_body")
        }
        FrameworkError::Client(ClientError::PayloadTooLarge { .. }) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
        }
        FrameworkError::Client(ClientError::UnsupportedContentType(_)) => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
        ),

        FrameworkError::Gateway(GatewayError::ResourceExhaustion) => {
            (StatusCode::SERVICE_UNAVAILABLE, "resource_exhaustion")
        }
        FrameworkError::Gateway(GatewayError::ShutdownInProgress) => {
            (StatusCode::SERVICE_UNAVAILABLE, "shutting_down")
        }
        FrameworkError::Gateway(GatewayError::Internal(_)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }

        FrameworkError::Config(_) => {
            // Config errors shouldn't normally surface during request handling —
            // they should fail at startup. If one does surface, treat it as internal.
            (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_unreachable_maps_to_503() {
        let err = WorkerError::Unreachable {
            pool: "default".to_string(),
            reason: "connection refused".to_string(),
        };
        let (status, code) = classify(&FrameworkError::Worker(err));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "worker_unreachable");
    }

    #[test]
    fn worker_timeout_maps_to_504() {
        let err = WorkerError::Timeout {
            pool: "default".to_string(),
            elapsed_ms: 5000,
        };
        let (status, code) = classify(&FrameworkError::Worker(err));
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(code, "worker_timeout");
    }

    #[test]
    fn client_invalid_body_maps_to_400() {
        let err = ClientError::InvalidBody("missing field".to_string());
        let (status, code) = classify(&FrameworkError::Client(err));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "invalid_body");
    }

    #[test]
    fn payload_too_large_maps_to_413() {
        let err = ClientError::PayloadTooLarge {
            limit: 1024,
            actual: 2048,
        };
        let (status, code) = classify(&FrameworkError::Client(err));
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(code, "payload_too_large");
    }
}
