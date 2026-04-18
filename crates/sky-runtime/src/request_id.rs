//! Request identifiers for observability and correlation.
//!
//! Request IDs are UUIDv7 — time-ordered 128-bit identifiers. Generated
//! at HTTP ingress, propagated through the entire request lifecycle,
//! included in every log line and every gRPC metadata header, and
//! returned to clients via the `X-Request-Id` response header.
//!
//! Time-ordered IDs mean that sorting logs by request ID approximates
//! chronological order, which is useful for debugging even when
//! timestamps are unreliable (clock skew, log reordering, etc.).

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// A unique identifier for a single request or operation, time-ordered
/// for convenient log sorting.
///
/// Wraps `uuid::Uuid` to provide type safety: a `RequestId` cannot be
/// accidentally confused with an unrelated UUID elsewhere in the codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(Uuid);

impl RequestId {
    /// Generate a new request ID based on the current time.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Access the underlying UUID. Use sparingly; prefer methods on
    /// `RequestId` itself when possible.
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_generates_unique_ids() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn v7_ids_are_time_ordered() {
        // Generate two IDs separated by a small sleep. The second
        // should sort greater than the first thanks to UUIDv7's
        // timestamp prefix.
        let a = RequestId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = RequestId::new();
        assert!(
            a.as_uuid() < b.as_uuid(),
            "expected time-ordered IDs: {a} should be < {b}"
        );
    }

    #[test]
    fn display_produces_standard_uuid_format() {
        let id = RequestId::new();
        let s = format!("{id}");
        // Standard UUID string is 36 characters: 8-4-4-4-12 plus four dashes.
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn round_trips_through_json() {
        let original = RequestId::new();
        let json = serde_json::to_string(&original).unwrap();
        let parsed: RequestId = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);
    }
}