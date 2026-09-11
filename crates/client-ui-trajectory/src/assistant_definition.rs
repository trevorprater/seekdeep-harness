//! Trajectory Assistant streaming, settlement, request, and Turn-end Definitions.

use std::rc::Rc;

use crate::json_value::{json, null};
use indexmap::IndexMap as Map;
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, AssistantBlock, ConversationAssemblerError,
    ConversationBoundaryStatus, ConversationLocation, ConversationLocationEvent, ConversationMatch,
    ConversationMatchResult, ConversationMatchRole, ConversationNodeContext,
    ConversationPublication, empty_assistant_block, to_assistant_block,
};
use seekdeep_failure_display::display_failure_message_json as display_failure_message;
use seekdeep_lossless_json::{JsonString, JsonValue as Value};
use serde::{Deserialize, Serialize};

use crate::{TRAJECTORY_TARGET, trajectory_node_at};

/// Assistant lifecycle Definition kind.
pub const TRAJECTORY_ASSISTANT_KIND: &str = "trajectory-assistant-step";
/// Turn terminal-boundary Definition kind.
pub const TRAJECTORY_TURN_END_KIND: &str = "trajectory-turn-end";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_field_names)] // Exact provider usage wire vocabulary.
struct UsageValue {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryValue {
    message: JsonString,
    retry: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_retries: Option<u64>,
    /// The source's jittered backoff is a plain number, not an integer.
    delay_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct EventState {
    seq: u64,
    time: i64,
    #[serde(rename = "type")]
    event_type: String,
    data: Value,
}

impl From<&ConversationLocationEvent> for EventState {
    fn from(event: &ConversationLocationEvent) -> Self {
        Self {
            seq: event.seq,
            time: event.time,
            event_type: event.event_type.clone(),
            data: event.data.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssistantState {
    turn: i64,
    step: i64,
    start_seq: u64,
    start_time: i64,
    started: bool,
    saw_chunk: bool,
    blocks: Vec<Option<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_visible_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_visible_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_token_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_event: Option<EventState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<UsageValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry: Option<RetryValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step_end: Option<EventState>,
}

/// Builds Assistant lifecycle and Turn-end Definitions in source order.
#[must_use]
pub fn trajectory_assistant_definitions() -> [AssemblerNodeDefinition; 2] {
    [
        trajectory_assistant_definition(),
        trajectory_turn_end_definition(),
    ]
}

fn trajectory_assistant_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_ASSISTANT_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            let role = if event.event_type == "step/start" {
                Some(ConversationMatchRole::Start)
            } else if matches!(
                event.event_type.as_str(),
                "assistant/chunk" | "assistant/message" | "llm/retry" | "step/end"
            ) {
                Some(ConversationMatchRole::Update)
            } else {
                None
            };
            Ok(role.map(|role| ConversationMatchResult {
                id: format!(
                    "{}:{}",
                    js_member_string(&event.data, "turn"),
                    js_member_string(&event.data, "step")
                ),
                role,
            }))
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "step/start" {
                return Err(ConversationAssemblerError::new(
                    "trajectory-assistant-step start requires step/start",
                ));
            }
            let turn = required_i64(&accepted.event.data, "turn")?;
            let step = required_i64(&accepted.event.data, "step")?;
            encode(&initial_state(
                turn,
                step,
                accepted.event.seq,
                accepted.event.time,
                true,
            ))
            .map(Some)
        }),
        update: Rc::new(update_assistant),
        publication: Some(Rc::new(|accepted| Ok(assistant_publication(accepted)))),
        build_location_data: None,
        build_view_node: Some(Rc::new(build_assistant_view_node)),
    }
}

fn trajectory_turn_end_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: TRAJECTORY_TURN_END_KIND.to_owned(),
        target: Some(TRAJECTORY_TARGET.to_owned()),
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "turn/end").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, accepted, _reader| {
            if accepted.event.event_type != "turn/end" {
                return Err(ConversationAssemblerError::new(
                    "trajectory-turn-end start requires turn/end",
                ));
            }
            let reason = accepted.event.data.get_value("reason").unwrap_or(null());
            let mut state = Map::<String, Value>::from_iter([
                (
                    "turn".to_owned(),
                    accepted
                        .event
                        .data
                        .get_value("turn")
                        .cloned()
                        .unwrap_or(null().clone()),
                ),
                ("seq".to_owned(), json!(accepted.event.seq)),
                ("time".to_owned(), json!(accepted.event.time)),
            ]);
            if reason.get_value("kind").and_then(Value::as_str) == Some("error") {
                state.insert(
                    "error".to_owned(),
                    json!(display_failure_message(
                        reason.get_value("error").unwrap_or(null())
                    )),
                );
            }
            Ok(Some(Rc::new(Value::object(state))))
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            let Some(state) = context.state.as_deref() else {
                return Ok(None);
            };
            let seq = required_u64(state, "seq")?;
            let mut data = Map::<String, Value>::from_iter([
                ("kind".to_owned(), json!("turn-end")),
                (
                    "turn".to_owned(),
                    state.get_value("turn").cloned().unwrap_or(null().clone()),
                ),
                (
                    "time".to_owned(),
                    state.get_value("time").cloned().unwrap_or(null().clone()),
                ),
            ]);
            copy_present(&mut data, state, "error");
            Ok(Some(trajectory_node_at(
                context,
                u64_as_f64(seq),
                Value::object(data),
            )))
        })),
    }
}

