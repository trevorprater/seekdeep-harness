//! Lifecycle-edge publication for subagent runs.

use std::sync::Arc;

use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::session::SessionEvent;
use seekdeep_scope::scope_target;
use uuid::Uuid;

use crate::assistant_output::final_assistant_output;
use crate::types::{
    SubagentRun, SubagentRunEndInfo, SubagentRunId, SubagentRunInfo, SubagentStopReason,
};

/// Emits one subagent lifecycle edge with per-listener containment.
#[allow(clippy::needless_pass_by_value)]
pub fn emit_subagent_lifecycle(
    context: &Context,
    name: &str,
    args: EventArgs,
    parent: Option<&Agent>,
) {
    let dispatch = parent.map_or_else(
        || context.clone(),
        |parent| scope_target(context, Some(parent.scope_key())),
    );
    match context.events().prepare_emit(&dispatch, name, &args) {
        Ok(emission) => emission.emit_contained(|error| {
            tracing::warn!(event = name, %error, "subagent listener failed");
        }),
        Err(error) => tracing::warn!(event = name, %error, "subagent dispatch failed"),
    }
}

/// Wraps a one-shot run with its start/end lifecycle pair.
pub fn observe_run(
    context: &Context,
    provider: &str,
    parent: &Arc<Agent>,
    run: Arc<dyn SubagentRun>,
) -> Arc<dyn SubagentRun> {
    let identity = SubagentRunInfo {
        run_id: SubagentRunId::new(Uuid::new_v4().to_string()),
        provider: provider.to_owned(),
        id: run.id().clone(),
        local: run.local_agent().is_some(),
    };
    let ctx = context.clone();
    let identity_end = identity.clone();
    let parent_end = Arc::clone(parent);
    let run_end = Arc::clone(&run);
    tokio::spawn(async move {
        let result = run_end.result().await;
        let info = SubagentRunEndInfo {
            run_id: identity_end.run_id,
            provider: identity_end.provider,
            id: identity_end.id,
            local: identity_end.local,
            stop_reason: result.stop_reason,
            last_assistant_message: if result.output.is_empty() {
                None
            } else {
                Some(result.output)
            },
        };
        emit_subagent_lifecycle(
            &ctx,
            "subagent/end",
            EventArgs::one(info),
            Some(&parent_end),
        );
    });
    emit_subagent_lifecycle(
        context,
        "subagent/start",
        EventArgs::one(identity),
        Some(parent),
    );
    run
}

/// Derives one epoch's terminal stop reason from consumed work.
#[must_use]
pub fn epoch_stop_reason(events: &[SessionEvent]) -> SubagentStopReason {
    let consumed = seekdeep_agent::fold_consumed_work(events);
    let kind = consumed.end.as_ref().and_then(|event| {
        event
            .data
            .get("reason")
            .and_then(|reason| reason.get("kind"))
            .and_then(|kind| kind.as_str())
    });
    match kind {
        Some("max-tokens") => SubagentStopReason::MaxTokens,
        Some("aborted" | "interrupted") => SubagentStopReason::Aborted,
        Some("blocked") => SubagentStopReason::Refusal,
        None | Some("completed") => {
            if consumed.dropped_unrun {
                SubagentStopReason::Aborted
            } else {
                SubagentStopReason::Completed
            }
        }
        Some(_) => SubagentStopReason::Error,
    }
}

/// Selects one epoch's final assistant content from its own suffix.
#[must_use]
pub fn epoch_output(events: &[SessionEvent]) -> Option<Vec<seekdeep_llm::ContentBlock>> {
    final_assistant_output(events)
}
