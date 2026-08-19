//! Pure Schedule tool argument validation and stable error mapping.
//!
//! The tool registrations themselves are ported separately.

use crate::{
    domain::{MIN_EVERY_INTERVAL_SECONDS, ScheduleInputCode, ScheduleInputError},
    types::{AtInput, ScheduleId, SchedulePersistenceOperation, ScheduleToolError},
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Raw tool arguments for one `schedule_create` call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
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

use std::sync::Arc;

use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_llm::ContentBlock;
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, TOOLS, ToolRuntime, define_tool,
    presentation::{GenericCallView, ToolCallKind, ToolCallView},
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    domain::{
        FoldedSchedules, allocate_schedule_id, create_after_schedule_record,
        create_at_schedule_record, create_every_schedule_record, fold_schedule_events,
        schedule_view,
    },
    persistence::flush_schedule_persistence,
    transaction::run_schedule_transaction,
    types::{
        ScheduleCreateValue, ScheduleDeleteResult, ScheduleDeleteValue, ScheduleListValue,
        ScheduleRecord,
    },
};

#[derive(Clone, Debug, Deserialize)]
struct ScheduleDeleteArgs {
    id: String,
}

/// Placeholder the registry replaces with its canonical ABORTED result.
fn cancellation_placeholder(signal: &seekdeep_llm::AbortSignal) -> Option<ScheduleToolError> {
    if signal.is_aborted() {
        Some(internal_error())
    } else {
        None
    }
}

/// Fold only after a successful preflight, mapping corruption to a stable value.
fn fold_for_tool(agent: &Arc<Agent>) -> Result<FoldedSchedules, ScheduleToolError> {
    let seed_length = agent
        .session()
        .header()
        .seed_length
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    fold_schedule_events(&agent.session().events(), seed_length).map_err(|_| corrupt_log_error())
}

/// Require one persistence checkpoint without leaking the backend failure.
async fn preflight(
    root_ctx: &Context,
    agent: &Arc<Agent>,
    operation: SchedulePersistenceOperation,
    id: Option<&ScheduleId>,
) -> Option<ScheduleToolError> {
    match flush_schedule_persistence(root_ctx, agent.session()).await {
        Ok(()) => None,
        Err(_) => Some(persistence_error(operation, id)),
    }
}

/// Contain a durable-change observer failure without aborting the operation.
fn notify(_root_ctx: &Context, on_durable_change: &Arc<dyn Fn() + Send + Sync>) {
    if let Err(error) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_durable_change()))
    {
        tracing::warn!(?error, "schedule: durable-change observer failed");
    }
}

/// Deterministic model content for every canonical Schedule value.
fn render_value<A, O: serde::Serialize>(_args: &A, value: &O) -> anyhow::Result<Vec<ContentBlock>> {
    Ok(vec![ContentBlock::Text {
        text: serde_json::to_string(value)?,
    }])
}

fn present(title: &str, kind: ToolCallKind, raw_input: Option<&str>) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        title: title.to_owned(),
        kind: Some(kind),
        raw_input: raw_input.map(|value| json!(value)),
        content: None,
        locations: None,
    })
}

/// Registers all three Schedule tools in one exact agent scope.
///
/// # Errors
///
/// Returns the first tool registration failure.
pub fn register_schedule_tools(
    root_ctx: &Context,
    tool_ctx: &Context,
    agent: Arc<Agent>,
    on_durable_change: impl Fn() + Send + Sync + 'static,
) -> anyhow::Result<EffectHandle> {
    let tools: Arc<ToolRuntime> = tool_ctx
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("schedule requires tools"))?;
    let on_durable_change = Arc::new(on_durable_change);

    let create = schedule_create_definition(root_ctx, agent.clone(), on_durable_change.clone())?;
    let list = schedule_list_definition(root_ctx, agent.clone(), on_durable_change.clone())?;
    let delete = schedule_delete_definition(root_ctx, agent, on_durable_change.clone())?;
    let _ = tools.register(tool_ctx, create)?;
    let _ = tools.register(tool_ctx, list)?;
    tools.register(tool_ctx, delete)
}

