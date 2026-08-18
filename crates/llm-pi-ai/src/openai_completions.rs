//! OpenAI-compatible Chat Completions native protocol engine.

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::StreamExt as _;
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use seekdeep_llm::CallId;
use seekdeep_llm_deepseek::sse::{ByteStream, DONE, parse_sse};
use serde_json::{Map, Value, json};

use crate::{
    adapter::{BoxPiEventStream, PiExecutionRequest, PiProtocolExecutor},
    catalog::{PiModel, PiThinkingLevel},
    context::{PiContext, PiMessage, PiToolResultMessage, PiUserContent, PiUserContentBlock},
    json::stringify_object,
    provider::{PiProtocol, PiProviderDispatch},
    replay::{
        PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiStopReason, PiUsage,
    },
    stream::{PiAssistantEvent, PiToolCall},
};

/// Reqwest-backed OpenAI-compatible Chat Completions engine.
#[derive(Clone, Debug)]
pub struct OpenAiCompletionsExecutor {
    http: reqwest::Client,
    flavor: CompletionsFlavor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionsFlavor {
    OpenAi,
    Mistral,
}

impl OpenAiCompletionsExecutor {
    /// Creates an executor using one reusable HTTP client.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: CompletionsFlavor::OpenAi,
        }
    }

    /// Creates the Mistral Conversations flavor.
    #[must_use]
    pub fn new_mistral(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: CompletionsFlavor::Mistral,
        }
    }
}

impl PiProtocolExecutor for OpenAiCompletionsExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        let expected_api = match self.flavor {
            CompletionsFlavor::OpenAi => "openai-completions",
            CompletionsFlavor::Mistral => "mistral-conversations",
        };
        if request.model.api.as_str() != expected_api
            || matches!(
                request.provider.dispatch,
                PiProviderDispatch::Protocol(protocol)
                    if self.flavor == CompletionsFlavor::OpenAi
                        && protocol != PiProtocol::OpenAiCompletions
            )
        {
            anyhow::bail!(
                "native OpenAI Completions executor cannot dispatch api \"{}\"",
                request.model.api.as_str()
            );
        }
        let has_auth_header = has_auth_header(&request);
        if request.options.api_key.as_deref().is_none_or(str::is_empty) && !has_auth_header {
            anyhow::bail!(
                "No API key for provider: {}",
                request.model.provider.as_str()
            );
        }

        let output = Arc::new(Mutex::new(empty_assistant(&request.model)));
        let signal = request.options.signal.clone();
        let native = native_events(self.http.clone(), request, output.clone(), self.flavor);
        Ok(Box::pin(async_stream::stream! {
            futures::pin_mut!(native);
            while let Some(event) = native.next().await {
                match event {
                    Ok(event) => yield Ok(event),
                    Err(error) => {
                        let mut failed = output.lock().clone();
                        failed.stop_reason = if signal.is_aborted() {
                            PiStopReason::Aborted
                        } else {
                            PiStopReason::Error
                        };
                        failed.error_message = Some(error.to_string());
                        yield Ok(PiAssistantEvent::Error {
                            reason: failed.stop_reason,
                            error: failed,
                        });
                        return;
                    }
                }
            }
        }))
    }
}

fn has_auth_header(request: &PiExecutionRequest) -> bool {
    let eligible = |name: &str| {
        name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("cf-aig-authorization")
    };
    request
        .model
        .extra
        .get("headers")
        .and_then(Value::as_object)
        .is_some_and(|headers| {
            headers.iter().any(|(name, value)| {
                eligible(name) && value.as_str().is_some_and(|value| !value.trim().is_empty())
            })
        })
        || request
            .options
            .headers
            .iter()
            .any(|(name, value)| eligible(name) && !value.trim().is_empty())
}

