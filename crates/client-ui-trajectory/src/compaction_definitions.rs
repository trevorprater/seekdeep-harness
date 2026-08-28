//! Trajectory compaction-request and Session-end Definitions.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocationEvent,
    ConversationMatchResult, ConversationMatchRole,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{TRAJECTORY_TARGET, trajectory_node};

/// Compaction lifecycle Definition kind.
pub const TRAJECTORY_COMPACTION_KIND: &str = "trajectory-compaction";
/// Session terminal-boundary Definition kind.
pub const TRAJECTORY_SESSION_END_KIND: &str = "trajectory-session-end";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct EventState {
    seq: u64,
    time: i64,
    #[serde(rename = "type")]
    event_type: String,
    data: Value,
}

impl From<&ConversationLocationEvent> for EventState {
    fn from(event: &ConversationLocationEvent) -> Self {
        Self {
            seq: event.seq,
            time: event.time,
            event_type: event.event_type.clone(),
            data: event.data.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CompactionState {
    start: EventState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<EventState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<EventState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<EventState>,
}

/// Builds compaction lifecycle and Session-end Definitions in source order.
#[must_use]
pub fn trajectory_compaction_definitions() -> [AssemblerNodeDefinition; 2] {
    [
        trajectory_compaction_definition(),
        trajectory_session_end_definition(),
    ]
}

fn trajectory_compaction_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_COMPACTION_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            if let Some(compaction_id) = event_compaction_id(event) {
                return Ok(Some(ConversationMatchResult {
                    id: compaction_id,
                    role: if event.event_type == "compaction/start" {
                        ConversationMatchRole::Start
                    } else {
                        ConversationMatchRole::Update
                    },
                }));
            }
            Ok(checkpoint_id(event).map(|id| ConversationMatchResult {
                id,
                role: ConversationMatchRole::Update,
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "compaction/start" {
                return Err(ConversationAssemblerError::new(
                    "trajectory-compaction start requires compaction/start",
                ));
            }
            encode(&CompactionState {
                start: EventState::from(accepted.event.as_ref()),
                summary: None,
                end: None,
                checkpoint: None,
            })
            .map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let Some(previous) = context.state.as_deref() else {
                return Ok(None);
            };
            let mut state = decode::<CompactionState>(previous)?;
            match accepted.event.event_type.as_str() {
                "compaction/summary" => {
                    state.summary = Some(EventState::from(accepted.event.as_ref()));
                }
                "compaction/end" => {
                    state.end = Some(EventState::from(accepted.event.as_ref()));
                }
                _ if checkpoint_id(&accepted.event).is_some() => {
                    state.checkpoint = Some(EventState::from(accepted.event.as_ref()));
                }
                _ => return Ok(context.state.clone()),
            }
            encode(&state).map(Some)
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let state = decode::<CompactionState>(state)?;
            let Some(request) = request_from_state(&state) else {
                return Ok(None);
            };
            Ok(Some(trajectory_node(
                context,
                state.start.seq,
                json!({"kind": "compaction", "request": request}),
            )))
        })),
    }
}

fn trajectory_session_end_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_SESSION_END_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "session/end-seed").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, accepted, _reader| {
            Ok(Some(Rc::new(json!({
                "seq": accepted.event.seq,
                "time": accepted.event.time,
            }))))
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let seq = state.get("seq").and_then(Value::as_u64).ok_or_else(|| {
                ConversationAssemblerError::new("trajectory Session-end state omitted seq")
            })?;
            let time = state.get("time").and_then(Value::as_i64).ok_or_else(|| {
                ConversationAssemblerError::new("trajectory Session-end state omitted time")
            })?;
            Ok(Some(trajectory_node(
                context,
                seq,
                json!({"kind": "session-end", "seq": seq, "time": time}),
            )))
        })),
    }
}

fn checkpoint_id(event: &ConversationLocationEvent) -> Option<String> {
    if event.event_type != "user/message" {
        return None;
    }
    let source = event.data.get("source")?.as_object()?;
    (source.get("kind").and_then(Value::as_str) == Some("plugin")
        && source.get("plugin").and_then(Value::as_str) == Some("compact"))
    .then(|| {
        source
            .get("compactionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
    })
    .flatten()
}

fn event_compaction_id(event: &ConversationLocationEvent) -> Option<String> {
    matches!(
        event.event_type.as_str(),
        "compaction/start" | "compaction/summary" | "compaction/end"
    )
    .then(|| {
        event
            .data
            .get("compactionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
    })
    .flatten()
}

fn request_from_state(state: &CompactionState) -> Option<Value> {
    if state.start.event_type != "compaction/start" {
        return None;
    }
    let mut request = Map::from_iter([
        ("purpose".to_owned(), json!("compaction")),
        ("startSeq".to_owned(), json!(state.start.seq)),
        (
            "turn".to_owned(),
            state.start.data.get("turn").cloned().unwrap_or(Value::Null),
        ),
        ("step".to_owned(), json!(0)),
        ("startedAt".to_owned(), json!(state.start.time)),
        (
            "completedAt".to_owned(),
            state
                .end
                .as_ref()
                .map_or(Value::Null, |end| json!(end.time)),
        ),
    ]);
    let ended = state
        .end
        .as_ref()
        .filter(|end| end.event_type == "compaction/end");
    let has_error = ended.is_some_and(|end| end.data.get("error").is_some());
    request.insert(
        "status".to_owned(),
        json!(if ended.is_none() {
            "running"
        } else if has_error {
            "error"
        } else {
            "complete"
        }),
    );
    if let Some(error) = ended.and_then(|end| end.data.get("error")).cloned() {
        request.insert("error".to_owned(), error);
    }
    if let Some(summary) = state
        .summary
        .as_ref()
        .filter(|summary| summary.event_type == "compaction/summary")
    {
        request.insert("resultSeq".to_owned(), json!(summary.seq));
        request.insert(
            "summary".to_owned(),
            summary.data.get("summary").cloned().unwrap_or(Value::Null),
        );
        copy_present(&mut request, &summary.data, "rawOutput");
        let provider = summary.data.get("provider").cloned().unwrap_or(Value::Null);
        let model = summary.data.get("model").cloned().unwrap_or(Value::Null);
        request.insert(
            "provenance".to_owned(),
            json!({"provider": provider, "model": model}),
        );
        let mut config = Map::from_iter([
            ("provider".to_owned(), provider),
            ("model".to_owned(), model),
            ("purpose".to_owned(), json!("compaction")),
        ]);
        copy_present(&mut config, &summary.data, "maxTokens");
        request.insert("requestConfig".to_owned(), Value::Object(config));
        copy_present(&mut request, &summary.data, "usage");
    }
    if let Some(checkpoint) = state
        .checkpoint
        .as_ref()
        .filter(|checkpoint| checkpoint.event_type == "user/message")
    {
        request.insert("replacementSeq".to_owned(), json!(checkpoint.seq));
    }
    Some(Value::Object(request))
}

fn copy_present(output: &mut Map<String, Value>, input: &Value, key: &str) {
    if let Some(value) = input.get(key) {
        output.insert(key.to_owned(), value.clone());
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
