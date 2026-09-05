use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocationEvent,
    ConversationMatch, ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{
    chat_node, conversation_coordinate, is_replacement_surface_event, js_string, sequence_anchor,
};

/// Slash-command and manual-compaction definition kind.
pub const COMMAND_NODE_KIND: &str = "command";
const COMPACT_PLUGIN: &str = "compact";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct EventEvidence {
    pub(crate) seq: u64,
    pub(crate) time: i64,
    #[serde(rename = "type")]
    pub(crate) event_type: String,
    pub(crate) data: Value,
}

impl From<&ConversationMatch> for EventEvidence {
    fn from(accepted: &ConversationMatch) -> Self {
        Self {
            seq: accepted.event.seq,
            time: accepted.event.time,
            event_type: accepted.event.event_type.clone(),
            data: accepted.event.data.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CommandState {
    command: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<EventEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<EventEvidence>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompactSource {
    pub(crate) compaction_id: String,
    pub(crate) source_command_id: Option<Value>,
}

/// Builds the slash-command lifecycle and integrated manual-compaction definition.
#[must_use]
pub fn conversation_command_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: COMMAND_NODE_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| Ok(match_command_event(event))),
        start: Rc::new(|_context, accepted, _reader| {
            encode(&CommandState {
                command: command_from_run(accepted)?,
                summary: None,
                checkpoint: None,
            })
            .map(Some)
        }),
        update: Rc::new(|context, accepted| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let mut state = decode::<CommandState>(state)?;
            if accepted.event.event_type == "command/done" {
                state.command = command_from_done(accepted, Some(&state.command))?;
                return encode(&state).map(Some);
            }
            update_compaction_state(context.state.clone(), state, accepted)
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(build_command_node)),
    }
}

fn match_command_event(event: &ConversationLocationEvent) -> Option<ConversationMatchResult> {
    if matches!(event.event_type.as_str(), "command/run" | "command/done") {
        return Some(ConversationMatchResult {
            id: event
                .data
                .get("commandId")
                .map_or_else(|| "undefined".to_owned(), js_string),
            role: if event.event_type == "command/run" {
                ConversationMatchRole::Start
            } else {
                ConversationMatchRole::Update
            },
        });
    }
    if let Some(command_id) = compact_source(event).and_then(|source| source.source_command_id) {
        return Some(ConversationMatchResult {
            id: js_string(&command_id),
            role: ConversationMatchRole::Update,
        });
    }
    if matches!(
        event.event_type.as_str(),
        "compaction/start" | "compaction/summary" | "compaction/end"
    ) && let Some(command_id) = event.data.get("sourceCommandId")
    {
        return Some(ConversationMatchResult {
            id: js_string(command_id),
            role: ConversationMatchRole::Update,
        });
    }
    None
}

fn command_from_run(accepted: &ConversationMatch) -> Result<Value, ConversationAssemblerError> {
    if accepted.event.event_type != "command/run" {
        return Err(ConversationAssemblerError::new(
            "command start requires command/run",
        ));
    }
    let data = &accepted.event.data;
    Ok(json!({
        "kind": COMMAND_NODE_KIND,
        "seq": accepted.event.seq,
        "time": accepted.event.time,
        "commandId": data.get("commandId").cloned().unwrap_or(Value::Null),
        "name": data.get("name").cloned().unwrap_or(Value::Null),
        "args": data.get("args").cloned().filter(|value| !value.is_null()).unwrap_or(Value::Null),
        "outcome": Value::Null,
    }))
}

fn command_from_done(
    accepted: &ConversationMatch,
    previous: Option<&Value>,
) -> Result<Value, ConversationAssemblerError> {
    if accepted.event.event_type != "command/done" {
        return Err(ConversationAssemblerError::new(
            "command update requires command/done",
        ));
    }
    let data = &accepted.event.data;
    let mut outcome = Map::from_iter([(
        "kind".to_owned(),
        data.get("kind").cloned().unwrap_or(Value::Null),
    )]);
    if let Some(text) = data.get("text") {
        outcome.insert("text".to_owned(), text.clone());
    }
    if data.get("kind").and_then(Value::as_str) == Some("success")
        && let Some(source_event_seq) = data.get("sourceEventSeq").and_then(conversation_coordinate)
    {
        outcome.insert("sourceEventSeq".to_owned(), json!(source_event_seq));
    }
    Ok(json!({
        "kind": COMMAND_NODE_KIND,
        "seq": previous.and_then(|value| value.get("seq")).and_then(Value::as_u64).unwrap_or(accepted.event.seq),
        "time": previous.and_then(|value| value.get("time")).and_then(Value::as_i64).unwrap_or(accepted.event.time),
        "commandId": data.get("commandId").cloned().unwrap_or(Value::Null),
        "name": previous.and_then(|value| value.get("name")).cloned().filter(|value| !value.is_null()).unwrap_or(Value::Null),
        "args": previous.and_then(|value| value.get("args")).cloned().filter(|value| !value.is_null()).unwrap_or(Value::Null),
        "outcome": Value::Object(outcome),
    }))
}

pub(crate) fn compact_source(event: &ConversationLocationEvent) -> Option<CompactSource> {
    if event.event_type != "user/message" || !is_replacement_surface_event(event) {
        return None;
    }
    let source = event.data.get("source")?;
    if source.get("kind").and_then(Value::as_str) != Some("plugin")
        || source.get("plugin").and_then(Value::as_str) != Some(COMPACT_PLUGIN)
    {
        return None;
    }
    Some(CompactSource {
        compaction_id: source.get("compactionId")?.as_str()?.to_owned(),
        source_command_id: source.get("sourceCommandId").cloned(),
    })
}

pub(crate) fn compact_summary(
    summary: Option<&EventEvidence>,
    checkpoint: &EventEvidence,
) -> Value {
    let mut text = Value::Null;
    let mut shadowed_item_count = Value::Null;
    let mut shadowed_token_count = Value::Null;
    if let Some(summary) = summary.filter(|summary| summary.event_type == "compaction/summary") {
        if let Some(blocks) = summary.data.get("summary").and_then(Value::as_array) {
            let joined = blocks
                .iter()
                .map(|block| {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        block.get("text").map_or_else(String::new, js_string)
                    } else {
                        String::new()
                    }
                })
                .collect::<String>();
            if !joined.trim().is_empty() {
                text = Value::String(joined);
            }
        }
        if let Some(seqs) = summary.data.get("shadowedSeqs").and_then(Value::as_array)
            && seqs
                .iter()
                .all(|seq| conversation_coordinate(seq).is_some())
        {
            shadowed_item_count = json!(seqs.len());
        }
        if let Some(tokens) = summary
            .data
            .get("shadowedTokenCount")
            .and_then(conversation_coordinate)
        {
            shadowed_token_count = json!(tokens);
        }
    }
    json!({
        "kind": "compaction",
        "seq": checkpoint.seq,
        "time": checkpoint.time,
        "summary": text,
        "summaryEventSeq": summary.map_or(Value::Null, |summary| json!(summary.seq)),
        "shadowedItemCount": shadowed_item_count,
        "shadowedTokenCount": shadowed_token_count,
    })
}