#[allow(clippy::too_many_lines)] // One ordered wire stream state machine.
fn native_events(
    http: reqwest::Client,
    request: PiExecutionRequest,
    shared: Arc<Mutex<PiAssistantMessage>>,
    flavor: CompletionsFlavor,
) -> BoxPiEventStream {
    Box::pin(async_stream::try_stream! {
        let mut compat = Compat::of(&request.model);
        if flavor == CompletionsFlavor::Mistral {
            compat.supports_store = false;
            compat.supports_developer_role = false;
            "max_tokens".clone_into(&mut compat.max_tokens_field);
        }
        let body = build_request(&request.model, &request.context, &request.options, &compat, flavor)?;
        let endpoint = if flavor == CompletionsFlavor::Mistral { "v1/chat/completions" } else { "chat/completions" };
        let url = format!("{}/{endpoint}", request.model.base_url.trim_end_matches('/'));
        let headers = request_headers(&request, flavor)?;
        let mut builder = http
            .post(url)
            .headers(headers)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body)?);
        if let Some(timeout_ms) = request.options.timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        let response_result: anyhow::Result<reqwest::Response> = tokio::select! {
            biased;
            () = request.options.signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
            response = builder.send() => response.map_err(anyhow::Error::from),
        };
        let response = response_result?;
        let response = if response.status().is_success() {
            response
        } else {
            let status = response.status().as_u16();
            let text_result: anyhow::Result<String> = tokio::select! {
                biased;
                () = request.options.signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
                body = response.text() => body.map_err(anyhow::Error::from),
            };
            let text = text_result?;
            let detail = serde_json::from_str::<Value>(&text).ok()
                .and_then(|body| body.get("error")?.get("message")?.as_str().map(str::to_owned))
                .filter(|message| !message.is_empty())
                .unwrap_or(text);
            Err::<reqwest::Response, _>(anyhow::anyhow!("OpenAI API error ({status}): {detail}"))?
        };
        let bytes: ByteStream = Box::pin(response.bytes_stream().map(|result| result.map_err(anyhow::Error::from)));
        let mut payloads = parse_sse(bytes, None);
        let mut output = shared.lock().clone();
        yield PiAssistantEvent::Start { partial: output.clone() };
        let mut text_index = None::<usize>;
        let mut thinking_index = None::<usize>;
        let mut tool_indices = HashMap::<i64, usize>::new();
        let mut tool_arguments = HashMap::<usize, String>::new();
        let mut has_finish_reason = false;

        loop {
            let payload = match payloads.next().await {
                Some(Ok(payload)) => payload,
                Some(Err(error)) if flavor == CompletionsFlavor::Mistral && has_finish_reason
                    && error.to_string().contains("without [DONE]") => break,
                Some(Err(error)) => Err::<String, _>(error)?,
                None => break,
            };
            if payload == DONE {
                break;
            }
            let chunk: Value = serde_json::from_str(&payload)?;
            if output.response_id.is_none() {
                output.response_id = chunk.get("id").and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(crate::replay::PiResponseId::new);
            }
            if output.response_model.is_none()
                && let Some(model) = chunk.get("model").and_then(Value::as_str)
                && !model.is_empty()
                && model != request.model.id.as_str()
            {
                output.response_model = Some(seekdeep_llm::ModelId::new(model));
            }
            if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
                output.usage = parse_usage(usage);
            }
            let Some(choice) = chunk.get("choices").and_then(Value::as_array).and_then(|choices| choices.first()) else {
                publish(&shared, &output);
                continue;
            };
            if chunk.get("usage").is_none()
                && let Some(usage) = choice.get("usage")
            {
                output.usage = parse_usage(usage);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                let (stop_reason, error) = map_finish_reason(reason);
                output.stop_reason = stop_reason;
                output.error_message = error;
                has_finish_reason = true;
            }
            if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
                if let Some(content) = delta.get("content").and_then(Value::as_str).filter(|text| !text.is_empty()) {
                    let (index, opened) = ensure_text(&mut output, &mut text_index);
                    if opened {
                        yield PiAssistantEvent::TextStart { content_index: index_u64(index), partial: output.clone() };
                    }
                    if let PiAssistantBlock::Text { text, .. } = &mut output.content[index] {
                        text.push_str(content);
                    }
                    publish(&shared, &output);
                    yield PiAssistantEvent::TextDelta {
                        content_index: index_u64(index),
                        delta: content.to_owned(),
                        partial: output.clone(),
                    };
                }
                if let Some((field, reasoning)) = ["reasoning_content", "reasoning", "reasoning_text"]
                    .into_iter()
                    .find_map(|field| delta.get(field).and_then(Value::as_str)
                        .filter(|value| !value.is_empty()).map(|value| (field, value)))
                {
                    let (index, opened) = ensure_thinking(&mut output, &mut thinking_index, field);
                    if opened {
                        yield PiAssistantEvent::ThinkingStart { content_index: index_u64(index), partial: output.clone() };
                    }
                    if let PiAssistantBlock::Thinking { thinking, .. } = &mut output.content[index] {
                        thinking.push_str(reasoning);
                    }
                    publish(&shared, &output);
                    yield PiAssistantEvent::ThinkingDelta {
                        content_index: index_u64(index),
                        delta: reasoning.to_owned(),
                        partial: output.clone(),
                    };
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let stream_index = call.get("index").and_then(Value::as_i64)
                            .unwrap_or_else(|| i64::try_from(tool_indices.len()).unwrap_or(i64::MAX));
                        let (index, opened) = ensure_tool(&mut output, &mut tool_indices, stream_index, call);
                        if opened {
                            yield PiAssistantEvent::ToolCallStart { content_index: index_u64(index), partial: output.clone() };
                        }
                        update_tool_identity(&mut output.content[index], call);
                        let fragment = call.get("function").and_then(Value::as_object)
                            .and_then(|function| function.get("arguments"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let raw = tool_arguments.entry(index).or_default();
                        raw.push_str(fragment);
                        if let PiAssistantBlock::ToolCall { arguments, .. } = &mut output.content[index] {
                            *arguments = parse_arguments(raw);
                        }
                        publish(&shared, &output);
                        yield PiAssistantEvent::ToolCallDelta {
                            content_index: index_u64(index),
                            delta: fragment.to_owned(),
                            partial: output.clone(),
                        };
                    }
                }
            }
            publish(&shared, &output);
        }

        for index in 0..output.content.len() {
            match output.content[index].clone() {
                PiAssistantBlock::Text { text, .. } => {
                    yield PiAssistantEvent::TextEnd {
                        content_index: index_u64(index), content: text, partial: output.clone(),
                    };
                }
                PiAssistantBlock::Thinking { thinking, .. } => {
                    yield PiAssistantEvent::ThinkingEnd {
                        content_index: index_u64(index), content: thinking, partial: output.clone(),
                    };
                }
                PiAssistantBlock::ToolCall { id, name, thought_signature, .. } => {
                    let arguments = parse_arguments(tool_arguments.get(&index).map_or("", String::as_str));
                    output.content[index] = PiAssistantBlock::ToolCall {
                        id: id.clone(), name: name.clone(), arguments: arguments.clone(),
                        thought_signature: thought_signature.clone(),
                    };
                    publish(&shared, &output);
                    yield PiAssistantEvent::ToolCallEnd {
                        content_index: index_u64(index),
                        tool_call: PiToolCall { id, name, arguments, thought_signature },
                        partial: output.clone(),
                    };
                }
            }
        }
        if request.options.signal.is_aborted() {
            Err::<(), _>(anyhow::anyhow!("Request was aborted"))?;
        }
        if output.stop_reason == PiStopReason::Error {
            Err::<(), _>(anyhow::anyhow!(
                "{}",
                output.error_message.as_deref().unwrap_or("Provider returned an error stop reason")
            ))?;
        }
        if !has_finish_reason {
            Err::<(), _>(anyhow::anyhow!("Stream ended without finish_reason"))?;
        }
        publish(&shared, &output);
        yield PiAssistantEvent::Done { reason: output.stop_reason, message: output };
    })
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent upstream compatibility switches.
struct Compat {
    supports_store: bool,
    supports_developer_role: bool,
    supports_reasoning_effort: bool,
    supports_usage_in_streaming: bool,
    max_tokens_field: String,
    requires_reasoning_content: bool,
    supports_strict_mode: bool,
    thinking_format: String,
}