#[allow(clippy::too_many_lines)]
fn schedule_create_definition(
    root_ctx: &Context,
    agent: Arc<Agent>,
    on_durable_change: Arc<dyn Fn() + Send + Sync>,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let root_ctx = root_ctx.clone();
    let output = DefineToolOutput::new(
        json!({"type": "object"}),
        Arc::new(render_value::<ScheduleCreateArgs, ScheduleCreateValue>),
    );
    define_tool(DefineToolOptions::new(
        "schedule_create",
        "Create one reminder in the current session.",
        json!({
            "prompt": {"type": "string", "required": true, "description": "Reminder content to present when the target becomes due."},
            "after_seconds": {"type": "number", "description": "Positive safe-integer delay in seconds."},
            "every_seconds": {"type": "number", "description": "Fixed-rate safe-integer interval in seconds, at least 300."},
            "at": {"description": "Absolute target as strict offset RFC 3339 or local date/time with an explicit IANA zone.", "oneOf": [{"type": "string"}, {"type": "object", "additionalProperties": false, "properties": {"date": {"type": "string", "required": true}, "time": {"type": "string", "required": true}, "time_zone": {"type": "string", "required": true}}}]}
        }),
        output,
        Arc::new(move |args: ScheduleCreateArgs, execution| {
            let agent = agent.clone();
            let root_ctx = root_ctx.clone();
            let on_durable_change = on_durable_change.clone();
            Box::pin(async move {
                if execution.agent.as_ref().is_none_or(|a| !Arc::ptr_eq(a, &agent)) {
                    return Ok(ScheduleCreateValue::Error(internal_error()));
                }
                if let Some(invalid) = validate_create_args(&args) {
                    return Ok(ScheduleCreateValue::Error(invalid));
                }
                let signal = execution.signal();
                Ok(run_schedule_transaction(agent.clone(), move || {
                    let agent = agent.clone();
                    let root_ctx = root_ctx.clone();
                    let on_durable_change = on_durable_change.clone();
                    let args = args.clone();
                    let signal = signal.clone();
                    Box::pin(async move {
                        if let Some(cancelled) = cancellation_placeholder(&signal) {
                            return ScheduleCreateValue::Error(cancelled);
                        }
                        if preflight(&root_ctx, &agent, SchedulePersistenceOperation::Create, None)
                            .await
                            .is_some()
                        {
                            return ScheduleCreateValue::Error(persistence_error(
                                SchedulePersistenceOperation::Create,
                                None,
                            ));
                        }
                        notify(&root_ctx, &on_durable_change);
                        let folded = match fold_for_tool(&agent) {
                            Ok(folded) => folded,
                            Err(error) => return ScheduleCreateValue::Error(error),
                        };
                        let id = allocate_schedule_id(&folded);
                        let record: ScheduleRecord = if let Some(at) = &args.at {
                            match create_at_schedule_record(id.clone(), &args.prompt, at, now_millis()) {
                                Ok(record) => ScheduleRecord::At(record),
                                Err(error) => return ScheduleCreateValue::Error(input_error(&error)),
                            }
                        } else if let Some(after_seconds) = args.after_seconds {
                            match create_after_schedule_record(id.clone(), &args.prompt, after_seconds, now_millis()) {
                                Ok(record) => ScheduleRecord::After(record),
                                Err(error) => return ScheduleCreateValue::Error(input_error(&error)),
                            }
                        } else {
                            match create_every_schedule_record(
                                id.clone(),
                                &args.prompt,
                                args.every_seconds.expect("validated selector"),
                                now_millis(),
                            ) {
                                Ok(record) => ScheduleRecord::Every(record),
                                Err(error) => return ScheduleCreateValue::Error(input_error(&error)),
                            }
                        };
                        if let Some(cancelled) = cancellation_placeholder(&signal) {
                            return ScheduleCreateValue::Error(cancelled);
                        }
                        if agent
                            .session()
                            .append(
                                "schedule/change",
                                json!({"version": 1, "operation": "create", "schedule": record}),
                                seekdeep_core::session::AppendOptions::default(),
                            )
                            .is_err()
                        {
                            return ScheduleCreateValue::Error(internal_error());
                        }
                        if preflight(&root_ctx, &agent, SchedulePersistenceOperation::Create, Some(&id))
                            .await
                            .is_some()
                        {
                            return ScheduleCreateValue::Error(persistence_error(
                                SchedulePersistenceOperation::Create,
                                Some(&id),
                            ));
                        }
                        notify(&root_ctx, &on_durable_change);
                        ScheduleCreateValue::View(schedule_view(&record, now_millis()))
                    })
                })
                .await)
            })
        }),
    )
    .present_call(Arc::new(|args: &ScheduleCreateArgs| {
        Some(present("Create reminder", ToolCallKind::Other, Some(&args.prompt)))
    }))
)
}

