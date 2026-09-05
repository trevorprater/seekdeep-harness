//! Anthropic Messages native protocol engine.

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::StreamExt as _;
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use seekdeep_llm::CallId;
use seekdeep_llm_deepseek::sse::{ByteStream, parse_sse};
use serde_json::{Map, Value, json};

use crate::{
    adapter::{BoxPiEventStream, PiExecutionRequest, PiProtocolExecutor},
    catalog::{PiModel, PiThinkingLevel},
    config::PiCacheRetention,
    context::{PiContext, PiMessage, PiToolResultMessage, PiUserContent, PiUserContentBlock},
    replay::{
        PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiResponseId, PiStopReason, PiUsage,
    },
    stream::{PiAssistantEvent, PiToolCall},
};

/// Reqwest-backed Anthropic Messages engine.
#[derive(Clone, Debug)]
pub struct AnthropicMessagesExecutor {
    http: reqwest::Client,
}

impl AnthropicMessagesExecutor {
    /// Creates an executor using one reusable HTTP client.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

impl PiProtocolExecutor for AnthropicMessagesExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        if request.model.api.as_str() != "anthropic-messages" {
            anyhow::bail!(
                "native Anthropic Messages executor cannot dispatch api \"{}\"",
                request.model.api.as_str()
            );
        }
        let has_auth_header = request
            .model
            .extra
            .get("headers")
            .and_then(Value::as_object)
            .is_some_and(|headers| {
                headers.iter().any(|(name, value)| {
                    is_auth_header(name)
                        && value.as_str().is_some_and(|value| !value.trim().is_empty())
                })
            })
            || request
                .options
                .headers
                .iter()
                .any(|(name, value)| is_auth_header(name) && !value.trim().is_empty());
        if request.options.api_key.as_deref().is_none_or(str::is_empty) && !has_auth_header {
            anyhow::bail!(
                "No API key for provider: {}",
                request.model.provider.as_str()
            );
        }
        let output = Arc::new(Mutex::new(empty_assistant(&request.model)));
        let signal = request.options.signal.clone();
        let native = native_events(self.http.clone(), request, output.clone());
        Ok(Box::pin(async_stream::stream! {
            futures::pin_mut!(native);
            while let Some(event) = native.next().await {
                match event {
                    Ok(event) => yield Ok(event),
                    Err(error) => {
                        let mut failed = output.lock().clone();
                        failed.stop_reason = if signal.is_aborted() { PiStopReason::Aborted } else { PiStopReason::Error };
                        failed.error_message = Some(error.to_string());
                        yield Ok(PiAssistantEvent::Error { reason: failed.stop_reason, error: failed });
                        return;
                    }
                }
            }
        }))
    }
}

fn is_auth_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("x-api-key")
        || name.eq_ignore_ascii_case("cf-aig-authorization")
}

#[derive(Clone, Debug)]
struct BlockSlot {
    content_index: usize,
    partial: String,
}