impl Compat {
    #[allow(clippy::too_many_lines)] // Source-ordered detection followed by explicit overrides.
    fn of(model: &PiModel) -> Self {
        let provider = model.provider.as_str();
        let base = model.base_url.as_str();
        let is_zai = provider == "zai"
            || provider == "zai-coding-cn"
            || base.contains("api.z.ai")
            || base.contains("open.bigmodel.cn");
        let is_together = provider == "together"
            || base.contains("api.together.ai")
            || base.contains("api.together.xyz");
        let is_moonshot = provider == "moonshotai"
            || provider == "moonshotai-cn"
            || base.contains("api.moonshot.");
        let is_openrouter = provider == "openrouter" || base.contains("openrouter.ai");
        let is_cloudflare_gateway =
            provider == "cloudflare-ai-gateway" || base.contains("gateway.ai.cloudflare.com");
        let is_nvidia = provider == "nvidia" || base.contains("integrate.api.nvidia.com");
        let is_ant_ling = provider == "ant-ling" || base.contains("api.ant-ling.com");
        let is_deepseek = provider == "deepseek" || base.contains("deepseek.com");
        let nonstandard = is_nvidia
            || provider == "cerebras"
            || base.contains("cerebras.ai")
            || provider == "xai"
            || base.contains("api.x.ai")
            || is_together
            || base.contains("chutes.ai")
            || is_deepseek
            || is_zai
            || is_moonshot
            || provider == "opencode"
            || base.contains("opencode.ai")
            || provider == "cloudflare-workers-ai"
            || is_cloudflare_gateway
            || is_ant_ling;
        let mut compat = Self {
            supports_store: !nonstandard,
            supports_developer_role: (is_openrouter
                && (model.id.as_str().starts_with("anthropic/")
                    || model.id.as_str().starts_with("openai/")))
                || (!nonstandard && !is_openrouter),
            supports_reasoning_effort: provider != "xai"
                && !is_zai
                && !is_moonshot
                && !is_together
                && !is_cloudflare_gateway
                && !is_nvidia
                && !is_ant_ling,
            supports_usage_in_streaming: true,
            max_tokens_field: if base.contains("chutes.ai")
                || is_moonshot
                || is_cloudflare_gateway
                || is_together
                || is_nvidia
                || is_ant_ling
            {
                "max_tokens"
            } else {
                "max_completion_tokens"
            }
            .to_owned(),
            requires_reasoning_content: is_deepseek,
            supports_strict_mode: !is_moonshot
                && !is_together
                && !is_cloudflare_gateway
                && !is_nvidia,
            thinking_format: if is_deepseek {
                "deepseek"
            } else if is_zai {
                "zai"
            } else if is_together {
                "together"
            } else if is_ant_ling {
                "ant-ling"
            } else if is_openrouter {
                "openrouter"
            } else {
                "openai"
            }
            .to_owned(),
        };
        if let Some(explicit) = &model.compat {
            assign_bool(explicit, "supportsStore", &mut compat.supports_store);
            assign_bool(
                explicit,
                "supportsDeveloperRole",
                &mut compat.supports_developer_role,
            );
            assign_bool(
                explicit,
                "supportsReasoningEffort",
                &mut compat.supports_reasoning_effort,
            );
            assign_bool(
                explicit,
                "supportsUsageInStreaming",
                &mut compat.supports_usage_in_streaming,
            );
            assign_bool(
                explicit,
                "requiresReasoningContentOnAssistantMessages",
                &mut compat.requires_reasoning_content,
            );
            assign_bool(
                explicit,
                "supportsStrictMode",
                &mut compat.supports_strict_mode,
            );
            if let Some(value) = explicit.get("maxTokensField").and_then(Value::as_str) {
                value.clone_into(&mut compat.max_tokens_field);
            }
            if let Some(value) = explicit.get("thinkingFormat").and_then(Value::as_str) {
                value.clone_into(&mut compat.thinking_format);
            }
        }
        compat
    }
}

