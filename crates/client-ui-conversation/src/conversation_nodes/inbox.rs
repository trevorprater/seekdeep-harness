use std::rc::Rc;

use indexmap::IndexSet;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationMatchResult,
    ConversationMatchRole, ConversationPublication,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cumulative next-turn inbox definition kind.
pub const INBOX_NEXT_TURN_KIND: &str = "inbox-next-turn";
/// Cumulative next-step inbox definition kind.
pub const INBOX_NEXT_STEP_KIND: &str = "inbox-next-step";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InboxIdentity {
    id: String,
}

/// Cumulative durable inbox state used to classify admitted steering messages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationInboxState {
    /// Identities still resident in this inbox.
    pending: Vec<InboxIdentity>,
    /// Next-step identities removed by a non-cancelled splice.
    claimed: IndexSet<String>,
}

impl ConversationInboxState {
    pub(crate) fn contains_claim(&self, id: &str) -> bool {
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
                && event.data.get("target").and_then(Value::as_str) == Some(target))
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
        .get("removedCount")
        .map(|_| required_index(splice, "removedCount"))
        .transpose()?
        .unwrap_or(0);
    let inserted = splice
        .get("inserted")
        .and_then(Value::as_array)
        .ok_or_else(|| ConversationAssemblerError::new("inbox splice inserted must be an array"))?
        .iter()
        .map(|identity| {
            identity
                .get("id")
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
    if target == "next-step" && splice.get("outcome").and_then(Value::as_str) != Some("canceled") {
        for identity in removed {
            claimed.insert(identity.id);
        }
    }
    Ok(ConversationInboxState { pending, claimed })
}

fn required_index(value: &Value, field: &str) -> Result<usize, ConversationAssemblerError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            ConversationAssemblerError::new(format!("inbox splice {field} must be a u64"))
        })
}

pub(crate) fn decode(value: &Value) -> Result<ConversationInboxState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
