//! Lifecycle-edge publication for subagent runs.

use std::sync::Arc;

use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_llm::ContentBlock;
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
        let (stop_reason, output) = match result {
            Ok(result) => (result.stop_reason, result.output),
            Err(error) => {
                tracing::warn!(%error, "subagent result channel failed");
                (SubagentStopReason::Error, Vec::new())
            }
        };
        let info = SubagentRunEndInfo {
            run_id: identity_end.run_id,
            provider: identity_end.provider,
            id: identity_end.id,
            local: identity_end.local,
            stop_reason,
            last_assistant_message: if output.is_empty() {
                None
            } else {
                Some(output)
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
pub fn epoch_output(events: &[SessionEvent]) -> Option<Vec<ContentBlock>> {
    final_assistant_output(events)
}

/// How one Activation's residency epoch ended, as both the terminal lifecycle
/// edge and the manager's own parent delivery report it.
#[derive(Clone, Debug)]
pub struct ActivationTerminal {
    /// Why this epoch's last ordinary turn ended, or `error` when teardown failed.
    pub stop_reason: SubagentStopReason,
    /// The epoch's final assistant content, absent when it produced none or failed.
    pub output: Option<Vec<ContentBlock>>,
}

impl Default for ActivationTerminal {
    fn default() -> Self {
        Self {
            stop_reason: SubagentStopReason::Completed,
            output: None,
        }
    }
}

/// Lifecycle observer for one continuable Activation's residency epoch, so
/// continuable children emit the same start/end pair as one-shot runs.
///
/// Package-private: the continuation manager is the only consumer, and its call
/// ordering is an in-package contract rather than a published extension point.
pub struct ActivationObserver {
    context: Context,
    identity: SubagentRunInfo,
    parent: Arc<Agent>,
    boundary: parking_lot::Mutex<usize>,
    captured: parking_lot::Mutex<ActivationTerminal>,
}

impl ActivationObserver {
    /// Publish the start edge once the epoch is resident.
    pub fn start(&self, child: &Agent) {
        *self.boundary.lock() = child.session().events().len();
        emit_subagent_lifecycle(
            &self.context,
            "subagent/start",
            EventArgs::one(self.identity.clone()),
            Some(self.parent.as_ref()),
        );
    }

    /// Snapshot the child-dependent terminal facts while the child is still
    /// registered, because handle disposal unregisters it.
    pub fn capture(&self, child: &Agent) {
        let boundary = *self.boundary.lock();
        let events = child.session().events();
        let own = &events[boundary.min(events.len())..];
        let output = epoch_output(own);
        let stop_reason = epoch_stop_reason(own);
        *self.captured.lock() = ActivationTerminal {
            stop_reason,
            output,
        };
    }

    /// Resolve the terminal facts `settle` will publish, without publishing.
    pub fn terminal(&self, failure: Option<&anyhow::Error>) -> ActivationTerminal {
        match failure {
            None => self.captured.lock().clone(),
            Some(_) => ActivationTerminal {
                stop_reason: SubagentStopReason::Error,
                output: None,
            },
        }
    }

    /// Publish the terminal edge exactly once, after the disposal outcome is known.
    pub fn settle(&self, failure: Option<&anyhow::Error>) {
        let terminal = self.terminal(failure);
        let info = SubagentRunEndInfo {
            run_id: self.identity.run_id.clone(),
            provider: self.identity.provider.clone(),
            id: self.identity.id.clone(),
            local: self.identity.local,
            stop_reason: terminal.stop_reason,
            last_assistant_message: terminal.output,
        };
        emit_subagent_lifecycle(
            &self.context,
            "subagent/end",
            EventArgs::one(info),
            Some(self.parent.as_ref()),
        );
    }
}

/// Builds the observer for one continuable Activation's residency epoch.
#[must_use]
pub fn create_activation_observer(
    context: &Context,
    provider: &str,
    child_id: &SessionId,
    parent: &Arc<Agent>,
) -> ActivationObserver {
    let identity = SubagentRunInfo {
        run_id: SubagentRunId::new(Uuid::new_v4().to_string()),
        provider: provider.to_owned(),
        id: child_id.clone(),
        local: true,
    };
    ActivationObserver {
        context: context.clone(),
        identity,
        parent: Arc::clone(parent),
        boundary: parking_lot::Mutex::new(0),
        captured: parking_lot::Mutex::new(ActivationTerminal {
            stop_reason: SubagentStopReason::Completed,
            output: None,
        }),
    }
}