fn assign_bool(map: &Map<String, Value>, key: &str, output: &mut bool) {
    if let Some(value) = map.get(key).and_then(Value::as_bool) {
        *output = value;
    }
}

fn build_request(
    model: &PiModel,
    context: &PiContext,
    options: &crate::adapter::PiStreamOptions,
    compat: &Compat,
    flavor: CompletionsFlavor,
) -> anyhow::Result<Value> {
    let mut root = Map::new();
    root.insert(
        "model".to_owned(),
        Value::String(model.id.as_str().to_owned()),
    );
    root.insert(
        "messages".to_owned(),
        Value::Array(convert_messages(model, context, compat)?),
    );
    root.insert("stream".to_owned(), Value::Bool(true));
    if compat.supports_usage_in_streaming {
        root.insert("stream_options".to_owned(), json!({"include_usage":true}));
    }
    if compat.supports_store {
        root.insert("store".to_owned(), Value::Bool(false));
    }
    if let Some(max_tokens) = options.max_tokens {
        root.insert(compat.max_tokens_field.clone(), Value::from(max_tokens));
    }
    if let Some(temperature) = options.temperature {
        root.insert("temperature".to_owned(), json!(temperature));
    }
    if flavor == CompletionsFlavor::Mistral
        && options.cache_retention != Some(crate::config::PiCacheRetention::None)
        && let Some(session) = &options.session_id
    {
        root.insert(
            "prompt_cache_key".to_owned(),
            Value::String(session.as_str().to_owned()),
        );
    }
    if flavor == CompletionsFlavor::Mistral
        && let Some(reasoning) = options.reasoning
    {
        root.insert(
            "reasoning_effort".to_owned(),
            Value::String(mapped_effort(model, reasoning)),
        );
    }
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        root.insert(
            "tools".to_owned(),
            Value::Array(tools.iter().map(|tool| tool_value(tool, compat)).collect()),
        );
    }
    apply_reasoning(model, options.reasoning, compat, &mut root);
    Ok(Value::Object(root))
}

