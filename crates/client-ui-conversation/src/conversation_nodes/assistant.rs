use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerNodeDefinition, AssistantBlock, ConversationAssemblerError,
    ConversationBoundaryStatus, ConversationLocation, ConversationLocationData,
    ConversationLocationDataScope, ConversationMatch, ConversationMatchResult,
    ConversationMatchRole, ConversationNodeContext, ConversationPublication, ConversationViewNode,
    ConversationVisibility, empty_assistant_block, to_assistant_block,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{
    CHAT_INTERRUPTED_ASSISTANT_OFFSET, chat_node_with, command::EventEvidence,
    conversation_coordinate, is_append_surface_event, js_string, sequence_anchor,
};

/// Per-Step Assistant definition kind and Location-data key.
pub const ASSISTANT_STEP_KIND: &str = "assistant-step";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssistantState {
    turn: u64,
    step: u64,
    blocks: Vec<Option<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_visible_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_visible_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_token_time: Option<i64>,
    hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_event: Option<EventEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
}

struct AssistantProjection {
    data: Value,
    anchor_seq: f64,
    visible: bool,
    settled: Option<Value>,
}

/// Builds the streaming, settled, and interrupted Assistant definition.
#[must_use]
pub fn conversation_assistant_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: ASSISTANT_STEP_KIND.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|event| {
            let role = if event.event_type == "step/start" {
                Some(ConversationMatchRole::Start)
            } else if event.event_type == "assistant/chunk"
                || (event.event_type == "assistant/message" && is_append_surface_event(event))
                || event.event_type == "llm/retry"
            {
                Some(ConversationMatchRole::Update)
            } else {
                None
            };
            Ok(role.map(|role| ConversationMatchResult {
                id: format!(
                    "{}:{}",
                    event
                        .data
                        .get("turn")
                        .map_or_else(|| "undefined".to_owned(), js_string),
                    event
                        .data
                        .get("step")
                        .map_or_else(|| "undefined".to_owned(), js_string)
                ),
                role,
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "step/start" {
                return Err(ConversationAssemblerError::new(
                    "assistant-step start requires step/start",
                ));
            }
            encode(&initial_state(
                required_coordinate(&accepted.event.data, "turn")?,
                required_coordinate(&accepted.event.data, "step")?,
            ))
            .map(Some)
        }),
        update: Rc::new(update_assistant),
        publication: Some(Rc::new(|accepted| Ok(publication(accepted)))),
        build_location_data: Some(Rc::new(build_location_data)),
        build_view_node: Some(Rc::new(build_view_node)),
    }
}

fn initial_state(turn: u64, step: u64) -> AssistantState {
    AssistantState {
        turn,
        step,
        blocks: Vec::new(),
        first_visible_seq: None,
        first_visible_time: None,
        first_token_time: None,
        hidden: false,
        final_event: None,
        usage: None,
    }
}

fn reset_for_retry(previous: &AssistantState) -> AssistantState {
    AssistantState {
        first_token_time: previous.first_token_time,
        hidden: true,
        ..initial_state(previous.turn, previous.step)
    }
}

fn update_assistant(
    context: &ConversationNodeContext,
    accepted: &Rc<ConversationMatch>,
) -> Result<Option<Rc<Value>>, ConversationAssemblerError> {
    let Some(previous) = context.state.as_deref() else {
        return Ok(None);
    };
    let mut state = decode(previous)?;
    match accepted.event.event_type.as_str() {
        "assistant/chunk" => update_chunk(&mut state, accepted)?,
        "assistant/message" => {
            state.blocks = message_blocks(&accepted.event.data)?
                .into_iter()
                .map(Some)
                .collect();
            state.hidden = false;
            state.final_event = Some(EventEvidence::from(accepted.as_ref()));
            state.usage = accepted.event.data.get("usage").cloned();
        }
        "llm/retry" => state = reset_for_retry(&state),
        _ => return Ok(context.state.clone()),
    }
    encode(&state).map(Some)
}

