//! Compaction service-definition vocabulary: triggers, classified manual
//! failures, and the agent context backends consume.

use std::sync::Arc;

use seekdeep_core::session::Session;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why automatic policy is asking a backend to consider compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompactionTrigger {
    /// Normal pressure.
    Pressure,
    /// Provider-confirmed context overflow.
    ContextOverflow,
}

/// Expected failure classes for an explicit idle-session compaction request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManualCompactionErrorCode {
    /// Another compaction owns the session lock.
    Busy,
    /// The agent or request was cancelled.
    Cancelled,
    /// The selected span changed under the summary.
    Changed,
    /// Summarization or shrink failed.
    Summary,
    /// The commit stage failed.
    Commit,
    /// Persistence failed.
    Persistence,
}

/// Expected manual-compaction failure suitable for a direct human-command result.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct ManualCompactionError {
    /// Stable failure class.
    pub code: ManualCompactionErrorCode,
    /// Backend diagnostic retained as the error message.
    pub message: String,
}

impl ManualCompactionError {
    /// Creates one classified compaction failure.
    #[must_use]
    pub fn new(code: ManualCompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Routing options guiding a backend's summarization call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionRoutingOptions {
    /// Provider route override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Minimal agent context compaction needs without depending on the agent package.
#[derive(Clone, Debug)]
pub struct CompactionAgentContext {
    /// Session whose surface is compacted.
    pub session: Arc<Session>,
    /// Routing options for the summary call.
    pub options: CompactionRoutingOptions,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_and_error_code_round_trip() {
        assert_eq!(
            serde_json::to_string(&CompactionTrigger::ContextOverflow).expect("trigger"),
            "\"context-overflow\""
        );
        assert_eq!(
            serde_json::to_string(&ManualCompactionErrorCode::Persistence).expect("code"),
            "\"persistence\""
        );
    }

    #[test]
    fn manual_compaction_error_carries_code_and_message() {
        let error = ManualCompactionError::new(ManualCompactionErrorCode::Busy, "already running");
        assert_eq!(error.code, ManualCompactionErrorCode::Busy);
        assert_eq!(error.message, "already running");
        assert_eq!(error.to_string(), "already running");
    }
}
