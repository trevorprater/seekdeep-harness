use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationBoundaryStatus,
    ConversationLocation, ConversationMatch, ConversationMatchResult, ConversationMatchRole,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
                let retry_id = event.data.get("retryId").and_then(Value::as_str);
                Ok(retry_id
                    .filter(|id| !id.is_empty())
                    .map(|id| ConversationMatchResult {
                        id: id.to_owned(),
                        role: if event.event_type == "llm/retry"
                            && event.data.get("retry").and_then(Value::as_u64) == Some(1)
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
                let retry = accepted.event.data.get("retry").cloned();
                for attempt in &mut state.attempts {
                    if retry.is_some() && attempt.get("retry") == retry.as_ref() {
                        object_mut(attempt)?.insert("retryState".to_owned(), json!("started"));
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
                .get("retryState")
                .and_then(Value::as_str)
                == Some("scheduled")
                && is_closed(&context_location(context))
            {
                object_mut(&mut attempts[last_index])?
                    .insert("retryState".to_owned(), json!("cancelled"));
            }
            let current = attempts[last_index].clone();
            let anchor = attempts[0]
                .get("seq")
                .and_then(Value::as_u64)
                .or_else(|| current.get("seq").and_then(Value::as_u64))
                .unwrap_or(0);
            Ok(Some(chat_node(
                context,
                MODEL_RETRY_KIND,
                sequence_anchor(anchor),
                json!({"attempts": attempts, "current": current}),
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
        .as_object()
        .ok_or_else(|| ConversationAssemblerError::new("llm/retry data must be an object"))?;
    let mut node = Map::from_iter([
        ("kind".to_owned(), json!(MODEL_RETRY_KIND)),
        ("seq".to_owned(), json!(accepted.event.seq)),
        ("time".to_owned(), json!(accepted.event.time)),
        ("retryState".to_owned(), json!("scheduled")),
    ]);
    node.extend(data.clone());
    Ok(Some(Value::Object(node)))
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

fn object_mut(value: &mut Value) -> Result<&mut Map<String, Value>, ConversationAssemblerError> {
    value
        .as_object_mut()
        .ok_or_else(|| ConversationAssemblerError::new("model-retry attempt must be an object"))
}

fn required_u64(value: &Value, key: &str) -> Result<u64, ConversationAssemblerError> {
    value.get(key).and_then(Value::as_u64).ok_or_else(|| {
        ConversationAssemblerError::new(format!("model-retry attempt omitted {key}"))
    })
}

fn decode(value: &Value) -> Result<RetryState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn encode(value: &RetryState) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