#[allow(clippy::too_many_lines)]
fn update_chunk(
    state: &mut AssistantState,
    accepted: &ConversationMatch,
) -> Result<(), ConversationAssemblerError> {
    let chunk = accepted
        .event
        .data
        .get("chunk")
        .ok_or_else(|| ConversationAssemblerError::new("assistant/chunk omitted chunk"))?;
    let chunk_type = chunk
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if chunk_type == "usage" {
        state.usage = chunk.get("usage").cloned();
        return Ok(());
    }
    let index = chunk
        .get("index")
        .and_then(conversation_coordinate)
        .and_then(|value| usize::try_from(value).ok());
    match chunk_type {
        "block-start" => set_block(
            &mut state.blocks,
            required_index(index, "block-start")?,
            assistant_block_value(&empty_assistant_block(
                chunk
                    .get("blockType")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )),
        ),
        "text-delta" | "reasoning-delta" => {
            let index = required_index(index, chunk_type)?;
            let kind = if chunk_type == "text-delta" {
                "text"
            } else {
                "reasoning"
            };
            let prefix = state
                .blocks
                .get(index)
                .and_then(Option::as_ref)
                .filter(|block| block.get("kind").and_then(Value::as_str) == Some(kind))
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            set_block(
                &mut state.blocks,
                index,
                json!({"kind": kind, "text": format!("{prefix}{}", chunk.get("text").and_then(Value::as_str).unwrap_or_default())}),
            );
        }
        "tool-call-delta" => update_tool_delta(state, chunk, index)?,
        "block-end" => {
            let index = required_index(index, "block-end")?;
            set_block(
                &mut state.blocks,
                index,
                assistant_block_value(&to_assistant_block(
                    chunk.get("block").unwrap_or(&Value::Null),
                )),
            );
        }
        _ => return Ok(()),
    }
    let blocks = compact_blocks(&state.blocks);
    let visible = has_visible_content(&blocks);
    if visible {
        state.hidden = false;
    }
    if visible && state.first_visible_seq.is_none() {
        state.first_visible_seq = Some(accepted.event.seq);
        state.first_visible_time = Some(accepted.event.time);
    }
    if state.first_token_time.is_none() && is_token_delta(chunk) {
        state.first_token_time = Some(accepted.event.time);
    }
    Ok(())
}

