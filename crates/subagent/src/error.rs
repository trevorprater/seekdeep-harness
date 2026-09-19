//! Typed failures shared by subagent service and provider operations.

use seekdeep_llm::HarnessError;
use thiserror::Error;

/// Typed failure for the subagent seam.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct SubagentError {
    /// Machine-routable taxonomy (open-ended string).
    pub code: String,
    /// Human-readable failure.
    pub message: String,
    #[source]
    inner: HarnessError,
}

impl SubagentError {
    /// Creates one classified subagent failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        let message = message.into();
        let code = code.into();
        Self {
            inner: HarnessError::named("SubagentError", message.clone(), code.clone()),
            code,
            message,
        }
    }

    /// JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "SubagentError"
    }
}

impl Clone for SubagentError {
    fn clone(&self) -> Self {
        Self::new(self.message.clone(), self.code.clone())
    }
}

impl PartialEq for SubagentError {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code && self.message == other.message
    }
}

impl Eq for SubagentError {}