fn tool_value(tool: &crate::context::PiTool, compat: &Compat) -> Value {
    let mut function = Map::from_iter([
        ("name".to_owned(), Value::String(tool.name.clone())),
        (
            "description".to_owned(),
            Value::String(tool.description.clone()),
        ),
        (
            "parameters".to_owned(),
            Value::Object(tool.parameters.clone()),
        ),
    ]);
    if compat.supports_strict_mode {
        function.insert("strict".to_owned(), Value::Bool(false));
    }
    json!({"type":"function","function":function})
}

fn convert_messages(
    model: &PiModel,
    context: &PiContext,
    compat: &Compat,
) -> anyhow::Result<Vec<Value>> {
    let mut messages = Vec::new();
    if let Some(system) = context
        .system_prompt
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        messages.push(json!({
            "role": if model.reasoning && compat.supports_developer_role { "developer" } else { "system" },
            "content": system,
        }));
    }
    for message in &context.messages {
        match message {
            PiMessage::User(message) => match &message.content {
                PiUserContent::Text(text) => messages.push(json!({"role":"user","content":text})),
                PiUserContent::Blocks(blocks) => {
                    let wire_content = blocks.iter().map(user_block).collect::<Vec<_>>();
                    if !wire_content.is_empty() {
                        messages.push(json!({"role":"user","content":wire_content}));
                    }
                }
            },
            PiMessage::Assistant(message) => {
                if let Some(message) = assistant_message(model, message, compat)? {
                    messages.push(message);
                }
            }
            PiMessage::ToolResult(message) => append_tool_result(model, message, &mut messages),
        }
    }
    Ok(messages)
}

fn user_block(block: &PiUserContentBlock) -> Value {
    match block {
        PiUserContentBlock::Text { text } => json!({"type":"text","text":text}),
        PiUserContentBlock::Image { data, mime_type } => {
            json!({"type":"image_url","image_url":{"url":format!("data:{mime_type};base64,{data}")}})
        }
    }
}

