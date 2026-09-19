use super::json_value;

use std::rc::Rc;

use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationAssemblerError, ConversationLocationEvent,
    ConversationMatch, ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
};
use seekdeep_lossless_json::JsonString;
use serde::{Deserialize, Serialize};

use super::{
    chat_node, conversation_coordinate, is_replacement_surface_event, js_string, js_text,
    sequence_anchor,
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
                .get_value("commandId")
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
    ) && let Some(command_id) = event.data.get_value("sourceCommandId")
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
    Ok(Value::object([
        ("kind", json_value(&COMMAND_NODE_KIND)),
        ("seq", json_value(&accepted.event.seq)),
        ("time", json_value(&accepted.event.time)),
        (
            "commandId",
            json_value(
                &(data
                    .get_value("commandId")
                    .cloned()
                    .unwrap_or_else(|| json_value(&()))),
            ),
        ),
        (
            "name",
            json_value(
                &(data
                    .get_value("name")
                    .cloned()
                    .unwrap_or_else(|| json_value(&()))),
            ),
        ),
        (
            "args",
            json_value(
                &(data
                    .get_value("args")
                    .cloned()
                    .filter(|value| !value.is_null())
                    .unwrap_or_else(|| json_value(&()))),
            ),
        ),
        ("outcome", json_value(&())),
    ]))
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
    let mut outcome = Vec::from([(
        "kind".to_owned(),
        data.get_value("kind")
            .cloned()
            .unwrap_or_else(|| json_value(&())),
    )]);
    if let Some(text) = data.get_value("text") {
        outcome.push(("text".to_owned(), text.clone()));
    }
    if data.get_value("kind").and_then(Value::as_str) == Some("success")
        && let Some(source_event_seq) = data
            .get_value("sourceEventSeq")
            .and_then(conversation_coordinate)
    {
        outcome.push(("sourceEventSeq".to_owned(), json_value(&source_event_seq)));
    }
    Ok(Value::object([
        ("kind", json_value(&COMMAND_NODE_KIND)),
        (
            "seq",
            json_value(
                &(previous
                    .and_then(|value| value.get_value("seq"))
                    .and_then(Value::as_u64)
                    .unwrap_or(accepted.event.seq)),
            ),
        ),
        (
            "time",
            json_value(
                &(previous
                    .and_then(|value| value.get_value("time"))
                    .and_then(Value::as_i64)
                    .unwrap_or(accepted.event.time)),
            ),
        ),
        (
            "commandId",
            json_value(
                &(data
                    .get_value("commandId")
                    .cloned()
                    .unwrap_or_else(|| json_value(&()))),
            ),
        ),
        (
            "name",
            json_value(
                &(previous
                    .and_then(|value| value.get_value("name"))
                    .cloned()
                    .filter(|value| !value.is_null())
                    .unwrap_or_else(|| json_value(&()))),
            ),
        ),
        (
            "args",
            json_value(
                &(previous
                    .and_then(|value| value.get_value("args"))
                    .cloned()
                    .filter(|value| !value.is_null())
                    .unwrap_or_else(|| json_value(&()))),
            ),
        ),
        ("outcome", Value::object(outcome)),
    ]))
}

pub(crate) fn compact_source(event: &ConversationLocationEvent) -> Option<CompactSource> {
    if event.event_type != "user/message" || !is_replacement_surface_event(event) {
        return None;
    }
    let source = event.data.get_value("source")?;
    if source.get_value("kind").and_then(Value::as_str) != Some("plugin")
        || source.get_value("plugin").and_then(Value::as_str) != Some(COMPACT_PLUGIN)
    {
        return None;
    }
    Some(CompactSource {
        compaction_id: source.get_value("compactionId")?.as_str()?.to_owned(),
        source_command_id: source.get_value("sourceCommandId").cloned(),
    })
}

pub(crate) fn compact_summary(
    summary: Option<&EventEvidence>,
    checkpoint: &EventEvidence,
) -> Value {
    let mut text = json_value(&());
    let mut shadowed_item_count = json_value(&());
    let mut shadowed_token_count = json_value(&());
    if let Some(summary) = summary.filter(|summary| summary.event_type == "compaction/summary") {
        if let Some(blocks) = summary.data.get_value("summary").and_then(Value::as_array) {
            let mut joined = JsonString::default();
            for block in blocks {
                if block.get_value("type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = block.get_value("text")
                {
                    joined.push_utf16(js_text(text).utf16_units());
                }
            }
            if !joined.trim().is_empty() {
                text = json_value(&joined);
            }
        }
        if let Some(seqs) = summary
            .data
            .get_value("shadowedSeqs")
            .and_then(Value::as_array)
            && seqs
                .iter()
                .all(|seq| conversation_coordinate(seq).is_some())
        {
            shadowed_item_count = json_value(&(seqs.len()));
        }
        if let Some(tokens) = summary
            .data
            .get_value("shadowedTokenCount")
            .and_then(conversation_coordinate)
        {
            shadowed_token_count = json_value(&tokens);
        }
    }
    Value::object([
        ("kind", json_value(&"compaction")),
        ("seq", json_value(&checkpoint.seq)),
        ("time", json_value(&checkpoint.time)),
        ("summary", json_value(&text)),
        (
            "summaryEventSeq",
            json_value(
                &(summary.map_or_else(|| json_value(&()), |summary| json_value(&summary.seq))),
            ),
        ),
        ("shadowedItemCount", json_value(&shadowed_item_count)),
        ("shadowedTokenCount", json_value(&shadowed_token_count)),
    ])
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
        Value::object([
            ("kind", json_value(&COMMAND_NODE_KIND)),
            ("seq", json_value(&checkpoint.event.seq)),
            ("time", json_value(&checkpoint.event.time)),
            ("commandId", json_value(&source_command_id)),
            ("name", json_value(&"compact")),
            ("args", json_value(&())),
            ("outcome", json_value(&())),
        ])
    };
    command.insert("name", json_value("compact")).ok()?;
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
        .get_value("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| ConversationAssemblerError::new("command state omitted seq"))?;
    if state.command.get_value("name").and_then(Value::as_str) != Some("compact") {
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
        .and_then(|marker| marker.get_value("seq"))
        .and_then(Value::as_u64)
        .unwrap_or(command_seq);
    Ok(Some(chat_node(
        context,
        "manual-compaction",
        sequence_anchor(anchor),
        Value::object([
            ("command", json_value(&state.command)),
            ("compaction", json_value(&compaction)),
        ]),
    )))
}

pub(crate) fn decode<T: for<'de> Deserialize<'de>>(
    value: &Value,
) -> Result<T, ConversationAssemblerError> {
    value
        .deserialize()
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Rc<Value>, ConversationAssemblerError> {
    Value::from_serialize(value)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
