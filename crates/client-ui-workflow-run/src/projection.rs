//! Workflow event folding into one durable keyed Chat projection.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ChatConversationViewMetadata, ConversationAssemblerError,
    ConversationBoundaryStatus, ConversationLocation, ConversationLocationEvent,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext, ConversationViewNode,
    ConversationVisibility,
};
use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Status shown for a workflow, phase, or member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowRunStatus {
    /// Execution is live.
    Running,
    /// Execution settled normally.
    Completed,
    /// Execution failed.
    Failed,
    /// Execution was explicitly cancelled.
    Cancelled,
    /// The owning location closed without a durable terminal fact.
    Interrupted,
}

/// Final renderer data for one member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunMemberData {
    /// Member sequence within the run.
    pub seq: u64,
    /// Model-authored display label.
    pub label: String,
    /// Child Session identity.
    pub child_id: SessionId,
    /// Current member status.
    pub status: WorkflowRunStatus,
}

/// Final renderer data for one exact phase identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunPhaseData {
    /// Collision-free renderer identity.
    pub key: String,
    /// Exact phase, preserving absent versus empty.
    pub phase: Option<String>,
    /// Members in start-event order.
    pub members: Vec<WorkflowRunMemberData>,
}

/// Final keyed Chat payload for one workflow run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunChatData {
    /// Run display name.
    pub name: String,
    /// Current run status.
    pub status: WorkflowRunStatus,
    /// Phase groups in first-member order.
    pub phases: Vec<WorkflowRunPhaseData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowMemberState {
    seq: u64,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    child_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<WorkflowAgentOutcomeWire>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowState {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_reason: Option<WorkflowStopReasonWire>,
    members: Vec<WorkflowMemberState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WorkflowStopReasonWire {
    Completed,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WorkflowAgentOutcomeWire {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunStartData {
    #[serde(rename = "runId")]
    _run_id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentStartData {
    #[serde(rename = "runId")]
    _run_id: String,
    seq: u64,
    label: String,
    #[serde(default)]
    phase: Option<String>,
    child_id: SessionId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentEndData {
    #[serde(rename = "runId")]
    _run_id: String,
    seq: u64,
    outcome: WorkflowAgentOutcomeWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunEndData {
    #[serde(rename = "runId")]
    _run_id: String,
    stop_reason: WorkflowStopReasonWire,
}

/// Builds a collision-free phase key using JavaScript UTF-16 string length.
#[must_use]
pub fn workflow_phase_key(phase: Option<&str>) -> String {
    phase.map_or_else(
        || "missing".to_owned(),
        |phase| format!("value:{}:{phase}", phase.encode_utf16().count()),
    )
}

fn status_from_stop_reason(stop_reason: WorkflowStopReasonWire) -> WorkflowRunStatus {
    match stop_reason {
        WorkflowStopReasonWire::Completed => WorkflowRunStatus::Completed,
        WorkflowStopReasonWire::Cancelled => WorkflowRunStatus::Cancelled,
        WorkflowStopReasonWire::Error => WorkflowRunStatus::Failed,
    }
}

fn status_from_outcome(outcome: WorkflowAgentOutcomeWire) -> WorkflowRunStatus {
    match outcome {
        WorkflowAgentOutcomeWire::Completed => WorkflowRunStatus::Completed,
        WorkflowAgentOutcomeWire::Cancelled => WorkflowRunStatus::Cancelled,
        WorkflowAgentOutcomeWire::Failed => WorkflowRunStatus::Failed,
    }
}

fn location_closed(location: &ConversationLocation) -> bool {
    match location {
        ConversationLocation::Step { turn, step } => {
            step.status == ConversationBoundaryStatus::Closed
                || turn.status == ConversationBoundaryStatus::Closed
        }
        ConversationLocation::Turn { turn } => turn.status == ConversationBoundaryStatus::Closed,
        ConversationLocation::Session | ConversationLocation::Unresolved => false,
    }
}

fn project_workflow(state: &WorkflowState, location: &ConversationLocation) -> WorkflowRunChatData {
    let interrupted = state.stop_reason.is_none() && location_closed(location);
    let mut phases = Vec::<WorkflowRunPhaseData>::new();
    for member in &state.members {
        let key = workflow_phase_key(member.phase.as_deref());
        let group = phases.iter_mut().find(|phase| phase.key == key);
        let group = if let Some(group) = group {
            group
        } else {
            phases.push(WorkflowRunPhaseData {
                key,
                phase: member.phase.clone(),
                members: Vec::new(),
            });
            phases.last_mut().expect("just pushed phase")
        };
        group.members.push(WorkflowRunMemberData {
            seq: member.seq,
            label: member.label.clone(),
            child_id: member.child_id.clone(),
            status: member.outcome.map_or_else(
                || {
                    if interrupted {
                        WorkflowRunStatus::Interrupted
                    } else {
                        WorkflowRunStatus::Running
                    }
                },
                status_from_outcome,
            ),
        });
    }
    WorkflowRunChatData {
        name: state.name.clone(),
        status: state.stop_reason.map_or_else(
            || {
                if interrupted {
                    WorkflowRunStatus::Interrupted
                } else {
                    WorkflowRunStatus::Running
                }
            },
            status_from_stop_reason,
        ),
        phases,
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    value: Value,
    event_type: &str,
) -> Result<T, ConversationAssemblerError> {
    serde_json::from_value(value).map_err(|error| {
        ConversationAssemblerError::new(format!("invalid {event_type} data: {error}"))
    })
}

fn state_of(
    context: &ConversationNodeContext,
) -> Result<WorkflowState, ConversationAssemblerError> {
    let state = context.state.as_deref().ok_or_else(|| {
        ConversationAssemblerError::new("workflow-run update requires initialized state")
    })?;
    decode(state.clone(), "workflow-run state")
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value).map(Rc::new).map_err(|error| {
        ConversationAssemblerError::new(format!("workflow-run serialization failed: {error}"))
    })
}

fn run_id(event: &ConversationLocationEvent) -> Result<String, ConversationAssemblerError> {
    event
        .data
        .get("runId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ConversationAssemblerError::new(format!("{} requires string runId", event.event_type))
        })
}

fn event_anchor(seq: u64) -> f64 {
    assert!(
        seq <= MAX_SAFE_INTEGER,
        "Conversation event sequence exceeds JavaScript safe-integer range"
    );
    #[allow(clippy::cast_precision_loss)]
    {
        seq as f64
    }
}

/// Builds the workflow-run Definition consumed by the Rust Conversation assembler.
#[must_use]
pub fn workflow_run_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "workflow-run".to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            let role = match event.event_type.as_str() {
                "tool-workflow/run-start" => ConversationMatchRole::Start,
                "tool-workflow/agent-start"
                | "tool-workflow/agent-end"
                | "tool-workflow/run-end" => ConversationMatchRole::Update,
                _ => return Ok(None),
            };
            Ok(Some(ConversationMatchResult {
                id: run_id(event)?,
                role,
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "tool-workflow/run-start" {
                return Err(ConversationAssemblerError::new(
                    "workflow-run start requires tool-workflow/run-start",
                ));
            }
            let data: RunStartData =
                decode(accepted.event.data.clone(), "tool-workflow/run-start")?;
            encode(&WorkflowState {
                name: data.name,
                stop_reason: None,
                members: Vec::new(),
            })
            .map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let mut state = state_of(context)?;
            match accepted.event.event_type.as_str() {
                "tool-workflow/agent-start" => {
                    let data: AgentStartData =
                        decode(accepted.event.data.clone(), "tool-workflow/agent-start")?;
                    state.members.push(WorkflowMemberState {
                        seq: data.seq,
                        label: data.label,
                        phase: data.phase,
                        child_id: data.child_id,
                        outcome: None,
                    });
                }
                "tool-workflow/agent-end" => {
                    let data: AgentEndData =
                        decode(accepted.event.data.clone(), "tool-workflow/agent-end")?;
                    for member in &mut state.members {
                        if member.seq == data.seq {
                            member.outcome = Some(data.outcome);
                        }
                    }
                }
                "tool-workflow/run-end" => {
                    let data: RunEndData =
                        decode(accepted.event.data.clone(), "tool-workflow/run-end")?;
                    state.stop_reason = Some(data.stop_reason);
                }
                _ => return Ok(context.state.clone()),
            }
            encode(&state).map(Some)
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(start) = context.start.as_ref() else {
                return Ok(None);
            };
            let state = state_of(context)?;
            let data = encode(&project_workflow(&state, &start.location))?;
            Ok(Some(Rc::new(ConversationViewNode {
                key: context.key.clone(),
                kind: "workflow-run".to_owned(),
                id: context.id.clone(),
                target: "chat".to_owned(),
                data,
                chat: Some(ChatConversationViewMetadata {
                    anchor_seq: event_anchor(start.event.seq),
                    location: start.location.clone(),
                    visibility: ConversationVisibility::Visible,
                }),
            })))
        })),
    }
}