fn update_tool_delta(
    state: &mut AssistantState,
    chunk: &Value,
    index: Option<usize>,
) -> Result<(), ConversationAssemblerError> {
    let index = required_index(index, "tool-call-delta")?;
    let prior = state
        .blocks
        .get(index)
        .and_then(Option::as_ref)
        .filter(|block| block.get("kind").and_then(Value::as_str) == Some("tool-call"));
    let prior_id = prior
        .and_then(|block| block.get("callId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let call_id = if prior_id.is_empty() {
        chunk
            .get("id")
            .map_or_else(|| "undefined".to_owned(), js_string)
    } else {
        prior_id.to_owned()
    };
    let name = chunk
        .get("name")
        .filter(|name| !name.is_null())
        .and_then(Value::as_str)
        .or_else(|| {
            prior
                .and_then(|block| block.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_owned();
    let args = prior
        .and_then(|block| block.get("argsRaw"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let delta = chunk
        .get("argumentsDelta")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    set_block(
        &mut state.blocks,
        index,
        json!({"kind": "tool-call", "callId": call_id, "name": name, "argsRaw": format!("{args}{delta}")}),
    );
    Ok(())
}

fn publication(accepted: &ConversationMatch) -> ConversationPublication {
    if accepted.event.event_type == "step/start" {
        return ConversationPublication::None;
    }
    if accepted.event.event_type != "assistant/chunk" {
        return ConversationPublication::Immediate;
    }
    match accepted
        .event
        .data
        .get("chunk")
        .and_then(|chunk| chunk.get("type"))
        .and_then(Value::as_str)
    {
        Some("usage" | "finish") => ConversationPublication::None,
        _ => ConversationPublication::AnimationFrame,
    }
}

fn build_location_data(
    context: &ConversationNodeContext,
    scope: ConversationLocationDataScope,
) -> Result<Option<Rc<ConversationLocationData>>, ConversationAssemblerError> {
    if scope != ConversationLocationDataScope::Step {
        return Ok(None);
    }
    let Some(projected) = project_assistant(context)? else {
        return Ok(None);
    };
    let turn = projected
        .data
        .get("turn")
        .and_then(Value::as_u64)
        .ok_or_else(|| ConversationAssemblerError::new("Assistant projection omitted turn"))?;
    let step = projected.data.get("step").and_then(Value::as_u64);
    Ok(Some(Rc::new(ConversationLocationData::Step {
        turn,
        step,
        key: ASSISTANT_STEP_KIND.to_owned(),
        value: Rc::new(projected.data),
    })))
}

fn build_view_node(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<ConversationViewNode>>, ConversationAssemblerError> {
    let Some(projected) = project_assistant(context)? else {
        return Ok(None);
    };
    if projected.settled.is_none() && !projected.visible {
        let Some(state) = state_for(context)? else {
            return Ok(None);
        };
        if !state.hidden
            || !context
                .current
                .borrow()
                .get("chat")
                .is_some_and(Option::is_some)
        {
            return Ok(None);
        }
    }
    let interrupted = projected
        .settled
        .as_ref()
        .and_then(|node| node.get("interrupted"))
        .and_then(Value::as_bool)
        == Some(true);
    Ok(Some(chat_node_with(
        context,
        ASSISTANT_STEP_KIND,
        projected.anchor_seq,
        projected.data,
        None,
        Some(if interrupted || projected.visible {
            ConversationVisibility::Visible
        } else {
            ConversationVisibility::Hidden
        }),
    )))
}

fn project_assistant(
    context: &ConversationNodeContext,
) -> Result<Option<AssistantProjection>, ConversationAssemblerError> {
    let Some(state) = state_for(context)? else {
        return Ok(None);
    };
    let settled = final_node(&state, context)?;
    let blocks = settled
        .as_ref()
        .and_then(|node| node.get("blocks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| compact_blocks(&state.blocks));
    let visible = has_visible_content(&blocks);
    let interrupted = settled
        .as_ref()
        .and_then(|node| node.get("interrupted"))
        .and_then(Value::as_bool)
        == Some(true);
    let status = if interrupted {
        "interrupted"
    } else if settled.is_some() {
        "settled"
    } else {
        "running"
    };
    let first_match = context.matches.borrow().first().cloned();
    let anchor_seq = settled
        .as_ref()
        .and_then(|node| node.get("seq"))
        .and_then(Value::as_f64)
        .or_else(|| state.first_visible_seq.map(sequence_anchor))
        .or_else(|| {
            first_match
                .as_ref()
                .map(|accepted| sequence_anchor(accepted.event.seq))
        })
        .unwrap_or(0.0);
    let time = settled
        .as_ref()
        .and_then(|node| node.get("time"))
        .and_then(Value::as_i64)
        .or(state.first_visible_time)
        .or_else(|| first_match.as_ref().map(|accepted| accepted.event.time))
        .unwrap_or(0);
    let mut data = Map::from_iter([
        ("status".to_owned(), json!(status)),
        ("turn".to_owned(), json!(state.turn)),
        ("step".to_owned(), json!(state.step)),
        ("blocks".to_owned(), Value::Array(blocks)),
        ("time".to_owned(), json!(time)),
    ]);
    if let Some(usage) = state.usage {
        data.insert("usage".to_owned(), usage);
    }
    if let Some(settled) = &settled {
        data.insert("finalNode".to_owned(), settled.clone());
    }
    Ok(Some(AssistantProjection {
        data: Value::Object(data),
        anchor_seq,
        visible,
        settled,
    }))
}

fn state_for(
    context: &ConversationNodeContext,
) -> Result<Option<AssistantState>, ConversationAssemblerError> {
    if let Some(state) = context.state.as_deref() {
        return decode(state).map(Some);
    }
    fallback_state(context)
}

fn fallback_state(
    context: &ConversationNodeContext,
) -> Result<Option<AssistantState>, ConversationAssemblerError> {
    let mut state = None;
    for accepted in context.matches.borrow().iter() {
        match accepted.event.event_type.as_str() {
            "assistant/chunk" => {
                if state.is_none() {
                    state = Some(initial_state(
                        required_coordinate(&accepted.event.data, "turn")?,
                        required_coordinate(&accepted.event.data, "step")?,
                    ));
                }
                update_chunk(state.as_mut().expect("initialized"), accepted)?;
            }
            "assistant/message" => {
                if state.is_none() {
                    state = Some(initial_state(
                        required_coordinate(&accepted.event.data, "turn")?,
                        required_coordinate(&accepted.event.data, "step")?,
                    ));
                }
                let current = state.as_mut().expect("initialized");
                current.blocks = message_blocks(&accepted.event.data)?
                    .into_iter()
                    .map(Some)
                    .collect();
                current.hidden = false;
                current.final_event = Some(EventEvidence::from(accepted.as_ref()));
                current.usage = accepted.event.data.get("usage").cloned();
            }
            "llm/retry" if state.is_some() => state = state.as_ref().map(reset_for_retry),
            _ => {}
        }
    }
    Ok(state)
}

fn final_node(
    state: &AssistantState,
    context: &ConversationNodeContext,
) -> Result<Option<Value>, ConversationAssemblerError> {
    if let Some(final_event) = state
        .final_event
        .as_ref()
        .filter(|event| event.event_type == "assistant/message")
    {
        let message = final_event.data.get("message").unwrap_or(&Value::Null);
        let mut node = Map::from_iter([
            ("kind".to_owned(), json!("assistant")),
            ("seq".to_owned(), json!(final_event.seq)),
            (
                "messageId".to_owned(),
                message.get("id").cloned().unwrap_or(Value::Null),
            ),
            ("time".to_owned(), json!(final_event.time)),
            ("turn".to_owned(), json!(state.turn)),
            ("step".to_owned(), json!(state.step)),
            (
                "blocks".to_owned(),
                Value::Array(message_blocks(&final_event.data)?),
            ),
            (
                "timing".to_owned(),
                json!({
                    "stepStartTime": context.start.as_ref().map_or(Value::Null, |start| json!(start.event.time)),
                    "firstTokenTime": state.first_token_time.map_or(Value::Null, |time| json!(time)),
                    "completedTime": final_event.time,
                }),
            ),
        ]);
        if let Some(usage) = final_event.data.get("usage") {
            node.insert("usage".to_owned(), usage.clone());
        }
        return Ok(Some(Value::Object(node)));
    }
    let location = context
        .start
        .as_ref()
        .map(|start| start.location.clone())
        .or_else(|| {
            context
                .matches
                .borrow()
                .last()
                .map(|accepted| accepted.location.clone())
        });
    let boundary = location.as_ref().and_then(closed_boundary);
    let blocks = compact_blocks(&state.blocks);
    let Some((seq, time)) = boundary.filter(|_| has_interruption_evidence(&blocks)) else {
        return Ok(None);
    };
    Ok(Some(json!({
        "kind": "assistant",
        "seq": sequence_anchor(seq) + CHAT_INTERRUPTED_ASSISTANT_OFFSET,
        "time": time,
        "turn": state.turn,
        "step": state.step,
        "blocks": blocks,
        "interrupted": true,
    })))
}

fn closed_boundary(location: &ConversationLocation) -> Option<(u64, i64)> {
    if let ConversationLocation::Step { step, .. } = location
        && step.status == ConversationBoundaryStatus::Closed
        && let Some(end) = &step.end
    {
        return Some((end.seq, end.time));
    }
    match location {
        ConversationLocation::Step { turn, .. } | ConversationLocation::Turn { turn }
            if turn.status == ConversationBoundaryStatus::Closed =>
        {
            turn.end.as_ref().map(|end| (end.seq, end.time))
        }
        _ => None,
    }
}

fn message_blocks(data: &Value) -> Result<Vec<Value>, ConversationAssemblerError> {
    let content = data
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .ok_or_else(|| ConversationAssemblerError::new("assistant/message omitted content"))?;
    Ok(content
        .iter()
        .map(|block| assistant_block_value(&to_assistant_block(block)))
        .collect())
}

fn assistant_block_value(block: &AssistantBlock) -> Value {
    match block {
        AssistantBlock::Text { text } => json!({"kind": "text", "text": text}),
        AssistantBlock::Reasoning { text } => json!({"kind": "reasoning", "text": text}),
        AssistantBlock::Image { attachment } => json!({"kind": "image", "attachment": attachment}),
        AssistantBlock::ToolCall {
            call_id,
            name,
            args_raw,
        } => json!({
            "kind": "tool-call", "callId": call_id, "name": name, "argsRaw": args_raw,
        }),
        AssistantBlock::Other { block } => json!({"kind": "other", "block": block}),
    }
}

fn compact_blocks(blocks: &[Option<Value>]) -> Vec<Value> {
    blocks.iter().flatten().cloned().collect()
}

fn has_visible_content(blocks: &[Value]) -> bool {
    blocks
        .iter()
        .any(|block| match block.get("kind").and_then(Value::as_str) {
            Some("tool-call") => false,
            Some("text" | "reasoning") => block
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()),
            _ => true,
        })
}

fn has_interruption_evidence(blocks: &[Value]) -> bool {
    blocks
        .iter()
        .any(|block| match block.get("kind").and_then(Value::as_str) {
            Some("text" | "reasoning") => block
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()),
            _ => true,
        })
}

fn is_token_delta(chunk: &Value) -> bool {
    match chunk.get("type").and_then(Value::as_str) {
        Some("text-delta" | "reasoning-delta") => chunk
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some("tool-call-delta") => {
            chunk.get("name").is_some_and(|name| !name.is_null())
                || chunk
                    .get("argumentsDelta")
                    .and_then(Value::as_str)
                    .is_some_and(|delta| !delta.is_empty())
        }
        _ => false,
    }
}

fn set_block(blocks: &mut Vec<Option<Value>>, index: usize, block: Value) {
    if blocks.len() <= index {
        blocks.resize(index + 1, None);
    }
    blocks[index] = Some(block);
}

fn required_index(index: Option<usize>, kind: &str) -> Result<usize, ConversationAssemblerError> {
    index.ok_or_else(|| ConversationAssemblerError::new(format!("{kind} omitted index")))
}

fn required_coordinate(value: &Value, key: &str) -> Result<u64, ConversationAssemblerError> {
    value
        .get(key)
        .and_then(conversation_coordinate)
        .ok_or_else(|| ConversationAssemblerError::new(format!("assistant event omitted {key}")))
}

fn encode(state: &AssistantState) -> Result<Rc<Value>, ConversationAssemblerError> {
    serde_json::to_value(state)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode(value: &Value) -> Result<AssistantState, ConversationAssemblerError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}
