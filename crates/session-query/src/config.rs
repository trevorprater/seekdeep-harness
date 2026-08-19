//! Public configuration and typed failures for the combined session-query service.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default maximum before/after raw-event window.
pub const SESSION_QUERY_READ_WINDOW_MAX: u64 = 50;

/// Default maximum number of concurrent persisted-log inspections in one batch read.
pub const SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY: u64 = 4;

/// Backend-independent configuration inherited by every session-query implementation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Maximum accepted raw read context on either side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_window_max: Option<u64>,
    /// Maximum concurrent persisted-log inspections in one batch read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_inspect_concurrency: Option<u64>,
}

/// Stable machine-routable failure taxonomy for session reads, traces, and search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionQueryErrorCode {
    /// The request was aborted.
    SessionQueryAborted,
    /// A session log is corrupt.
    SessionQueryCorruptSession,
    /// An event was not found.
    SessionQueryEventNotFound,
    /// Indexing failed.
    SessionQueryIndexFailed,
    /// Invalid configuration.
    SessionQueryInvalidConfig,
    /// Invalid cursor.
    SessionQueryInvalidCursor,
    /// Invalid filter.
    SessionQueryInvalidFilter,
    /// Invalid limit.
    SessionQueryInvalidLimit,
    /// Invalid query.
    SessionQueryInvalidQuery,
    /// Invalid lineage.
    SessionQueryInvalidLineage,
    /// Invalid surface.
    SessionQueryInvalidSurface,
    /// Invalid window.
    SessionQueryInvalidWindow,
    /// Persistence failed.
    SessionQueryPersistenceFailed,
    /// Search is disabled.
    SessionQuerySearchDisabled,
    /// Session not found.
    SessionQuerySessionNotFound,
    /// Stale cursor.
    SessionQueryStaleCursor,
    /// Conflicting source headers.
    SessionQuerySourceConflict,
}

/// Typed session-query failure whose code is one closed taxonomy member.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct SessionQueryError {
    /// Stable machine-routable classification.
    pub code: SessionQueryErrorCode,
    /// Human-readable rejection reason.
    pub message: String,
}

impl SessionQueryError {
    /// Creates one classified session-query failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: SessionQueryErrorCode) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "SessionQueryError"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_serializes_to_screaming_snake_case() {
        assert_eq!(
            serde_json::to_string(&SessionQueryErrorCode::SessionQuerySourceConflict)
                .expect("code"),
            "\"SESSION_QUERY_SOURCE_CONFLICT\""
        );
    }

    #[test]
    fn error_carries_code_and_message() {
        let error = SessionQueryError::new(
            "conflict",
            SessionQueryErrorCode::SessionQuerySourceConflict,
        );
        assert_eq!(
            error.code,
            SessionQueryErrorCode::SessionQuerySourceConflict
        );
        assert_eq!(error.to_string(), "conflict");
        assert_eq!(error.name(), "SessionQueryError");
    }
}