fn schedule_list_definition(
    root_ctx: &Context,
    agent: Arc<Agent>,
    on_durable_change: Arc<dyn Fn() + Send + Sync>,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let root_ctx = root_ctx.clone();
    let output = DefineToolOutput::new(
        json!({"type": "object"}),
        Arc::new(render_value::<(), ScheduleListValue>),
    );
    define_tool(
        DefineToolOptions::new(
            "schedule_list",
            "List every active reminder in the current session in creation order.",
            json!({}),
            output,
            Arc::new(move |_args: (), execution| {
                let agent = agent.clone();
                let root_ctx = root_ctx.clone();
                let on_durable_change = on_durable_change.clone();
                Box::pin(async move {
                    if execution
                        .agent
                        .as_ref()
                        .is_none_or(|a| !Arc::ptr_eq(a, &agent))
                    {
                        return Ok(ScheduleListValue::Error(internal_error()));
                    }
                    let signal = execution.signal();
                    Ok(run_schedule_transaction(agent.clone(), move || {
                        let agent = agent.clone();
                        let root_ctx = root_ctx.clone();
                        let on_durable_change = on_durable_change.clone();
                        let signal = signal.clone();
                        Box::pin(async move {
                            if let Some(cancelled) = cancellation_placeholder(&signal) {
                                return ScheduleListValue::Error(cancelled);
                            }
                            if preflight(
                                &root_ctx,
                                &agent,
                                SchedulePersistenceOperation::List,
                                None,
                            )
                            .await
                            .is_some()
                            {
                                return ScheduleListValue::Error(persistence_error(
                                    SchedulePersistenceOperation::List,
                                    None,
                                ));
                            }
                            notify(&root_ctx, &on_durable_change);
                            let folded = match fold_for_tool(&agent) {
                                Ok(folded) => folded,
                                Err(error) => return ScheduleListValue::Error(error),
                            };
                            let now = now_millis();
                            ScheduleListValue::Views(
                                folded
                                    .active
                                    .iter()
                                    .map(|record| schedule_view(record, now))
                                    .collect(),
                            )
                        })
                    })
                    .await)
                })
            }),
        )
        .present_call(Arc::new(|_args: &()| {
            Some(present("List reminders", ToolCallKind::Read, None))
        })),
    )
}

