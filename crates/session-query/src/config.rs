//! Public configuration and typed failures for the combined session-query service.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default maximum before/after raw-event window.
pub const SESSION_QUERY_READ_WINDOW_MAX: u64 = 50;

/// Default maximum number of concurrent persisted-log inspections in one batch read.
pub const SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY: u64 = 4;

/// Trims and collapses the exact ECMAScript Unicode whitespace set.
#[must_use]
pub fn normalize_session_query_whitespace(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars() {
        if is_ecmascript_whitespace(character) {
            separator = !output.is_empty();
        } else {
            if separator {
                output.push(' ');
                separator = false;
            }
            output.push(character);
        }
    }
    output
}

const fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

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

impl SessionQueryErrorCode {
    /// Stable source-compatible wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionQueryAborted => "SESSION_QUERY_ABORTED",
            Self::SessionQueryCorruptSession => "SESSION_QUERY_CORRUPT_SESSION",
            Self::SessionQueryEventNotFound => "SESSION_QUERY_EVENT_NOT_FOUND",
            Self::SessionQueryIndexFailed => "SESSION_QUERY_INDEX_FAILED",
            Self::SessionQueryInvalidConfig => "SESSION_QUERY_INVALID_CONFIG",
            Self::SessionQueryInvalidCursor => "SESSION_QUERY_INVALID_CURSOR",
            Self::SessionQueryInvalidFilter => "SESSION_QUERY_INVALID_FILTER",
            Self::SessionQueryInvalidLimit => "SESSION_QUERY_INVALID_LIMIT",
            Self::SessionQueryInvalidQuery => "SESSION_QUERY_INVALID_QUERY",
            Self::SessionQueryInvalidLineage => "SESSION_QUERY_INVALID_LINEAGE",
            Self::SessionQueryInvalidSurface => "SESSION_QUERY_INVALID_SURFACE",
            Self::SessionQueryInvalidWindow => "SESSION_QUERY_INVALID_WINDOW",
            Self::SessionQueryPersistenceFailed => "SESSION_QUERY_PERSISTENCE_FAILED",
            Self::SessionQuerySearchDisabled => "SESSION_QUERY_SEARCH_DISABLED",
            Self::SessionQuerySessionNotFound => "SESSION_QUERY_SESSION_NOT_FOUND",
            Self::SessionQueryStaleCursor => "SESSION_QUERY_STALE_CURSOR",
            Self::SessionQuerySourceConflict => "SESSION_QUERY_SOURCE_CONFLICT",
        }
    }
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
        assert_eq!(
            SessionQueryErrorCode::SessionQuerySourceConflict.as_str(),
            "SESSION_QUERY_SOURCE_CONFLICT"
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

    #[test]
    fn query_whitespace_matches_ecmascript_including_bom_but_not_next_line() {
        assert_eq!(
            normalize_session_query_whitespace("\u{feff}alpha\u{2003}\n beta\u{feff}"),
            "alpha beta"
        );
        assert_eq!(normalize_session_query_whitespace("\u{0085}"), "\u{0085}");
    }
}