fn initial_state(
    turn: i64,
    step: i64,
    start_seq: u64,
    start_time: i64,
    started: bool,
) -> AssistantState {
    AssistantState {
        turn,
        step,
        start_seq,
        start_time,
        started,
        saw_chunk: false,
        blocks: Vec::new(),
        first_visible_seq: None,
        first_visible_time: None,
        first_token_time: None,
        final_event: None,
        usage: None,
        retry: None,
        step_end: None,
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
            state.final_event = Some(EventState::from(accepted.event.as_ref()));
            if state.usage.is_none() {
                state.usage = accepted
                    .event
                    .data
                    .get_value("usage")
                    .cloned()
                    .map(crate::json_value::decode)
                    .transpose()
                    .map_err(|error| ConversationAssemblerError::new(error.to_string()))?;
            }
        }
        "step/end" => state.step_end = Some(EventState::from(accepted.event.as_ref())),
        "llm/retry" => state = retry_state(&state, &accepted.event.data)?,
        _ => return Ok(context.state.clone()),
    }
    encode(&state).map(Some)
}

#[allow(clippy::too_many_lines)] // Exhaustive source chunk state machine stays centralized.
fn update_chunk(
    state: &mut AssistantState,
    accepted: &ConversationMatch,
) -> Result<(), ConversationAssemblerError> {
    let chunk = accepted
        .event
        .data
        .get_value("chunk")
        .ok_or_else(|| ConversationAssemblerError::new("assistant/chunk omitted chunk"))?;
    let chunk_type = chunk
        .get_value("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if chunk_type == "usage" {
        let next: UsageValue = crate::json_value::decode(
            chunk
                .get_value("usage")
                .cloned()
                .ok_or_else(|| ConversationAssemblerError::new("usage chunk omitted usage"))?,
        )
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))?;
        state.usage = Some(add_usage(state.usage.as_ref(), &next));
        state.saw_chunk = true;
        return Ok(());
    }
    let index = chunk
        .get_value("index")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    match chunk_type {
        "block-start" => {
            set_block(
                &mut state.blocks,
                index
                    .ok_or_else(|| ConversationAssemblerError::new("block-start omitted index"))?,
                assistant_block_value(&empty_assistant_block(
                    chunk
                        .get_value("blockType")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )),
            );
        }
        "text-delta" | "reasoning-delta" => {
            let index = index.ok_or_else(|| {
                ConversationAssemblerError::new(format!("{chunk_type} omitted index"))
            })?;
            let kind = if chunk_type == "text-delta" { "text" } else { "reasoning" };
            let prefix = state.blocks.get(index).and_then(Option::as_ref)
                .filter(|block| block.get_value("kind").and_then(Value::as_str) == Some(kind))
                .and_then(|block| crate::text_value::member(block, "text"))
                .unwrap_or_default();
            let delta = crate::text_value::member(chunk, "text").unwrap_or_default();
            set_block(&mut state.blocks, index, json!({
                "kind": kind,
                "text": JsonString::concat(&[&prefix, &delta]),
            }));
        }
        "tool-call-delta" => update_tool_delta(state, chunk, index)?,
        "block-end" => {
            let index =
                index.ok_or_else(|| ConversationAssemblerError::new("block-end omitted index"))?;
            let block = chunk.get_value("block").cloned().unwrap_or(null().clone());
            set_block(
                &mut state.blocks,
                index,
                assistant_block_value(&to_assistant_block(&block)),
            );
        }
        _ => {
            state.saw_chunk = true;
            return Ok(());
        }
    }
    let compact = compact_blocks(&state.blocks);
    if state.first_visible_seq.is_none() && has_visible_content(&compact) {
        state.first_visible_seq = Some(accepted.event.seq);
        state.first_visible_time = Some(accepted.event.time);
    }
    if state.first_token_time.is_none() && is_token_delta(chunk) {
        state.first_token_time = Some(accepted.event.time);
    }
    state.saw_chunk = true;
    Ok(())
}