fn assistant_message(
    model: &PiModel,
    message: &PiAssistantMessage,
    compat: &Compat,
) -> anyhow::Result<Option<Value>> {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            PiAssistantBlock::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let thinking = message
        .content
        .iter()
        .filter_map(|block| match block {
            PiAssistantBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                Some(thinking.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let calls = message
        .content
        .iter()
        .filter_map(|block| match block {
            PiAssistantBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some((id, name, arguments)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if text.is_empty() && thinking.is_empty() && calls.is_empty() {
        return Ok(None);
    }
    let mut output = Map::from_iter([
        ("role".to_owned(), Value::String("assistant".to_owned())),
        (
            "content".to_owned(),
            if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            },
        ),
    ]);
    if !thinking.is_empty() {
        let signature = message
            .content
            .iter()
            .find_map(|block| match block {
                PiAssistantBlock::Thinking {
                    thinking_signature, ..
                } => thinking_signature.as_deref(),
                _ => None,
            })
            .unwrap_or("reasoning_content");
        output.insert(signature.to_owned(), Value::String(thinking.join("\n")));
    }
    if !calls.is_empty() {
        output.insert(
            "tool_calls".to_owned(),
            Value::Array(
                calls
                    .into_iter()
                    .map(|(id, name, arguments)| {
                        Ok(json!({
                            "id":id,
                            "type":"function",
                            "function":{"name":name,"arguments":stringify_object(arguments)?}
                        }))
                    })
                    .collect::<serde_json::Result<Vec<_>>>()?,
            ),
        );
    }
    if compat.requires_reasoning_content
        && model.reasoning
        && !output.contains_key("reasoning_content")
    {
        output.insert("reasoning_content".to_owned(), Value::String(String::new()));
    }
    Ok(Some(Value::Object(output)))
}

fn append_tool_result(model: &PiModel, message: &PiToolResultMessage, output: &mut Vec<Value>) {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            PiUserContentBlock::Text { text } => Some(text.as_str()),
            PiUserContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images = message
        .content
        .iter()
        .filter(|block| matches!(block, PiUserContentBlock::Image { .. }))
        .collect::<Vec<_>>();
    output.push(json!({
        "role":"tool",
        "content":if text.is_empty() { if images.is_empty() { "(no tool output)" } else { "(see attached image)" } } else { &text },
        "tool_call_id":message.tool_call_id,
    }));
    if !images.is_empty() && model.input.contains(&crate::catalog::PiModality::Image) {
        let mut content = vec![json!({"type":"text","text":"Attached image(s) from tool result:"})];
        content.extend(images.into_iter().map(user_block));
        output.push(json!({"role":"user","content":content}));
    }
}

fn apply_reasoning(
    model: &PiModel,
    reasoning: Option<PiThinkingLevel>,
    compat: &Compat,
    root: &mut Map<String, Value>,
) {
    let mapped = reasoning.and_then(|level| {
        model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(level.as_str()))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| Some(level.as_str().to_owned()))
    });
    if compat.thinking_format == "deepseek" && model.reasoning {
        root.insert(
            "thinking".to_owned(),
            json!({"type":if reasoning.is_some(){"enabled"}else{"disabled"}}),
        );
        if compat.supports_reasoning_effort
            && let Some(effort) = mapped
        {
            root.insert("reasoning_effort".to_owned(), Value::String(effort));
        }
    } else if model.reasoning
        && compat.supports_reasoning_effort
        && let Some(effort) = mapped.or_else(|| {
            model
                .thinking_level_map
                .as_ref()?
                .get("off")?
                .as_str()
                .map(str::to_owned)
        })
    {
        root.insert("reasoning_effort".to_owned(), Value::String(effort));
    }
}

fn mapped_effort(model: &PiModel, level: PiThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(level.as_str()))
        .and_then(Value::as_str)
        .unwrap_or(level.as_str())
        .to_owned()
}

fn request_headers(
    request: &PiExecutionRequest,
    flavor: CompletionsFlavor,
) -> anyhow::Result<HeaderMap> {
    let mut values = Map::<String, Value>::new();
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
    if flavor == CompletionsFlavor::Mistral
        && request.options.cache_retention != Some(crate::config::PiCacheRetention::None)
        && let Some(session) = &request.options.session_id
        && !values.contains_key("x-affinity")
    {
        values.insert(
            "x-affinity".to_owned(),
            Value::String(session.as_str().to_owned()),
        );
    }
    let mut headers = HeaderMap::new();
    if let Some(key) = &request.options.api_key {
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}"))?,
        );
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

fn parse_usage(value: &Value) -> PiUsage {
    let prompt = number(value.get("prompt_tokens"));
    let cache_read = value
        .get("prompt_tokens_details")
        .and_then(|value| value.get("cached_tokens"))
        .map_or_else(
            || number(value.get("prompt_cache_hit_tokens")),
            |value| number(Some(value)),
        );
    let cache_write = value
        .get("prompt_tokens_details")
        .and_then(|value| value.get("cache_write_tokens"))
        .map_or(0, |value| number(Some(value)));
    let output = number(value.get("completion_tokens"));
    PiUsage {
        input: prompt.saturating_sub(cache_read.saturating_add(cache_write)),
        output,
        cache_read,
        cache_write,
        total_tokens: prompt
            .saturating_sub(cache_read.saturating_add(cache_write))
            .saturating_add(output)
            .saturating_add(cache_read)
            .saturating_add(cache_write),
        cost: PiCost::default(),
    }
}

fn number(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or_default()
}

fn map_finish_reason(reason: &str) -> (PiStopReason, Option<String>) {
    match reason {
        "stop" | "end" => (PiStopReason::Stop, None),
        "length" => (PiStopReason::Length, None),
        "function_call" | "tool_calls" => (PiStopReason::ToolUse, None),
        other => (
            PiStopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

fn ensure_text(output: &mut PiAssistantMessage, slot: &mut Option<usize>) -> (usize, bool) {
    if let Some(index) = *slot {
        return (index, false);
    }
    let index = output.content.len();
    output.content.push(PiAssistantBlock::Text {
        text: String::new(),
        text_signature: None,
    });
    *slot = Some(index);
    (index, true)
}

fn ensure_thinking(
    output: &mut PiAssistantMessage,
    slot: &mut Option<usize>,
    signature: &str,
) -> (usize, bool) {
    if let Some(index) = *slot {
        return (index, false);
    }
    let index = output.content.len();
    output.content.push(PiAssistantBlock::Thinking {
        thinking: String::new(),
        thinking_signature: Some(signature.to_owned()),
        redacted: None,
    });
    *slot = Some(index);
    (index, true)
}

fn ensure_tool(
    output: &mut PiAssistantMessage,
    slots: &mut HashMap<i64, usize>,
    stream_index: i64,
    call: &Value,
) -> (usize, bool) {
    if let Some(index) = slots.get(&stream_index) {
        return (*index, false);
    }
    let index = output.content.len();
    let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
    let name = call
        .get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    output.content.push(PiAssistantBlock::ToolCall {
        id: CallId::new(id),
        name: name.to_owned(),
        arguments: Map::new(),
        thought_signature: None,
    });
    slots.insert(stream_index, index);
    (index, true)
}

fn update_tool_identity(block: &mut PiAssistantBlock, call: &Value) {
    let PiAssistantBlock::ToolCall { id, name, .. } = block else {
        return;
    };
    if id.as_str().is_empty()
        && let Some(value) = call.get("id").and_then(Value::as_str)
    {
        *id = CallId::new(value);
    }
    if name.is_empty()
        && let Some(value) = call
            .get("function")
            .and_then(Value::as_object)
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
    {
        value.clone_into(name);
    }
}

fn parse_arguments(raw: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn publish(shared: &Mutex<PiAssistantMessage>, output: &PiAssistantMessage) {
    *shared.lock() = output.clone();
}

fn index_u64(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}