#[allow(clippy::too_many_lines)] // Closed source event machine in wire order.
fn native_events(
    http: reqwest::Client,
    request: PiExecutionRequest,
    shared: Arc<Mutex<PiAssistantMessage>>,
) -> BoxPiEventStream {
    Box::pin(async_stream::try_stream! {
        let oauth = request.options.api_key.as_deref().is_some_and(|key| key.contains("sk-ant-oat"));
        let body = build_request(&request.model, &request.context, &request.options, oauth);
        let url = format!("{}/v1/messages", request.model.base_url.trim_end_matches('/'));
        let headers = request_headers(&request, oauth)?;
        let mut builder = http.post(url).headers(headers)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body)?);
        if let Some(timeout_ms) = request.options.timeout_ms { builder = builder.timeout(Duration::from_millis(timeout_ms)); }
        let response_result: anyhow::Result<reqwest::Response> = tokio::select! {
            biased;
            () = request.options.signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
            response = builder.send() => response.map_err(anyhow::Error::from),
        };
        let response = response_result?;
        let response = if response.status().is_success() { response } else {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text).ok()
                .and_then(|body| body.get("error")?.get("message")?.as_str().map(str::to_owned))
                .filter(|message| !message.is_empty()).unwrap_or(text);
            Err::<reqwest::Response, _>(anyhow::anyhow!("Anthropic API error ({status}): {detail}"))?
        };
        let bytes: ByteStream = Box::pin(response.bytes_stream().map(|result| result.map_err(anyhow::Error::from)));
        let mut payloads = parse_sse(bytes, None);
        let mut output = shared.lock().clone();
        let mut slots = HashMap::<i64, BlockSlot>::new();
        let mut saw_start = false;
        yield PiAssistantEvent::Start { partial: output.clone() };
        loop {
            let payload = match payloads.next().await {
                Some(Ok(payload)) => payload,
                Some(Err(error)) if error.to_string().contains("without [DONE]") && saw_start => {
                    Err::<String, _>(anyhow::anyhow!("Anthropic stream ended before message_stop"))?
                }
                Some(Err(error)) => Err::<String, _>(error)?,
                None if saw_start => Err::<String, _>(anyhow::anyhow!("Anthropic stream ended before message_stop"))?,
                None => {
                    yield PiAssistantEvent::Done { reason: output.stop_reason, message: output };
                    return;
                }
            };
            let event: Value = serde_json::from_str(&payload)?;
            match event.get("type").and_then(Value::as_str).unwrap_or_default() {
                "message_start" => {
                    saw_start = true;
                    let message = event.get("message").unwrap_or(&Value::Null);
                    output.response_id = message.get("id").and_then(Value::as_str).map(PiResponseId::new);
                    update_usage(&mut output.usage, message.get("usage"));
                }
                "content_block_start" => {
                    let wire_index = integer(event.get("index"));
                    let block = event.get("content_block").unwrap_or(&Value::Null);
                    if let Some(slot) = start_block(&mut output, wire_index, block, oauth, request.context.tools.as_deref()) {
                        slots.insert(wire_index, slot.clone());
                        match &output.content[slot.content_index] {
                            PiAssistantBlock::Text { .. } => yield PiAssistantEvent::TextStart {
                                content_index: index_u64(slot.content_index), partial: output.clone(),
                            },
                            PiAssistantBlock::Thinking { .. } => yield PiAssistantEvent::ThinkingStart {
                                content_index: index_u64(slot.content_index), partial: output.clone(),
                            },
                            PiAssistantBlock::ToolCall { .. } => yield PiAssistantEvent::ToolCallStart {
                                content_index: index_u64(slot.content_index), partial: output.clone(),
                            },
                        }
                    }
                }
                "content_block_delta" => {
                    let wire_index = integer(event.get("index"));
                    let Some(slot) = slots.get_mut(&wire_index) else { continue };
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    match delta.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "text_delta" => {
                            let text = delta.get("text").and_then(Value::as_str).unwrap_or_default();
                            if let PiAssistantBlock::Text { text: output_text, .. } = &mut output.content[slot.content_index] {
                                output_text.push_str(text);
                            }
                            yield PiAssistantEvent::TextDelta {
                                content_index: index_u64(slot.content_index), delta: text.to_owned(), partial: output.clone(),
                            };
                        }
                        "thinking_delta" => {
                            let text = delta.get("thinking").and_then(Value::as_str).unwrap_or_default();
                            if let PiAssistantBlock::Thinking { thinking, .. } = &mut output.content[slot.content_index] {
                                thinking.push_str(text);
                            }
                            yield PiAssistantEvent::ThinkingDelta {
                                content_index: index_u64(slot.content_index), delta: text.to_owned(), partial: output.clone(),
                            };
                        }
                        "input_json_delta" => {
                            let fragment = delta.get("partial_json").and_then(Value::as_str).unwrap_or_default();
                            slot.partial.push_str(fragment);
                            set_tool_arguments(&mut output.content[slot.content_index], &slot.partial);
                            yield PiAssistantEvent::ToolCallDelta {
                                content_index: index_u64(slot.content_index), delta: fragment.to_owned(), partial: output.clone(),
                            };
                        }
                        "signature_delta" => {
                            let signature = delta.get("signature").and_then(Value::as_str).unwrap_or_default();
                            if let PiAssistantBlock::Thinking { thinking_signature, .. } = &mut output.content[slot.content_index] {
                                thinking_signature.get_or_insert_with(String::new).push_str(signature);
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    let wire_index = integer(event.get("index"));
                    if let Some(slot) = slots.remove(&wire_index) {
                        match output.content[slot.content_index].clone() {
                            PiAssistantBlock::Text { text, .. } => yield PiAssistantEvent::TextEnd {
                                content_index: index_u64(slot.content_index), content: text, partial: output.clone(),
                            },
                            PiAssistantBlock::Thinking { thinking, .. } => yield PiAssistantEvent::ThinkingEnd {
                                content_index: index_u64(slot.content_index), content: thinking, partial: output.clone(),
                            },
                            PiAssistantBlock::ToolCall { id, name, thought_signature, .. } => {
                                let arguments = parse_arguments(&slot.partial);
                                output.content[slot.content_index] = PiAssistantBlock::ToolCall {
                                    id: id.clone(), name: name.clone(), arguments: arguments.clone(), thought_signature: thought_signature.clone(),
                                };
                                yield PiAssistantEvent::ToolCallEnd {
                                    content_index: index_u64(slot.content_index),
                                    tool_call: PiToolCall { id, name, arguments, thought_signature }, partial: output.clone(),
                                };
                            }
                        }
                    }
                }
                "message_delta" => {
                    if let Some(reason) = event.get("delta").and_then(|value| value.get("stop_reason")).and_then(Value::as_str) {
                        let (stop, message) = map_stop_reason(reason, event.get("delta").and_then(|value| value.get("stop_details")));
                        output.stop_reason = stop;
                        output.error_message = message;
                    }
                    update_usage(&mut output.usage, event.get("usage"));
                }
                "message_stop" => {
                    publish(&shared, &output);
                    if request.options.signal.is_aborted() { Err::<(), _>(anyhow::anyhow!("Request was aborted"))?; }
                    if output.stop_reason == PiStopReason::Error {
                        Err::<(), _>(anyhow::anyhow!("{}", output.error_message.as_deref().unwrap_or("An unknown error occurred")))?;
                    }
                    yield PiAssistantEvent::Done { reason: output.stop_reason, message: output };
                    return;
                }
                "error" => Err::<(), _>(anyhow::anyhow!("{event}"))?,
                _ => {}
            }
            publish(&shared, &output);
        }
    })
}

