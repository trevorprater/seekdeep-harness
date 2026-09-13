use std::rc::Rc;

use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ContextProvenanceJsonView, ContextRole, ConversationAssemblerError,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext, ConversationViewNode,
    KnownContextForm, context_form_json, context_provenance_json,
};

use super::{
    INBOX_NEXT_STEP_KIND, chat_node, inbox::decode, is_append_surface_event,
    is_replacement_surface_event, js_string, js_text, json_value, sequence_anchor,
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
                    .get_value("id")
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
                event.data.get_value("source").cloned().ok_or_else(|| {
                    ConversationAssemblerError::new("user/message omitted source")
                })?;
            let source_kind = source
                .get_value("kind")
                .filter(|value| value.is_string())
                .ok_or_else(|| {
                    ConversationAssemblerError::new("user/message source omitted kind")
                })?;
            let content = event
                .data
                .get_value("content")
                .cloned()
                .unwrap_or_else(|| json_value(&()));
            let state = if source_kind == "user" {
                let id = event
                    .data
                    .get_value("id")
                    .map_or_else(|| "undefined".into(), js_text);
                let claimed = reader
                    .previous(INBOX_NEXT_STEP_KIND)
                    .map(|previous| decode(previous.state.as_ref()))
                    .transpose()?
                    .is_some_and(|state| state.contains_claim(&id));
                if claimed {
                    Value::object([
                        ("kind", json_value(&"steering")),
                        (
                            "messageId",
                            json_value(
                                &(event
                                    .data
                                    .get_value("id")
                                    .cloned()
                                    .unwrap_or_else(|| json_value(&()))),
                            ),
                        ),
                        ("seq", json_value(&event.seq)),
                        ("time", json_value(&event.time)),
                        ("content", json_value(&content)),
                        ("source", json_value(&source)),
                    ])
                } else {
                    Value::object([
                        ("kind", json_value(&"user")),
                        ("seq", json_value(&event.seq)),
                        ("time", json_value(&event.time)),
                        ("content", json_value(&content)),
                        ("source", json_value(&source)),
                    ])
                }
            } else {
                context_message(event, content, &source)
            };
            Ok(Some(Rc::new(state)))
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(build_message_node)),
    }
}

fn build_message_node(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<ConversationViewNode>>, ConversationAssemblerError> {
    let Some(state) = context.state.as_deref() else {
        return Ok(None);
    };
    let kind = state
        .get_value("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| ConversationAssemblerError::new("input-message state omitted kind"))?;
    let seq = state
        .get_value("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| ConversationAssemblerError::new("input-message state omitted seq"))?;
    Ok(Some(chat_node(
        context,
        kind,
        sequence_anchor(seq),
        state.clone(),
    )))
}

fn context_message(
    event: &seekdeep_client_runtime::ConversationLocationEvent,
    content: Value,
    source: &Value,
) -> Value {
    let provenance = context_provenance_json(source);
    Value::object([
        ("kind", json_value("context")),
        ("seq", json_value(&event.seq)),
        ("time", json_value(&event.time)),
        ("content", content),
        ("source", source.clone()),
        ("provenance", provenance_value(&provenance)),
        (
            "form",
            context_form_json(source)
                .map_or_else(|| json_value(&()), |form| json_value(form_name(form))),
        ),
    ])
}

fn is_compaction_checkpoint(event: &seekdeep_client_runtime::ConversationLocationEvent) -> bool {
    event.event_type == "user/message"
        && is_replacement_surface_event(event)
        && event.data.get_value("source").is_some_and(|source| {
            source.get_value("kind").and_then(Value::as_str) == Some("plugin")
                && source.get_value("plugin").and_then(Value::as_str) == Some("compact")
        })
}

fn provenance_value(provenance: &ContextProvenanceJsonView) -> Value {
    let mut value = Vec::from([(
        "role".to_owned(),
        json_value(
            &(match provenance.role {
                ContextRole::Inject => "inject",
                ContextRole::Recall => "recall",
            }),
        ),
    )]);
    if let Some(label) = &provenance.label {
        value.push(("label".to_owned(), json_value(&label)));
    }
    Value::object(value)
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