fn update_tool_delta(
    state: &mut AssistantState,
    chunk: &Value,
    index: Option<usize>,
) -> Result<(), ConversationAssemblerError> {
    let index =
        index.ok_or_else(|| ConversationAssemblerError::new("tool-call-delta omitted index"))?;
    let previous = state.blocks.get(index).and_then(Option::as_ref);
    let is_tool = previous
        .and_then(|block| block.get_value("kind"))
        .and_then(Value::as_str)
        == Some("tool-call");
    let prior = is_tool.then_some(previous).flatten();
    let prior_id = prior.and_then(|block| crate::text_value::member(block, "callId"))
        .unwrap_or_default();
    let call_id = if prior_id.is_empty() {
        chunk.get_value("id").map_or_else(|| JsonString::from("undefined"), crate::json_value::js_text)
    } else {
        prior_id
    };
    let name = crate::text_value::member(chunk, "name")
        .or_else(|| prior.and_then(|block| crate::text_value::member(block, "name")))
        .unwrap_or_default();
    let args = prior.and_then(|block| crate::text_value::member(block, "argsRaw"))
        .unwrap_or_default();
    let delta = crate::text_value::member(chunk, "argumentsDelta").unwrap_or_default();
    set_block(
        &mut state.blocks,
        index,
        json!({
            "kind": "tool-call",
            "callId": call_id,
            "name": name,
            "argsRaw": JsonString::concat(&[&args, &delta]),
        }),
    );
    Ok(())
}

fn retry_state(
    previous: &AssistantState,
    data: &Value,
) -> Result<AssistantState, ConversationAssemblerError> {
    let mut state = initial_state(
        previous.turn,
        previous.step,
        previous.start_seq,
        previous.start_time,
        true,
    );
    state.first_token_time = previous.first_token_time;
    state.usage.clone_from(&previous.usage);
    state.retry = Some(RetryValue {
        message: display_failure_message(data.get_value("failure").unwrap_or(null())),
        retry: required_u64(data, "retry")?,
        max_retries: (data.get_value("mode").and_then(Value::as_str) == Some("normal"))
            .then(|| data.get_value("maxRetries").and_then(Value::as_u64))
            .flatten(),
        delay_ms: required_number(data, "delayMs")?,
    });
    Ok(state)
}

fn assistant_publication(accepted: &ConversationMatch) -> ConversationPublication {
    if accepted.event.event_type == "step/start" {
        return ConversationPublication::None;
    }
    if accepted.event.event_type != "assistant/chunk" {
        return ConversationPublication::Immediate;
    }
    let chunk_type = accepted
        .event
        .data
        .get_value("chunk")
        .and_then(|chunk| chunk.get_value("type"))
        .and_then(Value::as_str);
    if matches!(chunk_type, Some("usage" | "finish")) {
        ConversationPublication::None
    } else {
        ConversationPublication::AnimationFrame
    }
}

