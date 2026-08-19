//! Typed failures shared by subagent service and provider operations.

use thiserror::Error;

/// Typed failure for the subagent seam.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct SubagentError {
    /// Machine-routable taxonomy (open-ended string).
    pub code: String,
    /// Human-readable failure.
    pub message: String,
}

impl SubagentError {
    /// Creates one classified subagent failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "SubagentError"
    }
}
