use std::rc::Rc;

use indexmap::IndexSet;
use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationMatchResult,
    ConversationMatchRole, ConversationPublication,
};
use seekdeep_lossless_json::JsonString;
use serde::{Deserialize, Serialize};

/// Cumulative next-turn inbox definition kind.
pub const INBOX_NEXT_TURN_KIND: &str = "inbox-next-turn";
/// Cumulative next-step inbox definition kind.
pub const INBOX_NEXT_STEP_KIND: &str = "inbox-next-step";

/// Cumulative durable inbox state used to classify admitted steering messages.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConversationInboxState {
    /// Identities still resident in this inbox.
    pending: Vec<Value>,
    /// Next-step identities removed by a non-cancelled splice.
    claimed: IndexSet<JsonString>,
}

impl ConversationInboxState {
    pub(crate) fn contains_claim(&self, id: &JsonString) -> bool {
        self.claimed.contains(id)
    }
}

/// Builds cumulative next-turn and next-step inbox definitions in source order.
#[must_use]
pub fn conversation_inbox_definitions() -> [AssemblerNodeDefinition; 2] {
    [
        inbox_definition("next-turn", INBOX_NEXT_TURN_KIND),
        inbox_definition("next-step", INBOX_NEXT_STEP_KIND),
    ]
}

fn inbox_definition(target: &'static str, kind: &'static str) -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: kind.to_owned(),
        target: None,
        match_event: Rc::new(move |event| {
            Ok((event.event_type == "agent/inbox/spliced"
                && event.data.get_value("target").and_then(Value::as_str) == Some(target))
            .then(|| ConversationMatchResult {
                id: event.seq.to_string(),
                role: ConversationMatchRole::Start,
            }))
        }),
        start: Rc::new(move |_context, accepted, reader| {
            if accepted.event.event_type != "agent/inbox/spliced" {
                return Err(ConversationAssemblerError::new(format!(
                    "{kind} start requires agent/inbox/spliced"
                )));
            }
            let previous = reader
                .previous(kind)
                .map(|previous| decode(previous.state.as_ref()))
                .transpose()?;
            encode(&apply_splice(
                previous.as_ref(),
                target,
                &accepted.event.data,
            )?)
            .map(Some)
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: Some(Rc::new(|_| Ok(ConversationPublication::None))),
        build_location_data: None,
        build_view_node: None,
    }
}

fn apply_splice(
    previous: Option<&ConversationInboxState>,
    target: &str,
    splice: &Value,
) -> Result<ConversationInboxState, ConversationAssemblerError> {
    let mut pending = previous.map_or_else(Vec::new, |state| state.pending.clone());
    let mut claimed = previous.map_or_else(IndexSet::new, |state| state.claimed.clone());
    let start = required_index(splice, "start")?.min(pending.len());
    let removed_count = splice
        .get_value("removedCount")
        .map(|_| required_index(splice, "removedCount"))
        .transpose()?
        .unwrap_or(0);
    let inserted = splice
        .get_value("inserted")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversationAssemblerError::new("inbox splice inserted must be an array"))?
        .iter()
        .map(|identity| identity_id(identity).map(|_| identity.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let end = start.saturating_add(removed_count).min(pending.len());
    let removed = pending
        .splice(start..end, inserted.iter().cloned())
        .collect::<Vec<_>>();
    for identity in &inserted {
        claimed.shift_remove(&identity_id(identity)?);
    }
    if target == "next-step"
        && splice.get_value("outcome").and_then(Value::as_str) != Some("canceled")
    {
        for identity in removed {
            claimed.insert(identity_id(&identity)?);
        }
    }
    Ok(ConversationInboxState { pending, claimed })
}

fn required_index(value: &Value, field: &str) -> Result<usize, ConversationAssemblerError> {
    value
        .get_value(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            ConversationAssemblerError::new(format!("inbox splice {field} must be a u64"))
        })
}

fn identity_id(value: &Value) -> Result<JsonString, ConversationAssemblerError> {
    value
        .get_value("id")
        .and_then(|value| value.deserialize().ok())
        .ok_or_else(|| ConversationAssemblerError::new("inbox splice identity omitted string id"))
}

pub(crate) fn decode(value: &Value) -> Result<ConversationInboxState, ConversationAssemblerError> {
    value
        .deserialize()
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    Value::from_serialize(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
