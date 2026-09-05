//! Turn-scoped successful-mutation accumulator and Location-data publisher.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocationData,
    ConversationLocationDataScope, ConversationLocationEvent, ConversationMatchResult,
    ConversationMatchRole, ConversationNodeContext,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One produced path and the settlement sequence that made it visible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducedPath {
    /// Successful Tool settlement sequence.
    pub seq: u64,
    /// Follow-along file path.
    pub path: String,
}

/// Immutable produced-file facts published against one Turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverablesTurnData {
    /// Successful mutation locations in settlement order.
    pub produced: Vec<ProducedPath>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliverablesState {
    turn: u64,
    calls: IndexMap<String, Option<Value>>,
    produced: Vec<ProducedPath>,
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value).map(Rc::new).map_err(|error| {
        ConversationAssemblerError::new(format!("deliverables serialization failed: {error}"))
    })
}

fn decode<T: serde::de::DeserializeOwned>(
    value: Value,
    owner: &str,
) -> Result<T, ConversationAssemblerError> {
    serde_json::from_value(value)
        .map_err(|error| ConversationAssemblerError::new(format!("invalid {owner}: {error}")))
}

fn state_of(
    context: &ConversationNodeContext,
) -> Result<DeliverablesState, ConversationAssemblerError> {
    let state = context.state.as_deref().ok_or_else(|| {
        ConversationAssemblerError::new("deliverables update requires initialized state")
    })?;
    decode(state.clone(), "deliverables state")
}

fn value_string(value: Option<&Value>, owner: &str) -> Result<String, ConversationAssemblerError> {
    let value =
        value.ok_or_else(|| ConversationAssemblerError::new(format!("{owner} is missing")))?;
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null => Ok("null".to_owned()),
        Value::Array(_) => Ok(value.to_string()),
        Value::Object(_) => Ok("[object Object]".to_owned()),
    }
}

fn is_append_result(event: &ConversationLocationEvent) -> bool {
    event.event_type == "tool/result"
        && event
            .wire
            .as_ref()
            .and_then(|wire| wire.get("surfaceOp"))
            .and_then(Value::as_str)
            == Some("append")
}

fn produced_paths(view: Option<&Value>) -> Result<Vec<String>, ConversationAssemblerError> {
    let Some(view) = view.and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mutation = view.get("card").and_then(Value::as_str) == Some("diff")
        || (view.get("card").and_then(Value::as_str) == Some("generic")
            && view.get("kind").and_then(Value::as_str) == Some("edit"));
    if !mutation {
        return Ok(Vec::new());
    }
    let Some(locations) = view.get("locations") else {
        return Ok(Vec::new());
    };
    if locations.is_null() {
        return Ok(Vec::new());
    }
    locations
        .as_array()
        .ok_or_else(|| {
            ConversationAssemblerError::new("deliverables mutation locations must be an array")
        })?
        .iter()
        .map(|location| {
            location
                .get("path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ConversationAssemblerError::new(
                        "deliverables mutation location path must be a string",
                    )
                })
        })
        .collect()
}

fn result_error(event: &ConversationLocationEvent) -> Result<bool, ConversationAssemblerError> {
    event
        .data
        .pointer("/message/content/0/isError")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            ConversationAssemblerError::new("tool/result is missing message.content[0].isError")
        })
}

fn result_call_id(event: &ConversationLocationEvent) -> Result<String, ConversationAssemblerError> {
    value_string(
        event.data.pointer("/message/source/callId"),
        "tool/result message.source.callId",
    )
}

fn call_view(accepted: &seekdeep_client_runtime::ConversationMatch) -> Option<Value> {
    accepted
        .view
        .as_ref()
        .filter(|view| view.get("for").and_then(Value::as_str) == Some("call"))
        .and_then(|view| view.get("view"))
        .cloned()
}

/// Builds the state-only deliverables Definition consumed by the Rust assembler.
#[must_use]
pub fn deliverables_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "deliverables".to_owned(),
        target: None,
        match_event: Rc::new(|event| {
            let role = match event.event_type.as_str() {
                "turn/start" => ConversationMatchRole::Start,
                "tool/call" => ConversationMatchRole::Update,
                "tool/result" if is_append_result(event) => ConversationMatchRole::Update,
                _ => return Ok(None),
            };
            Ok(Some(ConversationMatchResult {
                id: value_string(event.data.get("turn"), "deliverables turn")?,
                role,
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "turn/start" {
                return Err(ConversationAssemblerError::new(
                    "deliverables start requires turn/start",
                ));
            }
            let turn = accepted
                .event
                .data
                .get("turn")
                .and_then(Value::as_u64)
                .ok_or_else(|| ConversationAssemblerError::new("turn/start requires safe turn"))?;
            encode(&DeliverablesState {
                turn,
                calls: IndexMap::new(),
                produced: Vec::new(),
            })
            .map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let mut state = state_of(context)?;
            if accepted.event.event_type == "tool/call" {
                let call_id = value_string(accepted.event.data.get("callId"), "tool/call callId")?;
                state.calls.insert(call_id, call_view(accepted));
                return encode(&state).map(Some);
            }
            if accepted.event.event_type != "tool/result" {
                return Ok(context.state.clone());
            }
            if result_error(&accepted.event)? {
                return Ok(context.state.clone());
            }
            let call_id = result_call_id(&accepted.event)?;
            let additions = produced_paths(state.calls.get(&call_id).and_then(Option::as_ref))?;
            if additions.is_empty() {
                return Ok(context.state.clone());
            }
            state
                .produced
                .extend(additions.into_iter().map(|path| ProducedPath {
                    seq: accepted.event.seq,
                    path,
                }));
            encode(&state).map(Some)
        }),
        publication: None,
        build_location_data: Some(Rc::new(|context, scope| {
            if scope != ConversationLocationDataScope::Turn || context.state.is_none() {
                return Ok(None);
            }
            let state = state_of(context)?;
            Ok(Some(Rc::new(ConversationLocationData::Turn {
                turn: state.turn,
                key: "deliverables".to_owned(),
                value: encode(&DeliverablesTurnData {
                    produced: state.produced,
                })?,
            })))
        })),
        build_view_node: None,
    }
}