fn build_request(
    model: &PiModel,
    context: &PiContext,
    options: &crate::adapter::PiStreamOptions,
    oauth: bool,
) -> Value {
    let mut root = Map::from_iter([
        (
            "model".to_owned(),
            Value::String(model.id.as_str().to_owned()),
        ),
        (
            "messages".to_owned(),
            Value::Array(convert_messages(context)),
        ),
        (
            "max_tokens".to_owned(),
            Value::from(options.max_tokens.unwrap_or(model.max_tokens)),
        ),
        ("stream".to_owned(), Value::Bool(true)),
    ]);
    let cache = options.cache_retention != Some(PiCacheRetention::None);
    if oauth {
        let mut system = vec![text_with_cache(
            "You are Claude Code, Anthropic's official CLI for Claude.",
            cache,
        )];
        if let Some(prompt) = &context.system_prompt {
            system.push(text_with_cache(prompt, cache));
        }
        root.insert("system".to_owned(), Value::Array(system));
    } else if let Some(prompt) = &context.system_prompt {
        root.insert(
            "system".to_owned(),
            Value::Array(vec![text_with_cache(prompt, cache)]),
        );
    }
    if options.reasoning.is_none()
        && let Some(temperature) = options.temperature
    {
        root.insert("temperature".to_owned(), json!(temperature));
    }
    if model.reasoning {
        if let Some(level) = options.reasoning {
            if model
                .compat
                .as_ref()
                .and_then(|compat| compat.get("forceAdaptiveThinking"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                root.insert(
                    "thinking".to_owned(),
                    json!({"type":"adaptive","display":"summarized"}),
                );
                root.insert(
                    "output_config".to_owned(),
                    json!({"effort":mapped_effort(model, level)}),
                );
            } else {
                let budget = thinking_budget(options, level).min(
                    options
                        .max_tokens
                        .unwrap_or(model.max_tokens)
                        .saturating_sub(1024),
                );
                root.insert(
                    "thinking".to_owned(),
                    json!({"type":"enabled","budget_tokens":budget.max(1),"display":"summarized"}),
                );
            }
        } else if model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get("off"))
            != Some(&Value::Null)
        {
            root.insert("thinking".to_owned(), json!({"type":"disabled"}));
        }
    }
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        root.insert("tools".to_owned(), Value::Array(tools.iter().map(|tool| json!({
            "name":tool.name,"description":tool.description,"input_schema":tool.parameters
        })).collect()));
    }
    Value::Object(root)
}

