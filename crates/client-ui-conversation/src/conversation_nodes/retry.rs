use super::json_value;

use std::rc::Rc;

use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationBoundaryStatus,
    ConversationLocation, ConversationMatch, ConversationMatchResult, ConversationMatchRole,
};
use seekdeep_lossless_json::JsonString;
use serde::{Deserialize, Serialize};

use super::{chat_node, context_location, sequence_anchor};

/// Producer-correlated model retry definition kind.
pub const MODEL_RETRY_KIND: &str = "model-retry";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RetryState {
    turn: u64,
    step: u64,
    attempts: Vec<Value>,
}

/// Builds the producer-correlated model-retry chain definition.
#[must_use]
pub fn conversation_retry_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: MODEL_RETRY_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| match event.event_type.as_str() {
            "llm/retry" | "llm/retry-started" => {
                let retry_id = event.data.get_value("retryId").and_then(Value::as_str);
                Ok(retry_id
                    .filter(|id| !id.is_empty())
                    .map(|id| ConversationMatchResult {
                        id: id.to_owned(),
                        role: if event.event_type == "llm/retry"
                            && event.data.get_value("retry").and_then(Value::as_u64) == Some(1)
                        {
                            ConversationMatchRole::Start
                        } else {
                            ConversationMatchRole::Update
                        },
                    }))
            }
            _ => Ok(None),
        }),
        start: Rc::new(|_context, accepted, _reader| {
            let node = scheduled_node(accepted)?.ok_or_else(|| {
                ConversationAssemblerError::new(
                    "model-retry start requires a valid llm/retry event",
                )
            })?;
            let state = RetryState {
                turn: required_u64(&node, "turn")?,
                step: required_u64(&node, "step")?,
                attempts: vec![node],
            };
            encode(&state).map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let Some(state) = context.state.as_deref() else {
                return Err(ConversationAssemblerError::new(
                    "model-retry update requires state",
                ));
            };
            let mut state = decode(state)?;
            if accepted.event.event_type == "llm/retry" {
                if let Some(node) = scheduled_node(accepted)? {
                    state.attempts.push(node);
                }
            } else if accepted.event.event_type == "llm/retry-started" {
                let retry = accepted.event.data.get_value("retry").cloned();
                for attempt in &mut state.attempts {
                    if retry.is_some() && attempt.get_value("retry") == retry.as_ref() {
                        set_retry_state(attempt, "started")?;
                    }
                }
            }
            encode(&state).map(Some)
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let state = decode(state)?;
            if state.attempts.is_empty() {
                return Ok(None);
            }
            let mut attempts = state.attempts;
            let last_index = attempts.len() - 1;
            if attempts[last_index]
                .get_value("retryState")
                .and_then(Value::as_str)
                == Some("scheduled")
                && is_closed(&context_location(context))
            {
                set_retry_state(&mut attempts[last_index], "cancelled")?;
            }
            let current = attempts[last_index].clone();
            let anchor = attempts[0]
                .get_value("seq")
                .and_then(Value::as_u64)
                .or_else(|| current.get_value("seq").and_then(Value::as_u64))
                .unwrap_or(0);
            Ok(Some(chat_node(
                context,
                MODEL_RETRY_KIND,
                sequence_anchor(anchor),
                Value::object([
                    ("attempts", json_value(&attempts)),
                    ("current", json_value(&current)),
                ]),
            )))
        })),
    }
}

fn scheduled_node(
    accepted: &ConversationMatch,
) -> Result<Option<Value>, ConversationAssemblerError> {
    if accepted.event.event_type != "llm/retry" {
        return Ok(None);
    }
    let data = accepted
        .event
        .data
        .object_entries()
        .ok_or_else(|| ConversationAssemblerError::new("llm/retry data must be an object"))?;
    let mut node = Vec::from([
        (JsonString::from("kind"), json_value(&MODEL_RETRY_KIND)),
        (JsonString::from("seq"), json_value(&accepted.event.seq)),
        (JsonString::from("time"), json_value(&accepted.event.time)),
        (JsonString::from("retryState"), json_value(&"scheduled")),
    ]);
    node.extend(data.into_iter().map(|(key, value)| {
        (
            JsonString::from_utf16(&key.to_utf16().expect("JSON object keys are strings")),
            value.to_owned(),
        )
    }));
    Ok(Some(Value::object(node)))
}

fn is_closed(location: &ConversationLocation) -> bool {
    match location {
        ConversationLocation::Step { turn, step } => {
            step.status == ConversationBoundaryStatus::Closed
                || turn.status == ConversationBoundaryStatus::Closed
        }
        ConversationLocation::Turn { turn } => turn.status == ConversationBoundaryStatus::Closed,
        ConversationLocation::Session | ConversationLocation::Unresolved => false,
    }
}

fn set_retry_state(value: &mut Value, state: &str) -> Result<(), ConversationAssemblerError> {
    if !value.is_object() {
        return Err(ConversationAssemblerError::new(
            "model-retry attempt must be an object",
        ));
    }
    value
        .insert("retryState", json_value(state))
        .map(|_| ())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn required_u64(value: &Value, key: &str) -> Result<u64, ConversationAssemblerError> {
    value.get_value(key).and_then(Value::as_u64).ok_or_else(|| {
        ConversationAssemblerError::new(format!("model-retry attempt omitted {key}"))
    })
}

fn decode(value: &Value) -> Result<RetryState, ConversationAssemblerError> {
    value
        .deserialize()
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn encode(value: &RetryState) -> Result<Rc<Value>, ConversationAssemblerError> {
    Value::from_serialize(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
