//! Shared runtime primitives for the Sky framework.
//!
//! This crate holds types and utilities used across the framework:
//! error types, trace context, request identifiers, and shared data
//! structures. Dependencies are intentionally minimal — this crate is
//! imported by nearly every other crate in the workspace.
//!
//! # Public API stability
//!
//! Types exposed from this crate are part of Sky's public API. Breaking
//! changes require a major version bump. Adding new types or new non-breaking
//! variants to existing enums is fine.

pub mod error;
pub mod handler;
pub mod request_id;
pub mod tracing_helpers;

// Re-export the most commonly used types at crate root for ergonomic
// access. Callers can use `sky_runtime::RequestId` instead of
// `sky_runtime::request_id::RequestId`.
pub use error::{ClientError, ConfigError, FrameworkError, GatewayError, WorkerError};
pub use handler::HandlerDescriptor;
pub use request_id::{ParseRequestIdError, RequestId};
pub use tracing_helpers::request_span;