fn build_assistant_view_node(
    context: &ConversationNodeContext,
) -> Result<Option<Rc<seekdeep_client_runtime::ConversationViewNode>>, ConversationAssemblerError> {
    let state = match context.state.as_deref() {
        Some(state) => Some(decode(state)?),
        None => fallback_state(context)?,
    };
    let Some(state) = state else {
        return Ok(None);
    };
    let node = final_node(&state, context)?;
    let boundary = closed_boundary(&state, context);
    let partial = if node.is_none() && boundary.is_none() && state.saw_chunk {
        Some(json!({
            "turn": state.turn,
            "step": state.step,
            "blocks": compact_blocks(&state.blocks),
        }))
    } else {
        None
    };
    let request = assistant_request(&state, node.as_ref(), boundary);
    if node.is_none() && partial.is_none() && request.is_none() {
        return Ok(None);
    }
    let mut data = Map::<String, Value>::from_iter([
        ("kind".to_owned(), json!("assistant")),
        ("partial".to_owned(), partial.unwrap_or(null().clone())),
    ]);
    if let Some(node) = node {
        data.insert("node".to_owned(), node);
    }
    if let Some(request) = request {
        data.insert("request".to_owned(), request);
    }
    Ok(Some(trajectory_node_at(
        context,
        u64_as_f64(state.start_seq),
        Value::object(data),
    )))
}

