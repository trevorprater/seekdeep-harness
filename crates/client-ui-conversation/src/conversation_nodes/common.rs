use std::rc::Rc;

use seekdeep_client_runtime::{
    ChatConversationViewMetadata, ConversationLocation, ConversationLocationEvent,
    ConversationNodeContext, ConversationViewNode, ConversationVisibility,
};
use serde_json::Value;

pub(crate) const CHAT_TARGET: &str = "chat";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_SAFE_INTEGER_F64: f64 = 9_007_199_254_740_991.0;

/// Relative position of an interrupted Assistant before its closing boundary.
pub const CHAT_INTERRUPTED_ASSISTANT_OFFSET: f64 = -0.9;
/// Relative position of a follow-up to an interrupted Assistant.
pub const CHAT_INTERRUPTED_FOLLOWUP_OFFSET: f64 = -0.8;
/// Relative position of a max-token notice after the closing Assistant.
pub const CHAT_MAX_TOKENS_NOTICE_OFFSET: f64 = 0.05;
/// Relative position of a follow-up to an ordinary finalized Assistant.
pub const CHAT_FINALIZED_FOLLOWUP_OFFSET: f64 = 0.1;

pub(crate) fn context_location(context: &ConversationNodeContext) -> ConversationLocation {
    context
        .start
        .as_ref()
        .map(|accepted| accepted.location.clone())
        .or_else(|| {
            context
                .matches
                .borrow()
                .first()
                .map(|accepted| accepted.location.clone())
        })
        .unwrap_or(ConversationLocation::Unresolved)
}

pub(crate) fn chat_node(
    context: &ConversationNodeContext,
    kind: &str,
    anchor_seq: f64,
    data: Value,
) -> Rc<ConversationViewNode> {
    chat_node_with(context, kind, anchor_seq, data, None, None)
}

pub(crate) fn chat_node_with(
    context: &ConversationNodeContext,
    kind: &str,
    anchor_seq: f64,
    data: Value,
    location: Option<ConversationLocation>,
    visibility: Option<ConversationVisibility>,
) -> Rc<ConversationViewNode> {
    Rc::new(ConversationViewNode {
        key: context.key.clone(),
        kind: kind.to_owned(),
        id: context.id.clone(),
        target: CHAT_TARGET.to_owned(),
        data: Rc::new(data),
        placement: None,
        chat: Some(ChatConversationViewMetadata {
            anchor_seq,
            location: location.unwrap_or_else(|| context_location(context)),
            visibility: visibility.unwrap_or(ConversationVisibility::Visible),
        }),
    })
}

pub(crate) fn is_append_surface_event(event: &ConversationLocationEvent) -> bool {
    is_surface_eligible(event)
        && event
            .wire
            .as_ref()
            .and_then(|wire| wire.get("surfaceOp"))
            .and_then(Value::as_str)
            == Some("append")
}

pub(crate) fn is_replacement_surface_event(event: &ConversationLocationEvent) -> bool {
    is_surface_eligible(event)
        && event
            .wire
            .as_ref()
            .is_some_and(|wire| wire.get("surfaceOp").is_some_and(|value| value != "append"))
}

fn is_surface_eligible(event: &ConversationLocationEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "user/message" | "assistant/message" | "tool/result"
    )
}

pub(crate) fn sequence_anchor(seq: u64) -> f64 {
    debug_assert!(seq <= MAX_SAFE_INTEGER);
    #[allow(clippy::cast_precision_loss)]
    {
        seq as f64
    }
}

/// Reads a finite non-negative JavaScript-safe integer coordinate.
#[must_use]
pub fn conversation_coordinate(value: &Value) -> Option<u64> {
    if let Some(value) = value.as_u64() {
        return (value <= MAX_SAFE_INTEGER).then_some(value);
    }
    let value = value.as_f64()?;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > MAX_SAFE_INTEGER_F64 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as u64)
}

pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}
