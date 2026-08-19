//! Pure Schedule tool argument validation and stable error mapping.
//!
//! The tool registrations themselves are ported separately.

use crate::{
    domain::{MIN_EVERY_INTERVAL_SECONDS, ScheduleInputCode, ScheduleInputError},
    types::{AtInput, ScheduleId, SchedulePersistenceOperation, ScheduleToolError},
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Raw tool arguments for one `schedule_create` call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduleCreateArgs {
    /// Reminder content to present when the target becomes due.
    pub prompt: String,
    /// Positive safe-integer delay in seconds.
    pub after_seconds: Option<u64>,
    /// Absolute target as strict offset RFC 3339 or local date/time.
    pub at: Option<AtInput>,
    /// Fixed-rate safe-integer interval in seconds.
    pub every_seconds: Option<u64>,
}

/// Validates the v1 selector constraints the open parameter root cannot express.
#[must_use]
pub fn validate_create_args(args: &ScheduleCreateArgs) -> Option<ScheduleToolError> {
    let selectors = usize::from(args.after_seconds.is_some())
        + usize::from(args.at.is_some())
        + usize::from(args.every_seconds.is_some());
    if selectors != 1 {
        return Some(ScheduleToolError::InvalidSelector {
            message: "schedule_create accepts exactly one of after_seconds, at, or every_seconds."
                .to_owned(),
        });
    }
    if args.prompt.trim().is_empty() {
        return Some(ScheduleToolError::InvalidPrompt {
            message: "prompt must be non-empty after trimming.".to_owned(),
        });
    }
    if let Some(after_seconds) = args.after_seconds
        && (after_seconds == 0 || after_seconds > MAX_SAFE_INTEGER)
    {
        return Some(ScheduleToolError::InvalidRule {
            message: "after_seconds must be a positive safe integer.".to_owned(),
        });
    }
    if let Some(every_seconds) = args.every_seconds {
        if every_seconds > MAX_SAFE_INTEGER {
            return Some(ScheduleToolError::InvalidRule {
                message: "every_seconds must be a safe integer.".to_owned(),
            });
        }
        if every_seconds < MIN_EVERY_INTERVAL_SECONDS {
            return Some(ScheduleToolError::FrequencyTooHigh {
                message: format!("every_seconds must be at least {MIN_EVERY_INTERVAL_SECONDS}."),
            });
        }
    }
    None
}

/// Translates one contained input failure to the closed tool union.
#[must_use]
pub fn input_error(error: &ScheduleInputError) -> ScheduleToolError {
    match error.code {
        ScheduleInputCode::InvalidPrompt => ScheduleToolError::InvalidPrompt {
            message: error.message.clone(),
        },
        ScheduleInputCode::InvalidRule => ScheduleToolError::InvalidRule {
            message: error.message.clone(),
        },
        ScheduleInputCode::InvalidTimeZone => ScheduleToolError::InvalidTimeZone {
            message: error.message.clone(),
        },
        ScheduleInputCode::NotFuture => ScheduleToolError::NotFuture {
            message: error.message.clone(),
        },
        ScheduleInputCode::TimeOutOfRange => ScheduleToolError::TimeOutOfRange {
            message: error.message.clone(),
        },
        ScheduleInputCode::FrequencyTooHigh => ScheduleToolError::FrequencyTooHigh {
            message: error.message.clone(),
        },
    }
}

/// Stable durable-log failure.
#[must_use]
pub fn corrupt_log_error() -> ScheduleToolError {
    ScheduleToolError::CorruptScheduleLog {
        message: "The session schedule log is corrupt.".to_owned(),
    }
}

/// Stable failure for failures not safe to expose.
#[must_use]
pub fn internal_error() -> ScheduleToolError {
    ScheduleToolError::InternalError {
        message: "The schedule operation failed.".to_owned(),
    }
}

/// Stable persistence uncertainty with the known operation identity.
#[must_use]
pub fn persistence_error(
    operation: SchedulePersistenceOperation,
    id: Option<&ScheduleId>,
) -> ScheduleToolError {
    ScheduleToolError::PersistenceUncertain {
        message: "Schedule persistence is uncertain; retry with schedule_list before relying on this result.".to_owned(),
        operation,
        id: id.cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_selector_constraints() {
        let valid = ScheduleCreateArgs {
            prompt: "  check logs  ".to_owned(),
            after_seconds: Some(30),
            ..ScheduleCreateArgs::default()
        };
        assert!(validate_create_args(&valid).is_none());

        let empty = ScheduleCreateArgs {
            prompt: "  ".to_owned(),
            after_seconds: Some(30),
            ..ScheduleCreateArgs::default()
        };
        assert!(matches!(
            validate_create_args(&empty),
            Some(ScheduleToolError::InvalidPrompt { .. })
        ));

        let zero = ScheduleCreateArgs {
            prompt: "x".to_owned(),
            after_seconds: Some(0),
            ..ScheduleCreateArgs::default()
        };
        assert!(matches!(
            validate_create_args(&zero),
            Some(ScheduleToolError::InvalidRule { .. })
        ));

        let none = ScheduleCreateArgs {
            prompt: "x".to_owned(),
            ..ScheduleCreateArgs::default()
        };
        assert!(matches!(
            validate_create_args(&none),
            Some(ScheduleToolError::InvalidSelector { .. })
        ));

        let too_many = ScheduleCreateArgs {
            prompt: "x".to_owned(),
            after_seconds: Some(30),
            every_seconds: Some(300),
            ..ScheduleCreateArgs::default()
        };
        assert!(matches!(
            validate_create_args(&too_many),
            Some(ScheduleToolError::InvalidSelector { .. })
        ));

        let too_frequent = ScheduleCreateArgs {
            prompt: "x".to_owned(),
            every_seconds: Some(299),
            ..ScheduleCreateArgs::default()
        };
        assert!(matches!(
            validate_create_args(&too_frequent),
            Some(ScheduleToolError::FrequencyTooHigh { .. })
        ));
    }

    #[test]
    fn maps_input_codes_to_tool_errors() {
        let error = input_error(&ScheduleInputError::new(
            ScheduleInputCode::NotFuture,
            "not future",
        ));
        assert!(matches!(error, ScheduleToolError::NotFuture { .. }));
        assert!(matches!(
            corrupt_log_error(),
            ScheduleToolError::CorruptScheduleLog { .. }
        ));
        assert!(matches!(
            internal_error(),
            ScheduleToolError::InternalError { .. }
        ));
        assert!(matches!(
            persistence_error(SchedulePersistenceOperation::Create, None),
            ScheduleToolError::PersistenceUncertain { .. }
        ));
    }
}