fn fallback_state(
    context: &ConversationNodeContext,
) -> Result<Option<AssistantState>, ConversationAssemblerError> {
    let mut state = None;
    for accepted in context.matches.borrow().iter() {
        let event = &accepted.event;
        match event.event_type.as_str() {
            "assistant/chunk" => {
                if state.is_none() {
                    state = Some(initial_state(
                        required_i64(&event.data, "turn")?,
                        required_i64(&event.data, "step")?,
                        event.seq,
                        event.time,
                        false,
                    ));
                }
                update_chunk(state.as_mut().expect("initialized"), accepted)?;
            }
            "assistant/message" => {
                if state.is_none() {
                    state = Some(initial_state(
                        required_i64(&event.data, "turn")?,
                        required_i64(&event.data, "step")?,
                        event.seq,
                        event.time,
                        false,
                    ));
                }
                let current = state.as_mut().expect("initialized");
                current.blocks = message_blocks(&event.data)?.into_iter().map(Some).collect();
                current.final_event = Some(EventState::from(event.as_ref()));
                if current.usage.is_none() {
                    current.usage = event
                        .data
                        .get_value("usage")
                        .cloned()
                        .map(crate::json_value::decode)
                        .transpose()
                        .map_err(|error| ConversationAssemblerError::new(error.to_string()))?;
                }
            }
            "step/end" if state.is_some() => {
                state.as_mut().expect("present").step_end = Some(EventState::from(event.as_ref()));
            }
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
        let message = final_event.data.get_value("message").unwrap_or(null());
        let source = message.get_value("source").unwrap_or(null());
        let mut node = Map::<String, Value>::from_iter([
            ("kind".to_owned(), json!("assistant")),
            ("seq".to_owned(), json!(final_event.seq)),
            (
                "messageId".to_owned(),
                message.get_value("id").cloned().unwrap_or(null().clone()),
            ),
            ("time".to_owned(), json!(final_event.time)),
            ("turn".to_owned(), json!(state.turn)),
            ("step".to_owned(), json!(state.step)),
            (
                "blocks".to_owned(),
                Value::array(&message_blocks(&final_event.data)?),
            ),
            (
                "provenance".to_owned(),
                json!({
                    "provider": source.get_value("provider").cloned().unwrap_or(null().clone()),
                    "model": source.get_value("model").cloned().unwrap_or(null().clone()),
                }),
            ),
            (
                "timing".to_owned(),
                json!({
                    "stepStartTime": state.started.then_some(state.start_time),
                    "firstTokenTime": state.first_token_time,
                    "completedTime": final_event.time,
                }),
            ),
        ]);
        copy_present(&mut node, &final_event.data, "usage");
        return Ok(Some(Value::object(node)));
    }
    let boundary = closed_boundary(state, context);
    let blocks = compact_blocks(&state.blocks);
    let Some((seq, time)) = boundary.filter(|_| has_interruption_evidence(&blocks)) else {
        return Ok(None);
    };
    Ok(Some(json!({
        "kind": "assistant",
        "seq": u64_as_f64(seq) - 0.9,
        "time": time,
        "turn": state.turn,
        "step": state.step,
        "blocks": blocks,
        "interrupted": true,
    })))
}

fn assistant_request(
    state: &AssistantState,
    node: Option<&Value>,
    boundary: Option<(u64, i64)>,
) -> Option<Value> {
    if !state.started {
        return None;
    }
    let interrupted = node
        .and_then(|node| node.get_value("interrupted"))
        .and_then(Value::as_bool)
        == Some(true);
    let status = if node.is_some() && !interrupted {
        "complete"
    } else if state.retry.is_some() || boundary.is_some() {
        "error"
    } else {
        "running"
    };
    let completed_at = node
        .and_then(|node| node.get_value("time"))
        .cloned()
        .or_else(|| boundary.map(|(_, time)| json!(time)))
        .unwrap_or(null().clone());
    let mut request = Map::<String, Value>::from_iter([
        ("purpose".to_owned(), json!("assistant")),
        ("startSeq".to_owned(), json!(state.start_seq)),
        ("turn".to_owned(), json!(state.turn)),
        ("step".to_owned(), json!(state.step)),
        ("startedAt".to_owned(), json!(state.start_time)),
        ("completedAt".to_owned(), completed_at),
        ("status".to_owned(), json!(status)),
    ]);
    if let Some(retry) = &state.retry {
        request.insert("error".to_owned(), json!(retry.message));
        request.insert("retry".to_owned(), json!(retry.retry));
        if let Some(max_retries) = retry.max_retries {
            request.insert("maxRetries".to_owned(), json!(max_retries));
        }
        request.insert("retryDelayMs".to_owned(), json!(retry.delay_ms));
    }
    if let Some(node) = node.filter(|_| !interrupted) {
        if let Some(seq) = node.get_value("seq") {
            request.insert("resultSeq".to_owned(), seq.clone());
        }
        copy_present(&mut request, node, "provenance");
    }
    if let Some(usage) = &state.usage {
        request.insert("usage".to_owned(), Value::from_serialize(usage).ok()?);
    }
    Some(Value::object(request))
}

fn closed_boundary(
    state: &AssistantState,
    context: &ConversationNodeContext,
) -> Option<(u64, i64)> {
    if let Some(end) = state
        .step_end
        .as_ref()
        .filter(|event| event.event_type == "step/end")
    {
        return Some((end.seq, end.time));
    }
    let matches = context.matches.borrow();
    let location = context
        .start
        .as_ref()
        .map(|start| &start.location)
        .or_else(|| matches.last().map(|accepted| &accepted.location))?;
    if let ConversationLocation::Step { step, .. } = location
        && matches!(step.status, ConversationBoundaryStatus::Closed)
        && let Some(end) = &step.end
    {
        return Some((end.seq, end.time));
    }
    match location {
        ConversationLocation::Step { turn, .. } | ConversationLocation::Turn { turn }
            if matches!(turn.status, ConversationBoundaryStatus::Closed) =>
        {
            turn.end.as_ref().map(|end| (end.seq, end.time))
        }
        _ => None,
    }
}

fn message_blocks(data: &Value) -> Result<Vec<Value>, ConversationAssemblerError> {
    let content = data
        .get_value("message")
        .and_then(|message| message.get_value("content"))
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
        AssistantBlock::Image { attachment } => {
            json!({"kind": "image", "attachment": attachment})
        }
        AssistantBlock::ToolCall {
            call_id,
            name,
            args_raw,
        } => json!({
            "kind": "tool-call",
            "callId": call_id,
            "name": name,
            "argsRaw": args_raw,
        }),
        AssistantBlock::Other { block } => json!({"kind": "other", "block": block}),
    }
}