pub(crate) fn update_compaction_state<State>(
    current: Option<Rc<Value>>,
    mut state: State,
    accepted: &ConversationMatch,
) -> Result<Option<Rc<Value>>, ConversationAssemblerError>
where
    State: CompactionEvidence + Serialize,
{
    if accepted.event.event_type == "compaction/summary" {
        state.set_summary(EventEvidence::from(accepted));
        return encode(&state).map(Some);
    }
    if compact_source(&accepted.event).is_some() {
        state.set_checkpoint(EventEvidence::from(accepted));
        return encode(&state).map(Some);
    }
    Ok(current)
}

pub(crate) trait CompactionEvidence {
    fn set_summary(&mut self, summary: EventEvidence);
    fn set_checkpoint(&mut self, checkpoint: EventEvidence);
}

impl CompactionEvidence for CommandState {
    fn set_summary(&mut self, summary: EventEvidence) {
        self.summary = Some(summary);
    }

    fn set_checkpoint(&mut self, checkpoint: EventEvidence) {
        self.checkpoint = Some(checkpoint);
    }
}

fn fallback_state(context: &ConversationNodeContext) -> Option<CommandState> {
    let matches = context.matches.borrow();
    let done = matches
        .iter()
        .find(|accepted| accepted.event.event_type == "command/done");
    let checkpoint = matches
        .iter()
        .find(|accepted| compact_source(&accepted.event).is_some());
    let summary = matches
        .iter()
        .find(|accepted| accepted.event.event_type == "compaction/summary");
    let Some(checkpoint) = checkpoint else {
        return done
            .map(|done| command_from_done(done, None))
            .transpose()
            .ok()
            .flatten()
            .map(|command| CommandState {
                command,
                summary: None,
                checkpoint: None,
            });
    };
    let source = compact_source(&checkpoint.event)?;
    let source_command_id = source.source_command_id?;
    let mut command = if let Some(done) = done {
        command_from_done(done, None).ok()?
    } else {
        json!({
            "kind": COMMAND_NODE_KIND,
            "seq": checkpoint.event.seq,
            "time": checkpoint.event.time,
            "commandId": source_command_id,
            "name": "compact",
            "args": Value::Null,
            "outcome": Value::Null,
        })
    };
    command["name"] = json!("compact");
    Some(CommandState {
        command,
        summary: summary.map(|accepted| EventEvidence::from(accepted.as_ref())),
        checkpoint: Some(EventEvidence::from(checkpoint.as_ref())),
    })
}

fn build_command_node(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<seekdeep_client_runtime::ConversationViewNode>>, ConversationAssemblerError> {
    let state = context
        .state
        .as_deref()
        .map(decode::<CommandState>)
        .transpose()?
        .or_else(|| fallback_state(context));
    let Some(state) = state else {
        return Ok(None);
    };
    let command_seq = state
        .command
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| ConversationAssemblerError::new("command state omitted seq"))?;
    if state.command.get("name").and_then(Value::as_str) != Some("compact") {
        return Ok(Some(chat_node(
            context,
            COMMAND_NODE_KIND,
            sequence_anchor(command_seq),
            state.command,
        )));
    }
    let compaction = state
        .checkpoint
        .as_ref()
        .map(|checkpoint| compact_summary(state.summary.as_ref(), checkpoint));
    let anchor = compaction
        .as_ref()
        .and_then(|marker| marker.get("seq"))
        .and_then(Value::as_u64)
        .unwrap_or(command_seq);
    Ok(Some(chat_node(
        context,
        "manual-compaction",
        sequence_anchor(anchor),
        json!({"command": state.command, "compaction": compaction}),
    )))
}

pub(crate) fn decode<T: for<'de> Deserialize<'de>>(
    value: &Value,
) -> Result<T, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
