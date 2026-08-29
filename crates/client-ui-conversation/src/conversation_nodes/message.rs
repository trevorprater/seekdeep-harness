use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ContextProvenanceView, ContextRole, ConversationAssemblerError,
    ConversationMatchResult, ConversationMatchRole, KnownContextForm, context_form,
    context_provenance,
};
use serde_json::{Map, Value, json};

use super::{
    INBOX_NEXT_STEP_KIND, chat_node, inbox::decode, is_append_surface_event,
    is_replacement_surface_event, js_string, sequence_anchor,
};

/// User, steering, and injected-context message definition kind.
pub const INPUT_MESSAGE_KIND: &str = "input-message";

/// Builds the user, steering, and injected-context message definition.
#[must_use]
pub fn conversation_message_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: INPUT_MESSAGE_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            Ok((event.event_type == "user/message"
                && is_append_surface_event(event)
                && !is_compaction_checkpoint(event))
            .then(|| ConversationMatchResult {
                id: event
                    .data
                    .get("id")
                    .map_or_else(|| "undefined".to_owned(), js_string),
                role: ConversationMatchRole::Start,
            }))
        }),
        start: Rc::new(|_context, accepted, reader| {
            if accepted.event.event_type != "user/message" {
                return Err(ConversationAssemblerError::new(
                    "input-message start requires user/message",
                ));
            }
            let event = &accepted.event;
            let source =
                event.data.get("source").cloned().ok_or_else(|| {
                    ConversationAssemblerError::new("user/message omitted source")
                })?;
            let source_kind = source.get("kind").and_then(Value::as_str).ok_or_else(|| {
                ConversationAssemblerError::new("user/message source omitted kind")
            })?;
            let content = event.data.get("content").cloned().unwrap_or(Value::Null);
            let state = if source_kind == "user" {
                let id = event
                    .data
                    .get("id")
                    .map_or_else(|| "undefined".to_owned(), js_string);
                let claimed = reader
                    .previous(INBOX_NEXT_STEP_KIND)
                    .map(|previous| decode(previous.state.as_ref()))
                    .transpose()?
                    .is_some_and(|state| state.contains_claim(&id));
                if claimed {
                    json!({
                        "kind": "steering",
                        "messageId": event.data.get("id").cloned().unwrap_or(Value::Null),
                        "seq": event.seq,
                        "time": event.time,
                        "content": content,
                        "source": source,
                    })
                } else {
                    json!({
                        "kind": "user",
                        "seq": event.seq,
                        "time": event.time,
                        "content": content,
                        "source": source,
                    })
                }
            } else {
                let provenance = context_provenance(&source);
                let mut state = Map::from_iter([
                    ("kind".to_owned(), json!("context")),
                    ("seq".to_owned(), json!(event.seq)),
                    ("time".to_owned(), json!(event.time)),
                    ("content".to_owned(), content),
                    ("source".to_owned(), source.clone()),
                    ("provenance".to_owned(), provenance_value(&provenance)),
                ]);
                if let Some(form) = context_form(&source) {
                    state.insert("form".to_owned(), json!(form_name(form)));
                }
                Value::Object(state)
            };
            Ok(Some(Rc::new(state)))
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let kind = state.get("kind").and_then(Value::as_str).ok_or_else(|| {
                ConversationAssemblerError::new("input-message state omitted kind")
            })?;
            let seq = state.get("seq").and_then(Value::as_u64).ok_or_else(|| {
                ConversationAssemblerError::new("input-message state omitted seq")
            })?;
            Ok(Some(chat_node(
                context,
                kind,
                sequence_anchor(seq),
                state.clone(),
            )))
        })),
    }
}

fn is_compaction_checkpoint(event: &seekdeep_client_runtime::ConversationLocationEvent) -> bool {
    event.event_type == "user/message"
        && is_replacement_surface_event(event)
        && event.data.get("source").is_some_and(|source| {
            source.get("kind").and_then(Value::as_str) == Some("plugin")
                && source.get("plugin").and_then(Value::as_str) == Some("compact")
        })
}

fn provenance_value(provenance: &ContextProvenanceView) -> Value {
    let mut value = Map::from_iter([(
        "role".to_owned(),
        json!(match provenance.role {
            ContextRole::Inject => "inject",
            ContextRole::Recall => "recall",
        }),
    )]);
    if let Some(label) = &provenance.label {
        value.insert("label".to_owned(), json!(label));
    }
    Value::Object(value)
}

const fn form_name(form: KnownContextForm) -> &'static str {
    match form {
        KnownContextForm::Instructions => "instructions",
        KnownContextForm::Catalog => "catalog",
        KnownContextForm::Snapshot => "snapshot",
        KnownContextForm::Notice => "notice",
        KnownContextForm::Relay => "relay",
        KnownContextForm::Recall => "recall",
    }
}