fn compact_blocks(blocks: &[Option<Value>]) -> Vec<Value> {
    blocks.iter().flatten().cloned().collect()
}

fn has_visible_content(blocks: &[Value]) -> bool {
    blocks.iter().any(
        |block| match block.get_value("kind").and_then(Value::as_str) {
            Some("tool-call") => false,
            Some("text" | "reasoning") => block
                .get_value("text")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .is_some_and(|text| !text.trim().is_empty()),
            _ => true,
        },
    )
}

fn has_interruption_evidence(blocks: &[Value]) -> bool {
    blocks.iter().any(
        |block| match block.get_value("kind").and_then(Value::as_str) {
            Some("text" | "reasoning") => block
                .get_value("text")
                .and_then(|value| value.deserialize::<JsonString>().ok())
                .is_some_and(|text| !text.trim().is_empty()),
            _ => true,
        },
    )
}

fn is_token_delta(chunk: &Value) -> bool {
    match chunk.get_value("type").and_then(Value::as_str) {
        Some("text-delta" | "reasoning-delta") => chunk
            .get_value("text")
            .and_then(|value| value.deserialize::<JsonString>().ok())
            .is_some_and(|text| !text.is_empty()),
        Some("tool-call-delta") => {
            chunk.get_value("name").is_some_and(|name| !name.is_null())
                || chunk
                    .get_value("argumentsDelta")
                    .and_then(|value| value.deserialize::<JsonString>().ok())
                    .is_some_and(|delta| !delta.is_empty())
        }
        _ => false,
    }
}

fn add_usage(current: Option<&UsageValue>, next: &UsageValue) -> UsageValue {
    UsageValue {
        input_tokens: current.map_or(0, |usage| usage.input_tokens) + next.input_tokens,
        output_tokens: current.map_or(0, |usage| usage.output_tokens) + next.output_tokens,
        cache_read_tokens: optional_sum(
            current.and_then(|usage| usage.cache_read_tokens),
            next.cache_read_tokens,
        ),
        cache_write_tokens: optional_sum(
            current.and_then(|usage| usage.cache_write_tokens),
            next.cache_write_tokens,
        ),
        reasoning_tokens: optional_sum(
            current.and_then(|usage| usage.reasoning_tokens),
            next.reasoning_tokens,
        ),
    }
}

fn optional_sum(current: Option<u64>, next: Option<u64>) -> Option<u64> {
    (current.is_some() || next.is_some()).then(|| current.unwrap_or(0) + next.unwrap_or(0))
}

fn set_block(blocks: &mut Vec<Option<Value>>, index: usize, block: Value) {
    if blocks.len() <= index {
        blocks.resize(index + 1, None);
    }
    blocks[index] = Some(block);
}

fn copy_present(output: &mut Map<String, Value>, input: &Value, key: &str) {
    if let Some(value) = input.get_value(key) {
        output.insert(key.to_owned(), value.clone());
    }
}

fn required_i64(value: &Value, key: &str) -> Result<i64, ConversationAssemblerError> {
    value
        .get_value(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ConversationAssemblerError::new(format!("assistant event omitted {key}")))
}

fn required_number(value: &Value, key: &str) -> Result<f64, ConversationAssemblerError> {
    value
        .get_value(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| ConversationAssemblerError::new(format!("assistant event omitted {key}")))
}

fn required_u64(value: &Value, key: &str) -> Result<u64, ConversationAssemblerError> {
    value
        .get_value(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ConversationAssemblerError::new(format!("assistant event omitted {key}")))
}

fn js_member_string(value: &Value, key: &str) -> String {
    value
        .get_value(key)
        .map_or_else(|| "undefined".to_owned(), js_string)
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

fn encode(state: &AssistantState) -> Result<Rc<Value>, ConversationAssemblerError> {
    Value::from_serialize(state)
        .map(Rc::new)
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn decode(value: &Value) -> Result<AssistantState, ConversationAssemblerError> {
    value
        .deserialize()
        .map_err(|error| ConversationAssemblerError::new(error.to_string()))
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