fn convert_messages(pi_context: &PiContext) -> Vec<Value> {
    let mut messages = Vec::new();
    for message in &pi_context.messages {
        match message {
            PiMessage::User(message) => {
                let wire_content = match &message.content {
                    PiUserContent::Text(text) => Value::String(text.clone()),
                    PiUserContent::Blocks(blocks) => {
                        Value::Array(blocks.iter().map(anthropic_input).collect())
                    }
                };
                if !wire_content
                    .as_str()
                    .is_some_and(|text| text.trim().is_empty())
                {
                    messages.push(json!({"role":"user","content":wire_content}));
                }
            }
            PiMessage::Assistant(message) => {
                let mut wire_content = Vec::new();
                for block in &message.content {
                    match block {
                        PiAssistantBlock::Text { text, .. } => wire_content.push(json!({"type":"text","text":text})),
                        PiAssistantBlock::Thinking { thinking, thinking_signature, redacted } => {
                            if *redacted == Some(true) {
                                wire_content.push(json!({"type":"redacted_thinking","data":thinking_signature}));
                            } else {
                                wire_content.push(json!({"type":"thinking","thinking":thinking,"signature":thinking_signature.as_deref().unwrap_or("")}));
                            }
                        }
                        PiAssistantBlock::ToolCall { id, name, arguments, .. } => wire_content.push(json!({
                            "type":"tool_use","id":normalize_id(id.as_str()),"name":name,"input":arguments
                        })),
                    }
                }
                if !wire_content.is_empty() {
                    messages.push(json!({"role":"assistant","content":wire_content}));
                }
            }
            PiMessage::ToolResult(message) => messages.push(json!({
                "role":"user","content":[tool_result(message)]
            })),
        }
    }
    messages
}

fn anthropic_input(block: &PiUserContentBlock) -> Value {
    match block {
        PiUserContentBlock::Text { text } => json!({"type":"text","text":text}),
        PiUserContentBlock::Image { data, mime_type } => json!({
            "type":"image","source":{"type":"base64","media_type":mime_type,"data":data}
        }),
    }
}

fn tool_result(message: &PiToolResultMessage) -> Value {
    let has_images = message
        .content
        .iter()
        .any(|block| matches!(block, PiUserContentBlock::Image { .. }));
    let content = if has_images {
        let mut blocks = message
            .content
            .iter()
            .map(anthropic_input)
            .collect::<Vec<_>>();
        if !message
            .content
            .iter()
            .any(|block| matches!(block, PiUserContentBlock::Text { .. }))
        {
            blocks.insert(0, json!({"type":"text","text":"(see attached image)"}));
        }
        Value::Array(blocks)
    } else {
        Value::String(
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    PiUserContentBlock::Text { text } => Some(text.as_str()),
                    PiUserContentBlock::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    json!({"type":"tool_result","tool_use_id":normalize_id(message.tool_call_id.as_str()),"content":content,"is_error":message.is_error})
}

fn request_headers(request: &PiExecutionRequest, oauth: bool) -> anyhow::Result<HeaderMap> {
    let mut values = Map::from_iter([
        (
            "accept".to_owned(),
            Value::String("application/json".to_owned()),
        ),
        (
            "anthropic-version".to_owned(),
            Value::String("2023-06-01".to_owned()),
        ),
        (
            "anthropic-dangerous-direct-browser-access".to_owned(),
            Value::String("true".to_owned()),
        ),
    ]);
    if let Some(headers) = request
        .model
        .extra
        .get("headers")
        .and_then(Value::as_object)
    {
        values.extend(headers.clone());
    }
    for (name, value) in &request.options.headers {
        values.insert(name.clone(), Value::String(value.clone()));
    }
    let mut headers = HeaderMap::new();
    if let Some(key) = &request.options.api_key {
        if oauth {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))?,
            );
        } else {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(key)?,
            );
        }
    }
    for (name, value) in values {
        let Some(value) = value.as_str() else {
            continue;
        };
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(headers)
}

fn start_block(
    output: &mut PiAssistantMessage,
    wire_index: i64,
    block: &Value,
    _oauth: bool,
    _tools: Option<&[crate::context::PiTool]>,
) -> Option<BlockSlot> {
    let content_index = output.content.len();
    let (native, partial) = match block.get("type").and_then(Value::as_str)? {
        "text" => (
            PiAssistantBlock::Text {
                text: String::new(),
                text_signature: None,
            },
            String::new(),
        ),
        "thinking" => (
            PiAssistantBlock::Thinking {
                thinking: String::new(),
                thinking_signature: Some(String::new()),
                redacted: None,
            },
            String::new(),
        ),
        "redacted_thinking" => (
            PiAssistantBlock::Thinking {
                thinking: "[Reasoning redacted]".to_owned(),
                thinking_signature: block.get("data").and_then(Value::as_str).map(str::to_owned),
                redacted: Some(true),
            },
            String::new(),
        ),
        "tool_use" => (
            PiAssistantBlock::ToolCall {
                id: CallId::new(block.get("id").and_then(Value::as_str).unwrap_or_default()),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                arguments: block
                    .get("input")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default(),
                thought_signature: None,
            },
            String::new(),
        ),
        _ => return None,
    };
    output.content.push(native);
    let _ = wire_index;
    Some(BlockSlot {
        content_index,
        partial,
    })
}

