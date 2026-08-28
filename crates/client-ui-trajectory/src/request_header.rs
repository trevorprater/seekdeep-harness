//! Trajectory request-header Definition and prompt-change state.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocation,
    ConversationLocationEvent, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeContext, ConversationPromptSnapshot, ConversationViewNode,
    ConversationViewPlacement, RequestPromptChange, RequestPromptChangeKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Stable Definition kind.
pub const TRAJECTORY_REQUEST_HEADER_KIND: &str = "trajectory-request-header";
/// Stable trajectory target name.
pub const TRAJECTORY_TARGET: &str = "trajectory";

/// Coordinate-only turn face consumed by the trajectory snapshot builder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryTurnLocation {
    /// Model turn number.
    pub turn: u64,
}

/// Coordinate-only step face consumed by the trajectory snapshot builder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryStepLocation {
    /// Agent-loop step number.
    pub step: u64,
}

/// JSON-safe Location projection retained inside trajectory contributions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TrajectoryLocation {
    /// Session-wide event.
    Session,
    /// Turn-scoped event.
    Turn {
        /// Owning Turn.
        turn: TrajectoryTurnLocation,
    },
    /// Step-scoped event.
    Step {
        /// Owning Turn.
        turn: TrajectoryTurnLocation,
        /// Owning Step.
        step: TrajectoryStepLocation,
    },
    /// Referenced hierarchy is outside the current window.
    Unresolved,
}

impl From<&ConversationLocation> for TrajectoryLocation {
    fn from(location: &ConversationLocation) -> Self {
        match location {
            ConversationLocation::Session => Self::Session,
            ConversationLocation::Turn { turn } => Self::Turn {
                turn: TrajectoryTurnLocation { turn: turn.turn },
            },
            ConversationLocation::Step { turn, step } => Self::Step {
                turn: TrajectoryTurnLocation { turn: turn.turn },
                step: TrajectoryStepLocation { step: step.step },
            },
            ConversationLocation::Unresolved => Self::Unresolved,
        }
    }
}

/// Request-header facts retained by the trajectory target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrajectoryRequestHeaderState {
    /// Header event sequence.
    pub seq: u64,
    /// Header event Unix milliseconds.
    pub time: i64,
    /// Complete model-visible prompt.
    pub prompt: ConversationPromptSnapshot,
    /// Prompt delta, when this header introduces one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<RequestPromptChange>,
    /// Engine-resolved placement.
    pub location: TrajectoryLocation,
}

/// Builds the native request-header Definition.
#[must_use]
pub fn trajectory_request_header_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_REQUEST_HEADER_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "request/header").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, accepted, reader| {
            let prompt = request_prompt(&accepted.event)?;
            let previous = reader
                .previous(TRAJECTORY_REQUEST_HEADER_KIND)
                .map(|previous| decode_state(previous.state.as_ref()))
                .transpose()?;
            let change = prompt_change(
                previous.as_ref().map(|state| &state.prompt),
                &prompt,
                &accepted.event,
            );
            encode_state(&TrajectoryRequestHeaderState {
                seq: accepted.event.seq,
                time: accepted.event.time,
                prompt,
                change,
                location: TrajectoryLocation::from(&accepted.location),
            })
            .map(Some)
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let state = decode_state(state)?;
            let anchor_seq = state.seq;
            let data = json!({
                "kind": "request-header",
                "header": state,
            });
            Ok(Some(trajectory_node(context, anchor_seq, data)))
        })),
    }
}

/// Wraps one contribution in the Engine-owned trajectory target envelope.
#[must_use]
pub fn trajectory_node(
    context: &ConversationNodeContext,
    anchor_seq: u64,
    data: Value,
) -> Rc<ConversationViewNode> {
    let location = context
        .start
        .as_ref()
        .map_or(ConversationLocation::Unresolved, |start| {
            start.location.clone()
        });
    Rc::new(ConversationViewNode {
        key: context.key.clone(),
        kind: context.kind.clone(),
        id: context.id.clone(),
        target: TRAJECTORY_TARGET.to_owned(),
        data: Rc::new(data),
        placement: Some(ConversationViewPlacement {
            anchor_seq: u64_as_f64(anchor_seq),
            location,
        }),
        chat: None,
    })
}

fn request_prompt(
    event: &ConversationLocationEvent,
) -> Result<ConversationPromptSnapshot, ConversationAssemblerError> {
    if event.event_type != "request/header" {
        return Err(ConversationAssemblerError::new(
            "trajectory-request-header start requires request/header",
        ));
    }
    let header = event
        .data
        .get("header")
        .and_then(Value::as_object)
        .ok_or_else(|| ConversationAssemblerError::new("request/header omitted header"))?;
    let config = serde_json::from_value(
        header
            .get("config")
            .cloned()
            .ok_or_else(|| ConversationAssemblerError::new("request/header omitted config"))?,
    )
    .map_err(|error| ConversationAssemblerError::new(error.to_string()))?;
    let system = match header.get("system") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(system)) => system.clone(),
        Some(_) => {
            return Err(ConversationAssemblerError::new(
                "request/header system must be a string or null",
            ));
        }
    };
    let tools = header
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(ConversationPromptSnapshot {
        config,
        system,
        tools,
    })
}

fn prompt_change(
    previous: Option<&ConversationPromptSnapshot>,
    prompt: &ConversationPromptSnapshot,
    event: &ConversationLocationEvent,
) -> Option<RequestPromptChange> {
    if event.event_type != "request/header" {
        return None;
    }
    if previous.is_none() && event.data.get("reason").and_then(Value::as_str) != Some("initial") {
        return None;
    }
    let system_changed = previous.is_some_and(|previous| previous.system != prompt.system);
    let tools_changed = previous.is_some_and(|previous| {
        serde_json::to_string(&previous.tools).ok() != serde_json::to_string(&prompt.tools).ok()
    });
    if previous.is_some() && !system_changed && !tools_changed {
        return None;
    }
    Some(RequestPromptChange {
        seq: event.seq,
        time: event.time,
        kind: match previous {
            None => RequestPromptChangeKind::Initial,
            Some(_) if system_changed && tools_changed => RequestPromptChangeKind::SystemAndTools,
            Some(_) if system_changed => RequestPromptChangeKind::System,
            Some(_) => RequestPromptChangeKind::Tools,
        },
        previous: previous.cloned().map(Box::new),
    })
}

fn encode_state(
    state: &TrajectoryRequestHeaderState,
) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(state)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode_state(value: &Value) -> Result<TrajectoryRequestHeaderState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
