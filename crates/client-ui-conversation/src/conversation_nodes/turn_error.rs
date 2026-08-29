use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocation,
    ConversationLocationEvent, ConversationMatch, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeContext, ConversationViewNode, ConversationVisibility,
};
use seekdeep_failure_display::display_failure_message;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{chat_node, chat_node_with, context_location, sequence_anchor};

/// Terminal unsuperseded turn failure definition kind.
pub const TURN_ERROR_KIND: &str = "turn-error";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TurnFailure {
    seq: u64,
    time: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TurnErrorState {
    turn: u64,
    hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<TurnFailure>,
}

/// Builds the terminal turn-error definition suppressed by an owning retry chain.
#[must_use]
pub fn conversation_turn_error_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TURN_ERROR_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| Ok(match_turn_error_event(event))),
        start: Rc::new(|_context, accepted, _reader| start_turn_error(accepted)),
        update: Rc::new(update_turn_error),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(build_turn_error)),
    }
}

fn match_turn_error_event(event: &ConversationLocationEvent) -> Option<ConversationMatchResult> {
    if event.event_type == "turn/start" {
        coordinate(&event.data, "turn").map(|turn| ConversationMatchResult {
            id: turn.to_string(),
            role: ConversationMatchRole::Start,
        })
    } else if event.event_type == "turn/end"
        && event
            .data
            .get("reason")
            .and_then(|reason| reason.get("kind"))
            .and_then(Value::as_str)
            == Some("error")
    {
        coordinate(&event.data, "turn").map(|turn| ConversationMatchResult {
            id: turn.to_string(),
            role: ConversationMatchRole::Update,
        })
    } else {
        retry_turn(&event.event_type, &event.data).map(|turn| ConversationMatchResult {
            id: turn.to_string(),
            role: ConversationMatchRole::Update,
        })
    }
}

fn start_turn_error(
    accepted: &ConversationMatch,
) -> Result<Option<Rc<Value>>, ConversationAssemblerError> {
    if accepted.event.event_type != "turn/start" {
        return Err(ConversationAssemblerError::new(
            "turn-error start requires turn/start",
        ));
    }
    let state = TurnErrorState {
        turn: coordinate(&accepted.event.data, "turn")
            .ok_or_else(|| ConversationAssemblerError::new("turn/start omitted turn"))?,
        hidden: false,
        failure: None,
    };
    encode(&state).map(Some)
}

fn update_turn_error(
    context: &ConversationNodeContext,
    accepted: &Rc<ConversationMatch>,
) -> Result<Option<Rc<Value>>, ConversationAssemblerError> {
    let Some(state) = context.state.as_deref() else {
        return Ok(None);
    };
    let mut state = decode(state)?;
    if let Some(failure) = failure_from(accepted) {
        state.failure = Some(failure);
    } else if retry_turn(&accepted.event.event_type, &accepted.event.data) == Some(state.turn) {
        state.hidden = true;
    }
    encode(&state).map(Some)
}

fn build_turn_error(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<ConversationViewNode>>, ConversationAssemblerError> {
    let state = context
        .state
        .as_deref()
        .map(decode)
        .transpose()?
        .or_else(|| fallback_state(context));
    let Some(state) = state else {
        return Ok(None);
    };
    let Some(failure) = state.failure else {
        return Ok(None);
    };
    let TurnFailure {
        seq,
        time,
        message,
        code,
    } = failure;
    let mut node = Map::from_iter([
        ("kind".to_owned(), json!(TURN_ERROR_KIND)),
        ("seq".to_owned(), json!(seq)),
        ("time".to_owned(), json!(time)),
        ("turn".to_owned(), json!(state.turn)),
        ("step".to_owned(), json!(last_step(context))),
        ("message".to_owned(), json!(message)),
    ]);
    if let Some(code) = code {
        node.insert("code".to_owned(), json!(code));
    }
    let data = Value::Object(node);
    if !state.hidden {
        return Ok(Some(chat_node(
            context,
            TURN_ERROR_KIND,
            sequence_anchor(seq),
            data,
        )));
    }
    let current_exists = context
        .current
        .borrow()
        .get("chat")
        .is_some_and(Option::is_some);
    Ok(current_exists.then(|| {
        chat_node_with(
            context,
            TURN_ERROR_KIND,
            sequence_anchor(seq),
            data,
            None,
            Some(ConversationVisibility::Hidden),
        )
    }))
}

fn coordinate(data: &Value, key: &str) -> Option<u64> {
    data.get(key).and_then(Value::as_u64)
}

fn retry_turn(event_type: &str, data: &Value) -> Option<u64> {
    matches!(event_type, "llm/retry" | "llm/retry-started")
        .then(|| coordinate(data, "turn"))
        .flatten()
}

fn failure_from(accepted: &ConversationMatch) -> Option<TurnFailure> {
    if accepted.event.event_type != "turn/end"
        || accepted
            .event
            .data
            .get("reason")
            .and_then(|reason| reason.get("kind"))
            .and_then(Value::as_str)
            != Some("error")
    {
        return None;
    }
    let failure = accepted
        .event
        .data
        .get("reason")?
        .get("error")
        .cloned()
        .unwrap_or(Value::Null);
    Some(TurnFailure {
        seq: accepted.event.seq,
        time: accepted.event.time,
        message: display_failure_message(&failure),
        code: failure
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn fallback_state(context: &ConversationNodeContext) -> Option<TurnErrorState> {
    let matches = context.matches.borrow();
    let failure_match = matches
        .iter()
        .find(|accepted| failure_from(accepted).is_some())?;
    let failure = failure_from(failure_match)?;
    let turn = coordinate(&failure_match.event.data, "turn")?;
    Some(TurnErrorState {
        turn,
        hidden: matches.iter().any(|accepted| {
            retry_turn(&accepted.event.event_type, &accepted.event.data) == Some(turn)
        }),
        failure: Some(failure),
    })
}

fn last_step(context: &ConversationNodeContext) -> u64 {
    match context_location(context) {
        ConversationLocation::Turn { turn } | ConversationLocation::Step { turn, .. } => {
            turn.steps.last().map_or(0, |step| step.step)
        }
        ConversationLocation::Session | ConversationLocation::Unresolved => 0,
    }
}

fn decode(value: &Value) -> Result<TurnErrorState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn encode(value: &TurnErrorState) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
