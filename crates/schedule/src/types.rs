//! Durable and model-facing Schedule value types.

use serde::{Deserialize, Serialize};

seekdeep_util::string_brand!(
    /// Stable reminder identity that is unique and never reused within one session.
    pub struct ScheduleId;
);

/// Durable one-shot reminder created from a positive delay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterScheduleRecord {
    /// Session-local stable identity.
    pub id: ScheduleId,
    /// Trimmed reminder content supplied at creation.
    pub prompt: String,
    /// Positive safe-integer delay accepted at creation.
    pub after_seconds: u64,
    /// Four-digit-year RFC 3339 UTC target.
    pub scheduled_at: String,
}

/// Durable one-shot reminder created from an absolute instant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtScheduleRecord {
    /// Session-local stable identity.
    pub id: ScheduleId,
    /// Trimmed reminder content supplied at creation.
    pub prompt: String,
    /// Four-digit-year RFC 3339 UTC target.
    pub scheduled_at: String,
}

/// Durable fixed-rate recurring reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EveryScheduleRecord {
    /// Session-local stable identity.
    pub id: ScheduleId,
    /// Trimmed reminder content supplied at creation.
    pub prompt: String,
    /// Fixed safe-integer interval, never below five minutes.
    pub every_seconds: u64,
    /// Earliest anchor-aligned occurrence not yet dispatched.
    pub scheduled_at: String,
}

/// Structured local-calendar input accepted by `schedule_create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAtInput {
    /// Four-digit ISO calendar date.
    pub date: String,
    /// Local wall-clock time with optional milliseconds.
    pub time: String,
    /// Explicit UTC or IANA Area/Location zone.
    pub time_zone: String,
}

/// Absolute selector accepted by `schedule_create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AtInput {
    /// RFC 3339 or offset string.
    String(String),
    /// Structured local calendar input.
    Local(LocalAtInput),
}

/// One-shot record variants that terminate on an id-only dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OneShotScheduleRecord {
    /// Delayed one-shot.
    #[serde(rename = "after")]
    After(AfterScheduleRecord),
    /// Absolute one-shot.
    #[serde(rename = "at")]
    At(AtScheduleRecord),
}

/// The v1 durable reminder record union.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScheduleRecord {
    /// Delayed one-shot.
    #[serde(rename = "after")]
    After(AfterScheduleRecord),
    /// Absolute one-shot.
    #[serde(rename = "at")]
    At(AtScheduleRecord),
    /// Fixed-rate recurring.
    #[serde(rename = "every")]
    Every(EveryScheduleRecord),
}

/// Creates one durable reminder record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleCreateChange {
    /// Wire version.
    pub version: u32,
    /// Literal create discriminator.
    pub operation: String,
    /// The created reminder record.
    pub schedule: ScheduleRecord,
}

/// Deletes one currently active reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDeleteChange {
    /// Wire version.
    pub version: u32,
    /// Literal delete discriminator.
    pub operation: String,
    /// Target identity.
    pub id: ScheduleId,
}

/// Records one dispatch decision; acceptedAt is absent for a one-shot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDispatchChange {
    /// Wire version.
    pub version: u32,
    /// Literal dispatch discriminator.
    pub operation: String,
    /// Target identity.
    pub id: ScheduleId,
    /// Wall-clock decision time for a fixed-rate dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<String>,
}

/// Strict version-1 durable Schedule mutation union.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
pub enum ScheduleChange {
    /// Create.
    #[serde(rename = "create")]
    Create(ScheduleCreateChange),
    /// Delete.
    #[serde(rename = "delete")]
    Delete(ScheduleDeleteChange),
    /// Dispatch.
    #[serde(rename = "dispatch")]
    Dispatch(ScheduleDispatchChange),
}

/// Current delivery timing derived from the durable record and wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleState {
    /// Target remains in the future.
    Scheduled,
    /// Target is due or past due.
    Overdue,
}

/// Fixed v1 delivery boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleDeliveryMode {
    /// The original session must be live.
    SessionLocal,
}

/// Complete model-facing view of one active reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleView {
    /// The underlying record.
    #[serde(flatten)]
    pub record: ScheduleRecord,
    /// Whether the target remains in the future.
    pub state: ScheduleState,
    /// Reminder delivery never leaves the owning session.
    pub delivery_mode: ScheduleDeliveryMode,
}

