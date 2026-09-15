//! Runtime constructors and protocol constants for the goal domain.

use crate::domain::GoalErrorCode;
use thiserror::Error;

/// Version of the goal change embedded in a round-zero message source.
pub const GOAL_CHANGE_VERSION: u32 = 1;

/// Error returned by the goal domain boundary.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct GoalError {
    /// Stable machine-routable classification.
    pub code: GoalErrorCode,
    /// Human-readable rejection reason.
    pub message: String,
}

impl GoalError {
    /// Creates one classified goal failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: GoalErrorCode) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "GoalError"
    }
}
