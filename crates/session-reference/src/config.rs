//! Configuration and stable diagnostics for session references.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Hard maximum references accepted by one message.
pub const MAX_REFERENCES: u64 = 3;
/// Default number of discovery candidates returned to a host.
pub const DEFAULT_CANDIDATE_LIMIT: u64 = 50;
/// Default UTF-8 budget for one rendered reference JSON object.
pub const DEFAULT_MAX_REFERENCE_BYTES: u64 = 65_536;

/// Session-reference service configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Maximum distinct source sessions referenced by one message, from one to three.
    pub max_references: Option<u64>,
    /// Default host candidate-list limit.
    pub candidate_limit: Option<u64>,
    /// Maximum rendered UTF-8 bytes for one source snapshot.
    pub max_reference_bytes: Option<u64>,
}

/// Stable failure codes exposed to host adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionReferenceErrorCode {
    /// Invalid service configuration.
    SessionReferenceInvalidConfig,
    /// Malformed or non-canonical reference.
    SessionReferenceInvalidReference,
    /// A session references itself.
    SessionReferenceSelfReference,
    /// Too many references in one message.
    SessionReferenceTooMany,
    /// Source-session read failed.
    SessionReferenceReadFailed,
    /// Snapshot could not fit the byte budget.
    SessionReferenceBudgetExceeded,
    /// Preparation was cancelled.
    SessionReferenceCancelled,
}

/// Typed session-reference failure suitable for host protocol error mapping.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct SessionReferenceError {
    /// Human-readable diagnosis.
    pub message: String,
    /// Stable routing code.
    pub code: SessionReferenceErrorCode,
}

impl SessionReferenceError {
    /// Creates one classified session-reference failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: SessionReferenceErrorCode) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
}
