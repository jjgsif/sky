//! Helpers for consistent tracing across the framework.
//!
//! The tracing crate provides structured logging primitives (events and
//! spans). These helpers standardize the span structures used throughout
//! Sky so that fields like `request_id` appear consistently in every
//! log and every trace.

use crate::request_id::RequestId;
use tracing::Span;

/// Create a top-level span for a single request. The span includes the
/// request ID as a structured field, so all events emitted while the
/// span is active will be tagged with it.
///
/// # Usage
///
/// ```no_run
/// use sky_runtime::{request_span, RequestId};
///
/// let span = request_span(RequestId::new());
/// let _guard = span.enter();
/// tracing::info!("handling request");
/// // The info event inherits the request_id field from the span.
/// ```
#[must_use]
pub fn request_span(request_id: RequestId) -> Span {
    tracing::info_span!("request", request_id = %request_id)
}

#[cfg(test)]
mod tests {
    use super::{request_span, RequestId};

    #[test]
    fn request_span_creates_span() {
        let id = RequestId::new();
        let span = request_span(id);
        // The span isn't "entered" yet, so it's inactive. We're just
        // verifying construction doesn't panic.
        let _ = span;
    }
}