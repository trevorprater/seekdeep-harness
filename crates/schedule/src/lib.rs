//! Schedule domain: durable reminder vocabulary and the persistence barrier.
//! The domain fold and tool service are ported separately.

pub mod domain;
pub mod index;
pub mod invariant;
pub mod persistence;
pub mod runtime;
pub mod tools;
pub mod transaction;
pub mod types;

pub use domain::{
    EveryOccurrence, EveryReminder, FoldedSchedules, MIN_EVERY_INTERVAL_SECONDS,
    SCHEDULE_CHANGE_VERSION, ScheduleInputCode, ScheduleInputError, ScheduleLogError,
    allocate_schedule_id, canonicalize_time_zone, create_after_schedule_record,
    create_at_schedule_record, create_every_schedule_record, decode_schedule_change,
    fold_schedule_events, render_every_reminder_batch_framing, render_reminder_framing,
    resolve_every_occurrence, schedule_view,
};
pub use index::{INJECT, NAME as PACKAGE_NAME, apply, plugin};
pub use invariant::{NAME, register_invariant};
pub use persistence::{SchedulePersistenceError, flush_schedule_persistence};
pub use runtime::{
    DueDecision, MAX_TIMER_DELAY_MS, ScheduleClock, ScheduleMessageFactory, ScheduleRuntime,
    due_decision,
};
pub use tools::{
    ScheduleCreateArgs, corrupt_log_error, input_error, internal_error, persistence_error,
    register_schedule_tools, validate_create_args,
};
pub use transaction::run_schedule_transaction;
pub use types::{
    AfterScheduleRecord, AtInput, AtScheduleRecord, EveryScheduleRecord, LocalAtInput,
    OneShotScheduleRecord, ScheduleChange, ScheduleCreateChange, ScheduleCreateValue,
    ScheduleDeleteChange, ScheduleDeleteResult, ScheduleDeleteValue, ScheduleDeliveryMode,
    ScheduleDispatchChange, ScheduleId, ScheduleListValue, SchedulePersistenceOperation,
    ScheduleRecord, ScheduleState, ScheduleToolError, ScheduleView,
};