/// Management operations whose persistence barrier may be uncertain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulePersistenceOperation {
    /// Create.
    Create,
    /// List.
    List,
    /// Delete.
    Delete,
}

/// Closed v1 Schedule management error union.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ScheduleToolError {
    /// Empty reminder prompt.
    #[serde(rename = "invalid_prompt")]
    InvalidPrompt {
        /// Human-readable reason.
        message: String,
    },
    /// Missing, conflicting, or unsupported selector.
    #[serde(rename = "invalid_selector")]
    InvalidSelector {
        /// Human-readable reason.
        message: String,
    },
    /// Invalid rule or management argument.
    #[serde(rename = "invalid_rule")]
    InvalidRule {
        /// Human-readable reason.
        message: String,
    },
    /// Invalid or unsupported IANA time zone.
    #[serde(rename = "invalid_time_zone")]
    InvalidTimeZone {
        /// Human-readable reason.
        message: String,
    },
    /// Absolute target is not strictly future.
    #[serde(rename = "not_future")]
    NotFuture {
        /// Human-readable reason.
        message: String,
    },
    /// Computed instant cannot use a four-digit UTC year.
    #[serde(rename = "time_out_of_range")]
    TimeOutOfRange {
        /// Human-readable reason.
        message: String,
    },
    /// Fixed-rate rule runs more often than supported.
    #[serde(rename = "frequency_too_high")]
    FrequencyTooHigh {
        /// Human-readable reason.
        message: String,
    },
    /// Durable Schedule stream is malformed.
    #[serde(rename = "corrupt_schedule_log")]
    CorruptScheduleLog {
        /// Human-readable reason.
        message: String,
    },
    /// Required persistence checkpoint did not complete.
    #[serde(rename = "persistence_uncertain")]
    PersistenceUncertain {
        /// Human-readable reason.
        message: String,
        /// Management operation.
        operation: SchedulePersistenceOperation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Target identity, when known.
        id: Option<ScheduleId>,
    },
    /// Stable fallback that does not disclose an internal exception.
    #[serde(rename = "internal_error")]
    InternalError {
        /// Human-readable reason.
        message: String,
    },
}

/// Canonical `schedule_create` value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScheduleCreateValue {
    /// Success.
    View(ScheduleView),
    /// Failure.
    Error(ScheduleToolError),
}

/// Canonical `schedule_list` value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScheduleListValue {
    /// Success.
    Views(Vec<ScheduleView>),
    /// Failure.
    Error(ScheduleToolError),
}

/// Successful `schedule_delete` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDeleteResult {
    /// Target identity.
    pub id: ScheduleId,
    /// Whether an active reminder was deleted.
    pub deleted: bool,
    /// Non-mutating not-found discriminator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Canonical `schedule_delete` value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScheduleDeleteValue {
    /// Success (including not-found).
    Result(ScheduleDeleteResult),
    /// Failure.
    Error(ScheduleToolError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_change_round_trip() {
        let after = ScheduleRecord::After(AfterScheduleRecord {
            id: ScheduleId::new("s1"),
            prompt: "remind me".to_owned(),
            after_seconds: 60,
            scheduled_at: "2026-07-14T00:00:00Z".to_owned(),
        });
        let value = serde_json::to_value(&after).expect("serialize");
        assert_eq!(value["kind"], "after");
        assert_eq!(value["afterSeconds"], 60);
        assert_eq!(value["scheduledAt"], "2026-07-14T00:00:00Z");

        let change = ScheduleChange::Create(ScheduleCreateChange {
            version: 1,
            operation: "create".to_owned(),
            schedule: after,
        });
        let value = serde_json::to_value(&change).expect("serialize");
        assert_eq!(value["operation"], "create");
        assert_eq!(value["schedule"]["kind"], "after");
    }

    #[test]
    fn error_union_round_trips_by_code() {
        let error = ScheduleToolError::NotFuture {
            message: "not future".to_owned(),
        };
        let value = serde_json::to_value(&error).expect("serialize");
        assert_eq!(value["code"], "not_future");
        assert_eq!(value["message"], "not future");
    }
}
