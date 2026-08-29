use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocationEvent,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    chat_node,
    command::{
        CompactionEvidence, EventEvidence, compact_source, compact_summary, decode, encode,
        update_compaction_state,
    },
    sequence_anchor,
};

/// Automatic compaction definition kind.
pub const COMPACTION_NODE_KIND: &str = "compaction";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct CompactionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<EventEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<EventEvidence>,
}

impl CompactionEvidence for CompactionState {
    fn set_summary(&mut self, summary: EventEvidence) {
        self.summary = Some(summary);
    }

    fn set_checkpoint(&mut self, checkpoint: EventEvidence) {
        self.checkpoint = Some(checkpoint);
    }
}

/// Builds the automatic-compaction lifecycle and landed-checkpoint definition.
#[must_use]
pub fn conversation_compaction_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: COMPACTION_NODE_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| Ok(match_compaction_event(event))),
        start: Rc::new(|_context, _accepted, _reader| {
            encode(&CompactionState::default()).map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            update_compaction_state(
                context.state.clone(),
                decode::<CompactionState>(state)?,
                accepted,
            )
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(build_compaction_node)),
    }
}

fn match_compaction_event(event: &ConversationLocationEvent) -> Option<ConversationMatchResult> {
    if let Some(source) = compact_source(event)
        && source.source_command_id.is_none()
    {
        return Some(ConversationMatchResult {
            id: source.compaction_id,
            role: ConversationMatchRole::Update,
        });
    }
    if !matches!(
        event.event_type.as_str(),
        "compaction/start" | "compaction/summary" | "compaction/end"
    ) || event.data.get("sourceCommandId").is_some()
    {
        return None;
    }
    let compaction_id = event
        .data
        .get("compactionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    Some(ConversationMatchResult {
        id: compaction_id.to_owned(),
        role: if event.event_type == "compaction/start" {
            ConversationMatchRole::Start
        } else {
            ConversationMatchRole::Update
        },
    })
}

fn fallback_state(context: &ConversationNodeContext) -> CompactionState {
    let matches = context.matches.borrow();
    CompactionState {
        summary: matches
            .iter()
            .find(|accepted| accepted.event.event_type == "compaction/summary")
            .map(|accepted| EventEvidence::from(accepted.as_ref())),
        checkpoint: matches
            .iter()
            .find(|accepted| compact_source(&accepted.event).is_some())
            .map(|accepted| EventEvidence::from(accepted.as_ref())),
    }
}

fn build_compaction_node(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<seekdeep_client_runtime::ConversationViewNode>>, ConversationAssemblerError> {
    let state = context
        .state
        .as_deref()
        .map(decode::<CompactionState>)
        .transpose()?
        .unwrap_or_else(|| fallback_state(context));
    let Some(checkpoint) = state.checkpoint.as_ref() else {
        return Ok(None);
    };
    let marker = compact_summary(state.summary.as_ref(), checkpoint);
    Ok(Some(chat_node(
        context,
        COMPACTION_NODE_KIND,
        sequence_anchor(checkpoint.seq),
        marker,
    )))
}