fn update_usage(usage: &mut PiUsage, value: Option<&Value>) {
    let Some(value) = value else { return };
    if value.get("input_tokens").is_some() {
        usage.input = number(value.get("input_tokens"));
    }
    if value.get("output_tokens").is_some() {
        usage.output = number(value.get("output_tokens"));
    }
    if value.get("cache_read_input_tokens").is_some() {
        usage.cache_read = number(value.get("cache_read_input_tokens"));
    }
    if value.get("cache_creation_input_tokens").is_some() {
        usage.cache_write = number(value.get("cache_creation_input_tokens"));
    }
    usage.total_tokens = usage
        .input
        .saturating_add(usage.output)
        .saturating_add(usage.cache_read)
        .saturating_add(usage.cache_write);
}

fn map_stop_reason(reason: &str, details: Option<&Value>) -> (PiStopReason, Option<String>) {
    match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => (PiStopReason::Stop, None),
        "max_tokens" => (PiStopReason::Length, None),
        "tool_use" => (PiStopReason::ToolUse, None),
        "refusal" => (
            PiStopReason::Error,
            Some(
                details
                    .and_then(|value| value.get("explanation"))
                    .and_then(Value::as_str)
                    .unwrap_or("The model refused to complete the request")
                    .to_owned(),
            ),
        ),
        "sensitive" => (PiStopReason::Error, None),
        other => (
            PiStopReason::Error,
            Some(format!("Unhandled stop reason: {other}")),
        ),
    }
}

fn thinking_budget(options: &crate::adapter::PiStreamOptions, level: PiThinkingLevel) -> u64 {
    let configured = options
        .thinking_budgets
        .as_ref()
        .and_then(|budgets| match level {
            PiThinkingLevel::Minimal => budgets.minimal,
            PiThinkingLevel::Low => budgets.low,
            PiThinkingLevel::Medium => budgets.medium,
            PiThinkingLevel::High
            | PiThinkingLevel::XHigh
            | PiThinkingLevel::Max
            | PiThinkingLevel::Off => budgets.high,
        });
    configured
        .and_then(|value| format!("{value:.0}").parse().ok())
        .unwrap_or(match level {
            PiThinkingLevel::Minimal | PiThinkingLevel::Off => 1_024,
            PiThinkingLevel::Low => 2_048,
            PiThinkingLevel::Medium => 8_192,
            PiThinkingLevel::High | PiThinkingLevel::XHigh | PiThinkingLevel::Max => 16_384,
        })
}

fn mapped_effort(model: &PiModel, level: PiThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(level.as_str()))
        .and_then(Value::as_str)
        .map_or_else(
            || match level {
                PiThinkingLevel::Minimal | PiThinkingLevel::Low => "low".to_owned(),
                PiThinkingLevel::Medium => "medium".to_owned(),
                PiThinkingLevel::High
                | PiThinkingLevel::XHigh
                | PiThinkingLevel::Max
                | PiThinkingLevel::Off => "high".to_owned(),
            },
            str::to_owned,
        )
}

fn text_with_cache(text: &str, cache: bool) -> Value {
    if cache {
        json!({"type":"text","text":text,"cache_control":{"type":"ephemeral"}})
    } else {
        json!({"type":"text","text":text})
    }
}
fn normalize_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}
fn set_tool_arguments(block: &mut PiAssistantBlock, raw: &str) {
    if let PiAssistantBlock::ToolCall { arguments, .. } = block {
        *arguments = parse_arguments(raw);
    }
}
fn parse_arguments(raw: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}
fn empty_assistant(model: &PiModel) -> PiAssistantMessage {
    PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        usage: PiUsage::default(),
        stop_reason: PiStopReason::Stop,
        error_message: None,
        timestamp: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default(),
    }
}
fn publish(shared: &Mutex<PiAssistantMessage>, output: &PiAssistantMessage) {
    *shared.lock() = output.clone();
}
fn number(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or_default()
}
fn integer(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or_default()
}
fn index_u64(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}
