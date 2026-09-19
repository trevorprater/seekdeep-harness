use super::json_value;
use std::rc::Rc;

use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationMatchResult, ConversationMatchRole,
};

use super::{chat_node, is_append_surface_event, sequence_anchor};

/// Unclaimed append-surface fallback definition kind.
pub const UNKNOWN_SURFACE_KIND: &str = "unknown-surface";

/// Builds the unmatched append-surface fallback definition.
#[must_use]
pub fn conversation_unknown_fallback_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: UNKNOWN_SURFACE_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            Ok(
                is_append_surface_event(event).then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, accepted, _reader| {
            Ok(Some(Rc::new(Value::object([
                ("kind", json_value(&"unknown")),
                ("seq", json_value(&accepted.event.seq)),
                ("time", json_value(&accepted.event.time)),
                ("type", json_value(&accepted.event.event_type)),
                ("data", json_value(&accepted.event.data)),
            ]))))
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let seq = state.get_value("seq").and_then(Value::as_u64).unwrap_or(0);
            Ok(Some(chat_node(
                context,
                "unknown",
                sequence_anchor(seq),
                state.clone(),
            )))
        })),
    }
}
