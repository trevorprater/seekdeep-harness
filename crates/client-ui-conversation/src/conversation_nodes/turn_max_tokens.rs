use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocation, ConversationMatch,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CHAT_MAX_TOKENS_NOTICE_OFFSET, chat_node, context_location, sequence_anchor};

/// Output-token-cap turn notice definition kind.
pub const TURN_MAX_TOKENS_KIND: &str = "turn-max-tokens";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TurnMaxTokensState {
    turn: u64,
    seq: u64,
    time: i64,
}

/// Builds the notice definition for turns ended by the request output-token cap.
#[must_use]
pub fn conversation_turn_max_tokens_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TURN_MAX_TOKENS_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            Ok((event.event_type == "turn/end"
                && event
                    .data
                    .get("reason")
                    .and_then(|reason| reason.get("kind"))
                    .and_then(Value::as_str)
                    == Some("max-tokens"))
            .then(|| {
                event
                    .data
                    .get("turn")
                    .and_then(Value::as_u64)
                    .map(|turn| ConversationMatchResult {
                        id: turn.to_string(),
                        role: ConversationMatchRole::Start,
                    })
            })
            .flatten())
        }),
        start: Rc::new(|_context, accepted, _reader| {
            let state = state_from(accepted).ok_or_else(|| {
                ConversationAssemblerError::new(
                    "turn-max-tokens start requires a max-tokens turn/end",
                )
            })?;
            encode(&state).map(Some)
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let state = decode(state)?;
            let node = json!({
                "kind": TURN_MAX_TOKENS_KIND,
                "seq": state.seq,
                "time": state.time,
                "turn": state.turn,
                "step": last_step(context),
            });
            Ok(Some(chat_node(
                context,
                TURN_MAX_TOKENS_KIND,
                notice_anchor(context, state.seq),
                node,
            )))
        })),
    }
}

fn state_from(accepted: &ConversationMatch) -> Option<TurnMaxTokensState> {
    if accepted.event.event_type != "turn/end"
        || accepted
            .event
            .data
            .get("reason")
            .and_then(|reason| reason.get("kind"))
            .and_then(Value::as_str)
            != Some("max-tokens")
    {
        return None;
    }
    Some(TurnMaxTokensState {
        turn: accepted.event.data.get("turn")?.as_u64()?,
        seq: accepted.event.seq,
        time: accepted.event.time,
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

fn notice_anchor(context: &ConversationNodeContext, seq: u64) -> f64 {
    let location = context_location(context);
    let turn = match location {
        ConversationLocation::Turn { turn } | ConversationLocation::Step { turn, .. } => turn,
        ConversationLocation::Session | ConversationLocation::Unresolved => {
            return sequence_anchor(seq);
        }
    };
    turn.data
        .get("turn-tail")
        .and_then(|tail| tail.get("closing").cloned())
        .filter(|closing| !closing.is_null())
        .and_then(|closing| {
            closing
                .get("finalNode")
                .and_then(|node| node.get("seq"))
                .and_then(Value::as_f64)
        })
        .map_or(sequence_anchor(seq), |closing| {
            closing + CHAT_MAX_TOKENS_NOTICE_OFFSET
        })
}

fn decode(value: &Value) -> Result<TurnMaxTokensState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn encode(value: &TurnMaxTokensState) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
