//! Trajectory inbox classification and input-message Definitions.

use std::rc::Rc;

use crate::json_value::{json, null};
use indexmap::IndexMap as Map;
use indexmap::IndexSet;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ContextProvenanceJsonView as ContextProvenanceView, ContextRole,
    ConversationAssemblerError, ConversationMatchResult, ConversationMatchRole,
    ConversationPublication, KnownContextForm, context_form_json as context_form,
    context_provenance_json as context_provenance,
};
use seekdeep_lossless_json::JsonValue as Value;
use serde::{Deserialize, Serialize};

use crate::{TRAJECTORY_TARGET, trajectory_node};

/// State-only next-step inbox Definition kind.
pub const TRAJECTORY_INBOX_KIND: &str = "trajectory-inbox-next-step";
/// User/context/steering input Definition kind.
pub const TRAJECTORY_INPUT_MESSAGE_KIND: &str = "trajectory-input-message";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InboxIdentity {
    id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct InboxState {
    pending: Vec<InboxIdentity>,
    claimed: IndexSet<String>,
}

/// Builds the state-only inbox Definition followed by the trajectory message Definition.
#[must_use]
pub fn trajectory_message_definitions() -> [AssemblerNodeDefinition; 2] {
    [
        trajectory_inbox_definition(),
        trajectory_message_definition(),
    ]
}

fn trajectory_inbox_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_INBOX_KIND.to_owned(),
        target: None,
        match_event: Rc::new(|event| {
            Ok((event.event_type == "agent/inbox/spliced"
                && event.data.get_value("target").and_then(Value::as_str) == Some("next-step"))
            .then(|| ConversationMatchResult {
                id: event.seq.to_string(),
                role: ConversationMatchRole::Start,
            }))
        }),
        start: Rc::new(|_context, accepted, reader| {
            if accepted.event.event_type != "agent/inbox/spliced" {
                return Err(ConversationAssemblerError::new(
                    "trajectory-inbox-next-step start requires agent/inbox/spliced",
                ));
            }
            let previous = reader
                .previous(TRAJECTORY_INBOX_KIND)
                .map(|previous| decode_inbox(previous.state.as_ref()))
                .transpose()?;
            apply_splice(previous.as_ref(), &accepted.event.data)
                .and_then(|state| encode(&state))
                .map(Some)
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: Some(Rc::new(|_| Ok(ConversationPublication::None))),
        build_location_data: None,
        build_view_node: None,
    }
}

fn trajectory_message_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_INPUT_MESSAGE_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "user/message").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, accepted, reader| {
            if accepted.event.event_type != "user/message" {
                return Err(ConversationAssemblerError::new(
                    "trajectory-input-message start requires user/message",
                ));
            }
            let event = &accepted.event;
            let source =
                event.data.get_value("source").cloned().ok_or_else(|| {
                    ConversationAssemblerError::new("user/message omitted source")
                })?;
            let source_kind = source
                .get_value("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ConversationAssemblerError::new("user/message source omitted kind")
                })?;
            let content = event
                .data
                .get_value("content")
                .cloned()
                .unwrap_or(null().clone());
            let state = if source_kind == "user" {
                let id = event
                    .data
                    .get_value("id")
                    .map_or_else(|| "undefined".to_owned(), js_string);
                let claimed = reader
                    .previous(TRAJECTORY_INBOX_KIND)
                    .map(|previous| decode_inbox(previous.state.as_ref()))
                    .transpose()?
                    .is_some_and(|state| state.claimed.contains(&id));
                if claimed {
                    json!({
                        "kind": "steering",
                        "messageId": event.data.get_value("id").cloned().unwrap_or(null().clone()),
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
                let mut state = Map::<String, Value>::from_iter([
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
                Value::object(state)
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
            let seq = state
                .get_value("seq")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ConversationAssemblerError::new("trajectory input state omitted seq")
                })?;
            Ok(Some(trajectory_node(
                context,
                seq,
                json!({"kind": "node", "node": state}),
            )))
        })),
    }
}

fn apply_splice(
    previous: Option<&InboxState>,
    splice: &Value,
) -> Result<InboxState, ConversationAssemblerError> {
    let mut pending = previous.map_or_else(Vec::new, |state| state.pending.clone());
    let mut claimed = previous.map_or_else(IndexSet::new, |state| state.claimed.clone());
    let start = splice
        .get_value("start")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| ConversationAssemblerError::new("inbox splice start must be a u64"))?
        .min(pending.len());
    let removed_count = splice
        .get_value("removedCount")
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    ConversationAssemblerError::new("inbox splice removedCount must be a u64")
                })
        })
        .transpose()?
        .unwrap_or(0);
    let inserted = splice
        .get_value("inserted")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversationAssemblerError::new("inbox splice inserted must be an array"))?
        .iter()
        .map(|identity| {
            identity
                .get_value("id")
                .and_then(Value::as_str)
                .map(|id| InboxIdentity { id: id.to_owned() })
                .ok_or_else(|| {
                    ConversationAssemblerError::new("inbox splice identity omitted string id")
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let end = start.saturating_add(removed_count).min(pending.len());
    let removed = pending
        .splice(start..end, inserted.iter().cloned())
        .collect::<Vec<_>>();
    for identity in &inserted {
        claimed.shift_remove(&identity.id);
    }
    if splice.get_value("outcome").and_then(Value::as_str) != Some("canceled") {
        for identity in removed {
            claimed.insert(identity.id);
        }
    }
    Ok(InboxState { pending, claimed })
}

fn provenance_value(provenance: &ContextProvenanceView) -> Value {
    let mut value = Map::<String, Value>::from_iter([(
        "role".to_owned(),
        json!(match provenance.role {
            ContextRole::Inject => "inject",
            ContextRole::Recall => "recall",
        }),
    )]);
    if let Some(label) = &provenance.label {
        value.insert("label".to_owned(), json!(label));
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

fn js_string(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        text.to_owned()
    } else if let Some(values) = value.as_array() {
        values.iter().map(js_string).collect::<Vec<_>>().join(",")
    } else if value.is_object() {
        "[object Object]".to_owned()
    } else {
        value.stringify()
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    Value::from_serialize(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode_inbox(value: &Value) -> Result<InboxState, ConversationAssemblerError> {
    value
        .deserialize()
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