#[allow(clippy::too_many_lines)]
fn schedule_delete_definition(
    root_ctx: &Context,
    agent: Arc<Agent>,
    on_durable_change: Arc<dyn Fn() + Send + Sync>,
) -> anyhow::Result<seekdeep_tools::ToolDefinition> {
    let root_ctx = root_ctx.clone();
    let output = DefineToolOutput::new(
        json!({"type": "object"}),
        Arc::new(render_value::<ScheduleDeleteArgs, ScheduleDeleteValue>),
    );
    define_tool(
        DefineToolOptions::new(
            "schedule_delete",
            "Delete one active reminder in the current session by its exact id.",
            json!({"id": {"type": "string", "required": true, "description": "Exact session-local schedule id."}}),
            output,
            Arc::new(move |args: ScheduleDeleteArgs, execution| {
                let agent = agent.clone();
                let root_ctx = root_ctx.clone();
                let on_durable_change = on_durable_change.clone();
                Box::pin(async move {
                    if args.id.is_empty() || args.id.trim() != args.id {
                        return Ok(ScheduleDeleteValue::Error(ScheduleToolError::InvalidRule {
                            message: "schedule_delete id must be non-empty without surrounding whitespace."
                                .to_owned(),
                        }));
                    }
                    let id = ScheduleId::new(&args.id);
                    if execution.agent.as_ref().is_none_or(|a| !Arc::ptr_eq(a, &agent)) {
                        return Ok(ScheduleDeleteValue::Error(internal_error()));
                    }
                    let signal = execution.signal();
                    Ok(run_schedule_transaction(agent.clone(), move || {
                        let agent = agent.clone();
                        let root_ctx = root_ctx.clone();
                        let on_durable_change = on_durable_change.clone();
                        let signal = signal.clone();
                        let id = id.clone();
                        Box::pin(async move {
                            if let Some(cancelled) = cancellation_placeholder(&signal) {
                                return ScheduleDeleteValue::Error(cancelled);
                            }
                            if preflight(&root_ctx, &agent, SchedulePersistenceOperation::Delete, Some(&id))
                                .await
                                .is_some()
                            {
                                return ScheduleDeleteValue::Error(persistence_error(
                                    SchedulePersistenceOperation::Delete,
                                    Some(&id),
                                ));
                            }
                            notify(&root_ctx, &on_durable_change);
                            let folded = match fold_for_tool(&agent) {
                                Ok(folded) => folded,
                                Err(error) => return ScheduleDeleteValue::Error(error),
                            };
                            if !folded.active.iter().any(|record| match record {
                                ScheduleRecord::After(record) => record.id == id,
                                ScheduleRecord::At(record) => record.id == id,
                                ScheduleRecord::Every(record) => record.id == id,
                            }) {
                                return ScheduleDeleteValue::Result(ScheduleDeleteResult {
                                    id,
                                    deleted: false,
                                    code: Some("schedule_not_found".to_owned()),
                                });
                            }
                            if let Some(cancelled) = cancellation_placeholder(&signal) {
                                return ScheduleDeleteValue::Error(cancelled);
                            }
                            if agent
                                .session()
                                .append(
                                    "schedule/change",
                                    json!({"version": 1, "operation": "delete", "id": id.as_str()}),
                                    seekdeep_core::session::AppendOptions::default(),
                                )
                                .is_err()
                            {
                                return ScheduleDeleteValue::Error(internal_error());
                            }
                            if preflight(&root_ctx, &agent, SchedulePersistenceOperation::Delete, Some(&id))
                                .await
                                .is_some()
                            {
                                return ScheduleDeleteValue::Error(persistence_error(
                                    SchedulePersistenceOperation::Delete,
                                    Some(&id),
                                ));
                            }
                            notify(&root_ctx, &on_durable_change);
                            ScheduleDeleteValue::Result(ScheduleDeleteResult {
                                id,
                                deleted: true,
                                code: None,
                            })
                        })
                    })
                    .await)
                })
            }),
        )
        .present_call(Arc::new(|args: &ScheduleDeleteArgs| {
            Some(present("Delete reminder", ToolCallKind::Other, Some(&args.id)))
        }))
    )
}

fn now_millis() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis()),
    )
    .unwrap_or(i64::MAX)
}
