//! Package-owned workflow lifecycle invariants.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_invariants::{InvariantFailure, InvariantInstaller, InvariantRegistry};

use crate::types::{WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowResultInfo, WorkflowRunInfo};

const PACKAGE_NAME: &str = "seekdeep-workflow";

/// Per-run trace accumulated across one workflow/* lifecycle.
#[derive(Debug, Default)]
struct WorkflowTrace {
    meta: String,
    agents: HashMap<u64, WorkflowAgentInfo>,
    starts: u64,
}

/// Registers the workflow invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<seekdeep_invariants::InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(std::iter::empty::<String>(), move |context, fail| {
            Box::pin(async move {
                let traces = Arc::new(Mutex::new(HashMap::<String, WorkflowTrace>::new()));
                context.events().on_sync(
                    &context,
                    "internal/dispatch",
                    move |_, args| {
                        let Some(mode) = args.get::<DispatchMode>(0) else {
                            return Ok(EventReply::Undefined);
                        };
                        if mode.as_ref() != &DispatchMode::Emit {
                            return Ok(EventReply::Undefined);
                        }
                        let Some(name) = args.get::<String>(1) else {
                            return Ok(EventReply::Undefined);
                        };
                        let Some(dispatch_args) = args.get::<EventArgs>(2) else {
                            return Ok(EventReply::Undefined);
                        };
                        if !name.starts_with("workflow/") {
                            return Ok(EventReply::Undefined);
                        }
                        let Some(info) = dispatch_args.get::<WorkflowRunInfo>(0) else {
                            reject(&fail, "workflow event lacks its run info")?;
                            return Ok(EventReply::Undefined);
                        };
                        validate(name.as_str(), &info, dispatch_args.as_ref(), &traces, &fail)?;
                        Ok(EventReply::Undefined)
                    },
                    EventOptions {
                        global: true,
                        ..EventOptions::default()
                    },
                )?;
                Ok(())
            })
        }),
    )
}

fn reject(failure: &InvariantFailure, message: impl Into<String>) -> anyhow::Result<()> {
    Err(failure.fail(message).into())
}

#[allow(clippy::too_many_lines)]
fn validate(
    name: &str,
    info: &WorkflowRunInfo,
    args: &EventArgs,
    traces: &Arc<Mutex<HashMap<String, WorkflowTrace>>>,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    let id = info.id.as_str().to_owned();
    match name {
        "workflow/start" => {
            if id.is_empty() || info.meta.name.is_empty() || info.meta.description.is_empty() {
                reject(
                    fail,
                    "workflow/start id, meta.name, and meta.description must be non-empty",
                )?;
            }
            let mut traces = traces.lock();
            if traces.contains_key(&id) {
                reject(fail, format!("workflow/start repeated run id {id:?}"))?;
            }
            traces.insert(
                id,
                WorkflowTrace {
                    meta: serde_json::to_string(&info.meta).expect("meta is lossless JSON"),
                    agents: HashMap::new(),
                    starts: 0,
                },
            );
        }
        "workflow/phase" | "workflow/log" => {
            let mut traces = traces.lock();
            let Some(trace) = traces.get_mut(&id) else {
                reject(
                    fail,
                    format!("workflow event has no matching workflow/start for run {id:?}"),
                )?;
                return Ok(());
            };
            let meta = serde_json::to_string(&info.meta).expect("meta is lossless JSON");
            if trace.meta != meta {
                reject(
                    fail,
                    format!("workflow event meta diverges from workflow/start for run {id:?}"),
                )?;
            }
        }
        "workflow/agent-start" => {
            let Some(agent) = args.get::<WorkflowAgentInfo>(1) else {
                reject(fail, "workflow/agent-start lacks its agent")?;
                return Ok(());
            };
            if agent.seq < 1 || agent.child_id.as_str().is_empty() {
                reject(
                    fail,
                    "workflow/agent-start seq must be positive and childId must be non-empty",
                )?;
            }
            let mut traces = traces.lock();
            let Some(trace) = traces.get_mut(&id) else {
                reject(
                    fail,
                    format!("workflow event has no matching workflow/start for run {id:?}"),
                )?;
                return Ok(());
            };
            if trace.agents.contains_key(&agent.seq) {
                reject(
                    fail,
                    format!("workflow/agent-start repeated seq {}", agent.seq),
                )?;
            }
            trace.agents.insert(agent.seq, agent.as_ref().clone());
            trace.starts += 1;
        }
        "workflow/agent-end" => {
            let Some(agent) = args.get::<WorkflowAgentEndInfo>(1) else {
                reject(fail, "workflow/agent-end lacks its agent")?;
                return Ok(());
            };
            let mut traces = traces.lock();
            let Some(trace) = traces.get_mut(&id) else {
                reject(
                    fail,
                    format!("workflow event has no matching workflow/start for run {id:?}"),
                )?;
                return Ok(());
            };
            let Some(start) = trace.agents.get(&agent.info.seq) else {
                reject(
                    fail,
                    format!(
                        "workflow/agent-end has no matching start for seq {}",
                        agent.info.seq
                    ),
                )?;
                return Ok(());
            };
            if start.label != agent.info.label
                || start.phase != agent.info.phase
                || start.child_id != agent.info.child_id
            {
                reject(
                    fail,
                    format!(
                        "workflow/agent-end identity diverges from workflow/agent-start for seq {}",
                        agent.info.seq
                    ),
                )?;
            }
            trace.agents.remove(&agent.info.seq);
        }
        "workflow/end" => {
            let Some(result) = args.get::<WorkflowResultInfo>(1) else {
                reject(fail, "workflow/end lacks its result")?;
                return Ok(());
            };
            let mut traces = traces.lock();
            let Some(trace) = traces.get_mut(&id) else {
                reject(
                    fail,
                    format!("workflow event has no matching workflow/start for run {id:?}"),
                )?;
                return Ok(());
            };
            if !trace.agents.is_empty() {
                reject(
                    fail,
                    format!(
                        "workflow/end has {} agent call(s) without workflow/agent-end",
                        trace.agents.len()
                    ),
                )?;
            }
            if result.agents_started < trace.starts {
                reject(
                    fail,
                    "workflow/end agentsStarted must be a safe integer covering every observed agent start",
                )?;
            }
            let error_consistency =
                if result.stop_reason == crate::types::WorkflowStopReason::Completed {
                    result.error.is_none()
                } else {
                    result.error.is_some()
                };
            if !error_consistency {
                reject(
                    fail,
                    "workflow/end error must be absent exactly for completed runs",
                )?;
            }
            traces.remove(&id);
        }
        _ => {}
    }
    Ok(())
}
