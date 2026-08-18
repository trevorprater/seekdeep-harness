//! Amazon Bedrock Converse Stream native protocol engine.

use std::{collections::HashMap, pin::Pin, sync::Arc};

use aws_sdk_bedrockruntime::{
    Client,
    config::{Credentials, Region, Token},
    types::{
        CachePointBlock, CachePointType, CacheTtl, ContentBlock, ContentBlockDelta,
        ContentBlockStart, ConversationRole, ConverseStreamOutput, InferenceConfiguration, Message,
        ReasoningContentBlock, ReasoningContentBlockDelta, ReasoningTextBlock, SystemContentBlock,
        Tool, ToolConfiguration, ToolInputSchema, ToolResultBlock, ToolResultContentBlock,
        ToolResultStatus, ToolSpecification, ToolUseBlock,
    },
};
use aws_smithy_types::{Document, Number};
use base64::Engine as _;
use futures::{Stream, StreamExt as _};
use http::header::{HeaderName, HeaderValue};
use parking_lot::Mutex;
use seekdeep_llm::CallId;
use serde_json::{Map, Value, json};

use crate::{
    adapter::{BoxPiEventStream, PiExecutionRequest, PiProtocolExecutor},
    catalog::{PiModel, PiThinkingLevel},
    config::PiCacheRetention,
    context::{PiContext, PiMessage, PiToolResultMessage, PiUserContent, PiUserContentBlock},
    replay::{
        PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiStopReason, PiUsage,
    },
    stream::{PiAssistantEvent, PiToolCall},
};

type BoxBedrockEventStream =
    Pin<Box<dyn Stream<Item = anyhow::Result<BedrockEvent>> + Send + 'static>>;

/// Provider-neutral projection of the AWS event-stream union, exposed for deterministic tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BedrockEvent {
    /// Assistant message opened.
    MessageStart,
    /// Tool block opened.
    ToolStart {
        /// Provider content-block index.
        wire_index: i32,
        /// Provider tool-use identity.
        id: String,
        /// Tool name.
        name: String,
    },
    /// Text suffix.
    Text {
        /// Provider content-block index.
        wire_index: i32,
        /// Exact suffix.
        text: String,
    },
    /// Tool JSON suffix.
    ToolInput {
        /// Provider content-block index.
        wire_index: i32,
        /// Exact JSON suffix.
        input: String,
    },
    /// Reasoning text suffix.
    ReasoningText {
        /// Provider content-block index.
        wire_index: i32,
        /// Exact suffix.
        text: String,
    },
    /// Reasoning signature suffix.
    ReasoningSignature {
        /// Provider content-block index.
        wire_index: i32,
        /// Exact signature suffix.
        signature: String,
    },
    /// Content block completed.
    BlockStop {
        /// Provider content-block index.
        wire_index: i32,
    },
    /// Model terminal reason.
    MessageStop {
        /// Extensible Bedrock stop-reason spelling.
        reason: String,
    },
    /// Token accounting.
    Metadata {
        /// Uncached input tokens.
        input: u64,
        /// Generated tokens.
        output: u64,
        /// Cache-hit input tokens.
        cache_read: u64,
        /// Cache-populated input tokens.
        cache_write: u64,
        /// Provider total.
        total: u64,
    },
}

/// Official-AWS-SDK-backed Bedrock Converse Stream executor.
#[derive(Clone, Debug, Default)]
pub struct BedrockExecutor;

impl PiProtocolExecutor for BedrockExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        if request.model.api.as_str() != "bedrock-converse-stream" {
            anyhow::bail!(
                "native Bedrock executor cannot dispatch api \"{}\"",
                request.model.api.as_str()
            );
        }
        let output = Arc::new(Mutex::new(empty_assistant(&request.model)));
        let signal = request.options.signal.clone();
        let output_for_events = output.clone();
        let events = Box::pin(async_stream::try_stream! {
            let stream = open_aws_stream(&request).await?;
            futures::pin_mut!(stream);
            while let Some(event) = stream.next().await { yield event?; }
        });
        let native = translate_events(events, output_for_events);
        Ok(Box::pin(async_stream::stream! {
            futures::pin_mut!(native);
            while let Some(event) = native.next().await {
                match event {
                    Ok(event) => yield Ok(event),
                    Err(error) => {
                        let mut failed = output.lock().clone();
                        failed.stop_reason = if signal.is_aborted() { PiStopReason::Aborted } else { PiStopReason::Error };
                        failed.error_message = Some(format_bedrock_error(&error));
                        yield Ok(PiAssistantEvent::Error { reason: failed.stop_reason, error: failed });
                        return;
                    }
                }
            }
        }))
    }
}

/// Translates a deterministic Bedrock event stream without opening AWS transport.
#[must_use]
pub fn translate_bedrock_events<S>(events: S, model: &PiModel) -> BoxPiEventStream
where
    S: Stream<Item = anyhow::Result<BedrockEvent>> + Send + 'static,
{
    let output = Arc::new(Mutex::new(empty_assistant(model)));
    let translated = translate_events(Box::pin(events), output.clone());
    Box::pin(async_stream::stream! {
        futures::pin_mut!(translated);
        while let Some(event) = translated.next().await {
            match event {
                Ok(event) => yield Ok(event),
                Err(error) => {
                    let mut failed = output.lock().clone();
                    failed.stop_reason = PiStopReason::Error;
                    failed.error_message = Some(format_bedrock_error(&error));
                    yield Ok(PiAssistantEvent::Error {
                        reason: PiStopReason::Error,
                        error: failed,
                    });
                    return;
                }
            }
        }
    })
}

async fn open_aws_stream(request: &PiExecutionRequest) -> anyhow::Result<BoxBedrockEventStream> {
    let signal = request.options.signal.clone();
    let auth_environment = &request.options.auth_environment;
    let (region, explicit_endpoint) = bedrock_target(request);
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
    if let Some(region) = region.clone() {
        loader = loader.region(region);
    }
    if let Some(profile) = auth_environment.get("AWS_PROFILE") {
        loader = loader.profile_name(profile);
    }
    let shared = loader.load().await;
    let mut config = aws_sdk_bedrockruntime::config::Builder::from(&shared);
    if shared.region().is_none() {
        config = config.region(Region::new("us-east-1"));
    }
    if let Some(endpoint) = explicit_endpoint {
        config = config.endpoint_url(endpoint);
    }
    if let Some(http_client) = bedrock_http_client(auth_environment, &request.model.base_url)? {
        config = config.http_client(http_client);
    }
    if let Some(credentials) = bedrock_credentials(auth_environment) {
        config = config.credentials_provider(credentials);
    } else if let Some(token) = request
        .options
        .api_key
        .clone()
        .filter(|token| !(token.is_empty() || token.starts_with('<') && token.ends_with('>')))
    {
        config = config.bearer_token(Token::new(token, None));
    }
    let client = Client::from_conf(config.build());
    let cache_retention = resolve_cache_retention(request);
    let messages = bedrock_messages(
        &request.context,
        &request.model,
        cache_retention,
        auth_environment,
    )?;
    let system = bedrock_system(
        request.context.system_prompt.as_deref(),
        &request.model,
        cache_retention,
        auth_environment,
    )?;
    let max_tokens = i32::try_from(bedrock_max_tokens(request))
        .map_err(|_| anyhow::anyhow!("Bedrock maxTokens exceeds i32"))?;
    let temperature = request
        .options
        .temperature
        .map(|value| value.to_string().parse::<f32>())
        .transpose()
        .map_err(anyhow::Error::from)?;
    let inference = InferenceConfiguration::builder()
        .max_tokens(max_tokens)
        .set_temperature(temperature)
        .build();
    let tools = bedrock_tools(&request.context)?;
    let additional = bedrock_additional_fields(request).map(|value| document(&value));
    let custom_headers = bedrock_headers(request)?;
    let operation = client
        .converse_stream()
        .model_id(request.model.id.as_str())
        .set_messages(Some(messages))
        .set_system(system)
        .inference_config(inference)
        .set_tool_config(tools)
        .set_additional_model_request_fields(additional)
        .customize()
        .mutate_request(move |request| {
            for (name, value) in &custom_headers {
                request.headers_mut().insert(name.clone(), value.clone());
            }
        });
    let response = tokio::select! {
        biased;
        () = signal.cancelled() => Err(anyhow::anyhow!("Request was aborted"))?,
        response = operation.send() => response?,
    };
    let mut stream = response.stream;
    Ok(Box::pin(async_stream::try_stream! {
        loop {
            let next: anyhow::Result<_> = tokio::select! {
                biased;
                () = signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
                next = stream.recv() => next.map_err(anyhow::Error::from),
            };
            let Some(event) = next? else { return };
            if let Some(event) = project_sdk_event(event)? { yield event; }
        }
    }))
}

fn bedrock_target(request: &PiExecutionRequest) -> (Option<Region>, Option<String>) {
    let environment = &request.options.auth_environment;
    let configured_region = environment
        .get("AWS_REGION")
        .or_else(|| environment.get("AWS_DEFAULT_REGION"))
        .filter(|value| !value.is_empty())
        .cloned();
    let endpoint = explicit_endpoint(
        &request.model.base_url,
        configured_region.as_deref(),
        environment.contains_key("AWS_PROFILE"),
    );
    let region = bedrock_region(&request.model)
        .or_else(|| configured_region.map(Region::new))
        .or_else(|| {
            endpoint
                .as_deref()
                .and_then(standard_bedrock_endpoint_region)
                .map(Region::new)
        });
    (region, endpoint)
}

fn bedrock_credentials(environment: &HashMap<String, String>) -> Option<Credentials> {
    let skip_auth = environment
        .get("AWS_BEDROCK_SKIP_AUTH")
        .is_some_and(|value| value == "1")
        || std::env::var("AWS_BEDROCK_SKIP_AUTH").as_deref() == Ok("1");
    if skip_auth {
        return Some(Credentials::new(
            "dummy-access-key",
            "dummy-secret-key",
            None,
            None,
            "seekdeep-bedrock-skip-auth",
        ));
    }
    let (Some(access), Some(secret)) = (
        environment.get("AWS_ACCESS_KEY_ID"),
        environment.get("AWS_SECRET_ACCESS_KEY"),
    ) else {
        return None;
    };
    Some(Credentials::new(
        access,
        secret,
        environment.get("AWS_SESSION_TOKEN").cloned(),
        None,
        "seekdeep-bedrock-explicit",
    ))
}

fn bedrock_http_client(
    environment: &HashMap<String, String>,
    target: &str,
) -> anyhow::Result<Option<aws_sdk_bedrockruntime::config::SharedHttpClient>> {
    use aws_smithy_http_client::{Builder, Connector, proxy::ProxyConfig, tls};

    let https = target.trim().to_ascii_lowercase().starts_with("https://");
    let proxy = if https {
        environment
            .get("HTTPS_PROXY")
            .or_else(|| environment.get("HTTP_PROXY"))
    } else {
        environment.get("HTTP_PROXY")
    };
    if let Some(proxy) = proxy.filter(|proxy| !proxy.is_empty()) {
        let mut proxy = if https {
            ProxyConfig::https(proxy.as_str())?
        } else {
            ProxyConfig::http(proxy.as_str())?
        };
        if let Some(no_proxy) = environment.get("NO_PROXY") {
            proxy = proxy.no_proxy(no_proxy);
        }
        return Ok(Some(Builder::new().build_with_connector_fn(move |_, _| {
            Connector::builder()
                .proxy_config(proxy.clone())
                .tls_provider(tls::Provider::Rustls(
                    tls::rustls_provider::CryptoMode::AwsLc,
                ))
                .build()
        })));
    }
    if environment
        .get("AWS_BEDROCK_FORCE_HTTP1")
        .is_some_and(|value| value == "1")
    {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        return Ok(Some(
            aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder::new().build(connector),
        ));
    }
    Ok(None)
}

fn bedrock_headers(request: &PiExecutionRequest) -> anyhow::Result<Vec<(HeaderName, HeaderValue)>> {
    let mut values = Map::new();
    if let Some(headers) = request
        .model
        .extra
        .get("headers")
        .and_then(Value::as_object)
    {
        values.extend(headers.clone());
    }
    for (name, value) in &request.options.headers {
        if let Some(existing) = values
            .keys()
            .find(|existing| existing.eq_ignore_ascii_case(name))
            .cloned()
        {
            values.remove(&existing);
        }
        values.insert(name.clone(), Value::String(value.clone()));
    }
    values
        .into_iter()
        .filter(|(name, _)| !is_reserved_bedrock_header(name))
        .filter_map(|(name, value)| value.as_str().map(|value| (name, value.to_owned())))
        .map(|(name, value)| {
            Ok((
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(&value)?,
            ))
        })
        .collect()
}

fn is_reserved_bedrock_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "authorization" || lower == "host" || lower.starts_with("x-amz-")
}

fn project_sdk_event(event: ConverseStreamOutput) -> anyhow::Result<Option<BedrockEvent>> {
    Ok(Some(match event {
        ConverseStreamOutput::MessageStart(event) => {
            anyhow::ensure!(
                event.role == ConversationRole::Assistant,
                "Unexpected assistant message start but got user message start instead"
            );
            BedrockEvent::MessageStart
        }
        ConverseStreamOutput::ContentBlockStart(event) => {
            let Some(ContentBlockStart::ToolUse(tool)) = event.start else {
                return Ok(None);
            };
            BedrockEvent::ToolStart {
                wire_index: event.content_block_index,
                id: tool.tool_use_id,
                name: tool.name,
            }
        }
        ConverseStreamOutput::ContentBlockDelta(event) => {
            let Some(delta) = event.delta else {
                return Ok(None);
            };
            match delta {
                ContentBlockDelta::Text(text) => BedrockEvent::Text {
                    wire_index: event.content_block_index,
                    text,
                },
                ContentBlockDelta::ToolUse(tool) => BedrockEvent::ToolInput {
                    wire_index: event.content_block_index,
                    input: tool.input,
                },
                ContentBlockDelta::ReasoningContent(ReasoningContentBlockDelta::Text(text)) => {
                    BedrockEvent::ReasoningText {
                        wire_index: event.content_block_index,
                        text,
                    }
                }
                ContentBlockDelta::ReasoningContent(ReasoningContentBlockDelta::Signature(
                    signature,
                )) => BedrockEvent::ReasoningSignature {
                    wire_index: event.content_block_index,
                    signature,
                },
                _ => return Ok(None),
            }
        }
        ConverseStreamOutput::ContentBlockStop(event) => BedrockEvent::BlockStop {
            wire_index: event.content_block_index,
        },
        ConverseStreamOutput::MessageStop(event) => BedrockEvent::MessageStop {
            reason: event.stop_reason.as_str().to_owned(),
        },
        ConverseStreamOutput::Metadata(event) => {
            let Some(usage) = event.usage else {
                return Ok(None);
            };
            BedrockEvent::Metadata {
                input: nonnegative(usage.input_tokens),
                output: nonnegative(usage.output_tokens),
                cache_read: usage.cache_read_input_tokens.map_or(0, nonnegative),
                cache_write: usage.cache_write_input_tokens.map_or(0, nonnegative),
                total: nonnegative(usage.total_tokens),
            }
        }
        _ => return Ok(None),
    }))
}

fn translate_events(
    events: BoxBedrockEventStream,
    shared: Arc<Mutex<PiAssistantMessage>>,
) -> BoxPiEventStream {
    Box::pin(async_stream::try_stream! {
        futures::pin_mut!(events);
        let mut output = shared.lock().clone();
        let mut slots = HashMap::<i32, usize>::new();
        let mut partial = HashMap::<i32, String>::new();
        while let Some(event) = events.next().await {
            match event? {
                BedrockEvent::MessageStart => yield PiAssistantEvent::Start { partial: output.clone() },
                BedrockEvent::ToolStart { wire_index, id, name } => {
                    let index = output.content.len(); slots.insert(wire_index, index); partial.insert(wire_index, String::new());
                    output.content.push(PiAssistantBlock::ToolCall { id: CallId::new(id), name, arguments: Map::new(), thought_signature: None });
                    yield PiAssistantEvent::ToolCallStart { content_index: index_u64(index), partial: output.clone() };
                }
                BedrockEvent::Text { wire_index, text } => {
                    let (index, opened) = ensure_text(&mut output, &mut slots, wire_index);
                    if opened { yield PiAssistantEvent::TextStart { content_index: index_u64(index), partial: output.clone() }; }
                    if let PiAssistantBlock::Text { text: value, .. } = &mut output.content[index] { value.push_str(&text); }
                    yield PiAssistantEvent::TextDelta { content_index: index_u64(index), delta: text, partial: output.clone() };
                }
                BedrockEvent::ReasoningText { wire_index, text } => {
                    let (index, opened) = ensure_thinking(&mut output, &mut slots, wire_index);
                    if opened { yield PiAssistantEvent::ThinkingStart { content_index: index_u64(index), partial: output.clone() }; }
                    if let PiAssistantBlock::Thinking { thinking, .. } = &mut output.content[index] { thinking.push_str(&text); }
                    yield PiAssistantEvent::ThinkingDelta { content_index: index_u64(index), delta: text, partial: output.clone() };
                }
                BedrockEvent::ReasoningSignature { wire_index, signature } => {
                    let (index, _) = ensure_thinking(&mut output, &mut slots, wire_index);
                    if let PiAssistantBlock::Thinking { thinking_signature, .. } = &mut output.content[index] {
                        thinking_signature.get_or_insert_with(String::new).push_str(&signature);
                    }
                }
                BedrockEvent::ToolInput { wire_index, input } => {
                    let Some(index) = slots.get(&wire_index).copied() else { continue };
                    let raw = partial.entry(wire_index).or_default(); raw.push_str(&input);
                    set_tool_arguments(&mut output.content[index], raw);
                    yield PiAssistantEvent::ToolCallDelta { content_index: index_u64(index), delta: input, partial: output.clone() };
                }
                BedrockEvent::BlockStop { wire_index } => {
                    let Some(index) = slots.remove(&wire_index) else { continue };
                    match output.content[index].clone() {
                        PiAssistantBlock::Text { text, .. } => yield PiAssistantEvent::TextEnd { content_index: index_u64(index), content: text, partial: output.clone() },
                        PiAssistantBlock::Thinking { thinking, .. } => yield PiAssistantEvent::ThinkingEnd { content_index: index_u64(index), content: thinking, partial: output.clone() },
                        PiAssistantBlock::ToolCall { id, name, thought_signature, .. } => {
                            let arguments = parse_arguments(partial.get(&wire_index).map_or("", String::as_str));
                            output.content[index] = PiAssistantBlock::ToolCall { id: id.clone(), name: name.clone(), arguments: arguments.clone(), thought_signature: thought_signature.clone() };
                            yield PiAssistantEvent::ToolCallEnd { content_index: index_u64(index), tool_call: PiToolCall { id, name, arguments, thought_signature }, partial: output.clone() };
                        }
                    }
                }
                BedrockEvent::MessageStop { reason } => {
                    let (stop, message) = map_stop_reason(&reason); output.stop_reason = stop; output.error_message = message;
                }
                BedrockEvent::Metadata { input, output: output_tokens, cache_read, cache_write, total } => {
                    output.usage = PiUsage { input, output: output_tokens, cache_read, cache_write, total_tokens: total, cost: PiCost::default() };
                }
            }
            *shared.lock() = output.clone();
        }
        if output.stop_reason == PiStopReason::Error { Err::<(), _>(anyhow::anyhow!("{}", output.error_message.as_deref().unwrap_or("An unknown error occurred")))?; }
        yield PiAssistantEvent::Done { reason: output.stop_reason, message: output };
    })
}

fn bedrock_messages(
    context: &PiContext,
    model: &PiModel,
    cache_retention: PiCacheRetention,
    environment: &HashMap<String, String>,
) -> anyhow::Result<Vec<Message>> {
    let mut projected = Vec::<(ConversationRole, Vec<ContentBlock>)>::new();
    let mut index = 0;
    while index < context.messages.len() {
        match &context.messages[index] {
            PiMessage::User(message) => {
                projected.push((ConversationRole::User, user_blocks(&message.content)?));
                index += 1;
            }
            PiMessage::Assistant(message) => {
                let blocks = assistant_blocks(message, model)?;
                if !blocks.is_empty() {
                    projected.push((ConversationRole::Assistant, blocks));
                }
                index += 1;
            }
            PiMessage::ToolResult(_) => {
                let mut blocks = Vec::new();
                while let Some(PiMessage::ToolResult(message)) = context.messages.get(index) {
                    blocks.push(ContentBlock::ToolResult(tool_result(message)?));
                    index += 1;
                }
                projected.push((ConversationRole::User, blocks));
            }
        }
    }
    if cache_retention != PiCacheRetention::None
        && supports_prompt_caching(model, environment)
        && let Some((ConversationRole::User, blocks)) = projected.last_mut()
    {
        blocks.push(ContentBlock::CachePoint(cache_point(cache_retention)?));
    }
    projected
        .into_iter()
        .map(|(role, content)| {
            Message::builder()
                .role(role)
                .set_content(Some(content))
                .build()
                .map_err(anyhow::Error::from)
        })
        .collect()
}

fn user_blocks(content: &PiUserContent) -> anyhow::Result<Vec<ContentBlock>> {
    match content {
        PiUserContent::Text(text) => Ok(vec![ContentBlock::Text(required_text(text))]),
        PiUserContent::Blocks(blocks) => {
            let mut content = Vec::new();
            for block in blocks {
                match block {
                    PiUserContentBlock::Text { text } if !text.trim().is_empty() => {
                        content.push(ContentBlock::Text(text.clone()));
                    }
                    PiUserContentBlock::Image { data, mime_type } => {
                        content.push(ContentBlock::Image(image_block(data, mime_type)?));
                    }
                    PiUserContentBlock::Text { .. } => {}
                }
            }
            if content.is_empty() {
                content.push(ContentBlock::Text("<empty>".to_owned()));
            }
            Ok(content)
        }
    }
}

fn assistant_blocks(
    message: &PiAssistantMessage,
    model: &PiModel,
) -> anyhow::Result<Vec<ContentBlock>> {
    let mut output = Vec::new();
    for block in &message.content {
        match block {
            PiAssistantBlock::Text { text, .. } if !text.trim().is_empty() => {
                output.push(ContentBlock::Text(text.clone()));
            }
            PiAssistantBlock::Thinking {
                thinking,
                thinking_signature,
                redacted,
            } => {
                if thinking.trim().is_empty() {
                    continue;
                }
                if *redacted == Some(true) {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(thinking_signature.as_deref().unwrap_or_default())?;
                    output.push(ContentBlock::ReasoningContent(
                        ReasoningContentBlock::RedactedContent(bytes.into()),
                    ));
                } else if is_anthropic_claude(model)
                    && thinking_signature.as_deref().is_none_or(str::is_empty)
                {
                    output.push(ContentBlock::Text(thinking.clone()));
                } else {
                    let mut builder = ReasoningTextBlock::builder().text(thinking);
                    if is_anthropic_claude(model)
                        && let Some(signature) = thinking_signature
                    {
                        builder = builder.signature(signature);
                    }
                    output.push(ContentBlock::ReasoningContent(
                        ReasoningContentBlock::ReasoningText(builder.build()?),
                    ));
                }
            }
            PiAssistantBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => output.push(ContentBlock::ToolUse(
                ToolUseBlock::builder()
                    .tool_use_id(id.as_str())
                    .name(name)
                    .input(document(&Value::Object(arguments.clone())))
                    .build()?,
            )),
            PiAssistantBlock::Text { .. } => {}
        }
    }
    Ok(output)
}

fn tool_result(message: &PiToolResultMessage) -> anyhow::Result<ToolResultBlock> {
    let mut content = Vec::new();
    for block in &message.content {
        match block {
            PiUserContentBlock::Text { text } if !text.trim().is_empty() => {
                content.push(ToolResultContentBlock::Text(text.clone()));
            }
            PiUserContentBlock::Image { data, mime_type } => {
                content.push(ToolResultContentBlock::Image(image_block(data, mime_type)?));
            }
            PiUserContentBlock::Text { .. } => {}
        }
    }
    if content.is_empty() {
        content.push(ToolResultContentBlock::Text("<empty>".to_owned()));
    }
    ToolResultBlock::builder()
        .tool_use_id(message.tool_call_id.as_str())
        .set_content(Some(content))
        .status(if message.is_error {
            ToolResultStatus::Error
        } else {
            ToolResultStatus::Success
        })
        .build()
        .map_err(anyhow::Error::from)
}

fn bedrock_tools(context: &PiContext) -> anyhow::Result<Option<ToolConfiguration>> {
    let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) else {
        return Ok(None);
    };
    let tools = tools
        .iter()
        .map(|tool| {
            Ok(Tool::ToolSpec(
                ToolSpecification::builder()
                    .name(&tool.name)
                    .description(&tool.description)
                    .input_schema(ToolInputSchema::Json(document(&Value::Object(
                        tool.parameters.clone(),
                    ))))
                    .build()?,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(
        ToolConfiguration::builder()
            .set_tools(Some(tools))
            .build()?,
    ))
}

fn resolve_cache_retention(request: &PiExecutionRequest) -> PiCacheRetention {
    request.options.cache_retention.unwrap_or_else(|| {
        if request
            .options
            .auth_environment
            .get("PI_CACHE_RETENTION")
            .is_some_and(|value| value == "long")
        {
            PiCacheRetention::Long
        } else {
            PiCacheRetention::Short
        }
    })
}

fn model_candidates(model: &PiModel) -> Vec<String> {
    [model.id.as_str(), model.name.as_str()]
        .into_iter()
        .flat_map(|value| {
            let lower = value.to_ascii_lowercase();
            let normalized = lower
                .chars()
                .map(|character| {
                    if matches!(character, ' ' | '_' | '.' | ':') {
                        '-'
                    } else {
                        character
                    }
                })
                .collect::<String>();
            [lower, normalized]
        })
        .collect()
}

fn is_anthropic_claude(model: &PiModel) -> bool {
    model_candidates(model)
        .iter()
        .any(|value| value.contains("claude"))
}

fn supports_prompt_caching(model: &PiModel, environment: &HashMap<String, String>) -> bool {
    let candidates = model_candidates(model);
    if !candidates.iter().any(|value| value.contains("claude")) {
        return environment
            .get("AWS_BEDROCK_FORCE_CACHE")
            .is_some_and(|value| value == "1");
    }
    candidates.iter().any(|value| {
        value.contains("fable-5")
            || value.contains("opus-5")
            || value.contains("sonnet-5")
            || value.contains("-4-")
            || value.contains("claude-3-7-sonnet")
            || value.contains("claude-3-5-haiku")
    })
}

fn cache_point(retention: PiCacheRetention) -> anyhow::Result<CachePointBlock> {
    let mut builder = CachePointBlock::builder().r#type(CachePointType::Default);
    if retention == PiCacheRetention::Long {
        builder = builder.ttl(CacheTtl::OneHour);
    }
    builder.build().map_err(anyhow::Error::from)
}

fn bedrock_system(
    prompt: Option<&str>,
    model: &PiModel,
    cache_retention: PiCacheRetention,
    environment: &HashMap<String, String>,
) -> anyhow::Result<Option<Vec<SystemContentBlock>>> {
    let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) else {
        return Ok(None);
    };
    let mut blocks = vec![SystemContentBlock::Text(prompt.to_owned())];
    if cache_retention != PiCacheRetention::None && supports_prompt_caching(model, environment) {
        blocks.push(SystemContentBlock::CachePoint(cache_point(
            cache_retention,
        )?));
    }
    Ok(Some(blocks))
}

fn supports_adaptive_thinking(model: &PiModel) -> bool {
    model_candidates(model).iter().any(|value| {
        value.contains("opus-4-6")
            || value.contains("opus-4-7")
            || value.contains("opus-4-8")
            || value.contains("opus-5")
            || value.contains("sonnet-4-6")
            || value.contains("sonnet-5")
            || value.contains("fable-5")
    })
}

fn supports_native_xhigh(model: &PiModel) -> bool {
    model_candidates(model).iter().any(|value| {
        value.contains("opus-4-7")
            || value.contains("opus-4-8")
            || value.contains("opus-5")
            || value.contains("sonnet-5")
            || value.contains("fable-5")
    })
}

fn thinking_effort(model: &PiModel, level: PiThinkingLevel) -> &'static str {
    if level == PiThinkingLevel::XHigh && supports_native_xhigh(model) {
        return "xhigh";
    }
    match level {
        PiThinkingLevel::Minimal | PiThinkingLevel::Low => "low",
        PiThinkingLevel::Medium => "medium",
        PiThinkingLevel::High
        | PiThinkingLevel::XHigh
        | PiThinkingLevel::Max
        | PiThinkingLevel::Off => "high",
    }
}

fn thinking_budget(request: &PiExecutionRequest, level: PiThinkingLevel) -> f64 {
    let selected = match level {
        PiThinkingLevel::Minimal => request
            .options
            .thinking_budgets
            .as_ref()
            .and_then(|budgets| budgets.minimal)
            .unwrap_or(1024.0),
        PiThinkingLevel::Low => request
            .options
            .thinking_budgets
            .as_ref()
            .and_then(|budgets| budgets.low)
            .unwrap_or(2048.0),
        PiThinkingLevel::Medium => request
            .options
            .thinking_budgets
            .as_ref()
            .and_then(|budgets| budgets.medium)
            .unwrap_or(8192.0),
        PiThinkingLevel::High
        | PiThinkingLevel::XHigh
        | PiThinkingLevel::Max
        | PiThinkingLevel::Off => request
            .options
            .thinking_budgets
            .as_ref()
            .and_then(|budgets| budgets.high)
            .unwrap_or(16384.0),
    };
    selected.max(0.0)
}

fn bedrock_max_tokens(request: &PiExecutionRequest) -> u64 {
    let desired = if let Some(level) = request.options.reasoning {
        if !is_anthropic_claude(&request.model) || supports_adaptive_thinking(&request.model) {
            request
                .options
                .max_tokens
                .unwrap_or(request.model.max_tokens)
        } else {
            let budget = thinking_budget(request, level);
            request
                .options
                .max_tokens
                .map_or(request.model.max_tokens, |tokens| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let budget = budget as u64;
                    tokens.saturating_add(budget).min(request.model.max_tokens)
                })
        }
    } else {
        request
            .options
            .max_tokens
            .unwrap_or(request.model.max_tokens)
    };
    clamp_max_tokens_to_context(&request.model, &request.context, desired)
}

fn clamp_max_tokens_to_context(model: &PiModel, context: &PiContext, max_tokens: u64) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(1);
    }
    let available = model
        .context_window
        .saturating_sub(estimate_context_tokens(context))
        .saturating_sub(4096)
        .max(1);
    max_tokens.min(available).max(1)
}

fn estimate_context_tokens(context: &PiContext) -> u64 {
    let mut latest_prefix_timestamp = 0_u64;
    let mut usage = None::<(usize, u64)>;
    for (index, message) in context.messages.iter().enumerate() {
        if let PiMessage::Assistant(assistant) = message {
            let total = if assistant.usage.total_tokens > 0 {
                assistant.usage.total_tokens
            } else {
                assistant
                    .usage
                    .input
                    .saturating_add(assistant.usage.output)
                    .saturating_add(assistant.usage.cache_read)
                    .saturating_add(assistant.usage.cache_write)
            };
            if assistant.timestamp >= latest_prefix_timestamp
                && !matches!(
                    assistant.stop_reason,
                    PiStopReason::Aborted | PiStopReason::Error
                )
                && total > 0
            {
                usage = Some((index, total));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }
    if let Some((index, tokens)) = usage {
        return context.messages[index + 1..]
            .iter()
            .fold(tokens, |total, message| {
                total.saturating_add(estimate_message_tokens(message))
            });
    }
    let message_tokens = context.messages.iter().fold(0_u64, |total, message| {
        total.saturating_add(estimate_message_tokens(message))
    });
    let system_tokens = context
        .system_prompt
        .as_deref()
        .map_or(0, estimate_text_tokens);
    let tool_tokens = context.tools.as_ref().map_or(0, |tools| {
        estimate_text_tokens(
            &serde_json::to_string(tools).unwrap_or_else(|_| "[unserializable]".to_owned()),
        )
    });
    message_tokens
        .saturating_add(system_tokens)
        .saturating_add(tool_tokens)
}

fn message_timestamp(message: &PiMessage) -> u64 {
    match message {
        PiMessage::User(message) => message.timestamp,
        PiMessage::Assistant(message) => message.timestamp,
        PiMessage::ToolResult(message) => message.timestamp,
    }
}

fn estimate_message_tokens(message: &PiMessage) -> u64 {
    let characters = match message {
        PiMessage::User(message) => estimate_user_content_characters(&message.content),
        PiMessage::ToolResult(message) => message.content.iter().fold(0_u64, |total, block| {
            total.saturating_add(match block {
                PiUserContentBlock::Text { text } => utf16_len(text),
                PiUserContentBlock::Image { .. } => 4800,
            })
        }),
        PiMessage::Assistant(message) => message.content.iter().fold(0_u64, |total, block| {
            total.saturating_add(match block {
                PiAssistantBlock::Text { text, .. } => utf16_len(text),
                PiAssistantBlock::Thinking { thinking, .. } => utf16_len(thinking),
                PiAssistantBlock::ToolCall {
                    name, arguments, ..
                } => utf16_len(name).saturating_add(utf16_len(
                    &serde_json::to_string(arguments)
                        .unwrap_or_else(|_| "[unserializable]".to_owned()),
                )),
            })
        }),
    };
    characters.saturating_add(3) / 4
}

fn estimate_user_content_characters(content: &PiUserContent) -> u64 {
    match content {
        PiUserContent::Text(text) => utf16_len(text),
        PiUserContent::Blocks(blocks) => blocks.iter().fold(0_u64, |total, block| {
            total.saturating_add(match block {
                PiUserContentBlock::Text { text } => utf16_len(text),
                PiUserContentBlock::Image { .. } => 4800,
            })
        }),
    }
}

fn estimate_text_tokens(text: &str) -> u64 {
    utf16_len(text).saturating_add(3) / 4
}

fn utf16_len(text: &str) -> u64 {
    u64::try_from(text.encode_utf16().count()).unwrap_or(u64::MAX)
}

fn bedrock_additional_fields(request: &PiExecutionRequest) -> Option<Value> {
    let level = request.options.reasoning?;
    if !request.model.reasoning || !is_anthropic_claude(&request.model) {
        return None;
    }
    let region = request
        .options
        .auth_environment
        .get("AWS_REGION")
        .or_else(|| request.options.auth_environment.get("AWS_DEFAULT_REGION"));
    let gov_cloud = region.is_some_and(|region| region.to_ascii_lowercase().starts_with("us-gov-"))
        || request.model.id.as_str().starts_with("us-gov.")
        || request.model.id.as_str().starts_with("arn:aws-us-gov:");
    let display = (!gov_cloud).then_some("summarized");
    if supports_adaptive_thinking(&request.model) {
        let mut thinking = Map::from_iter([("type".to_owned(), json!("adaptive"))]);
        if let Some(display) = display {
            thinking.insert("display".to_owned(), json!(display));
        }
        return Some(json!({
            "thinking":thinking,
            "output_config":{"effort":thinking_effort(&request.model, level)}
        }));
    }
    let budget_ceiling = Value::from(bedrock_max_tokens(request).saturating_sub(1024))
        .as_f64()
        .unwrap_or(f64::MAX);
    let budget = thinking_budget(request, level).min(budget_ceiling);
    let mut thinking = Map::from_iter([
        ("type".to_owned(), json!("enabled")),
        ("budget_tokens".to_owned(), json!(budget)),
    ]);
    if let Some(display) = display {
        thinking.insert("display".to_owned(), json!(display));
    }
    Some(json!({
        "thinking":thinking,
        "anthropic_beta":["interleaved-thinking-2025-05-14"]
    }))
}

fn image_block(
    data: &str,
    mime_type: &str,
) -> anyhow::Result<aws_sdk_bedrockruntime::types::ImageBlock> {
    let format = match mime_type {
        "image/png" => aws_sdk_bedrockruntime::types::ImageFormat::Png,
        "image/jpeg" | "image/jpg" => aws_sdk_bedrockruntime::types::ImageFormat::Jpeg,
        "image/gif" => aws_sdk_bedrockruntime::types::ImageFormat::Gif,
        "image/webp" => aws_sdk_bedrockruntime::types::ImageFormat::Webp,
        _ => anyhow::bail!("unsupported Bedrock image media type {mime_type}"),
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    Ok(aws_sdk_bedrockruntime::types::ImageBlock::builder()
        .format(format)
        .source(aws_sdk_bedrockruntime::types::ImageSource::Bytes(
            bytes.into(),
        ))
        .build()?)
}

fn required_text(text: &str) -> String {
    if text.trim().is_empty() {
        "<empty>".to_owned()
    } else {
        text.to_owned()
    }
}

fn document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(value) => Document::Bool(*value),
        Value::Number(value) => value.as_u64().map_or_else(
            || {
                value.as_i64().map_or_else(
                    || Document::Number(Number::Float(value.as_f64().unwrap_or_default())),
                    |value| Document::Number(Number::NegInt(value)),
                )
            },
            |value| Document::Number(Number::PosInt(value)),
        ),
        Value::String(value) => Document::String(value.clone()),
        Value::Array(values) => Document::Array(values.iter().map(document).collect()),
        Value::Object(values) => Document::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), document(value)))
                .collect(),
        ),
    }
}

fn bedrock_region(model: &PiModel) -> Option<Region> {
    let id = model.id.as_str();
    if let Some(rest) = id.strip_prefix("arn:") {
        let parts = rest.split(':').collect::<Vec<_>>();
        if parts
            .first()
            .is_some_and(|partition| partition.starts_with("aws"))
            && parts.get(1) == Some(&"bedrock")
            && let Some(region) = parts.get(2).filter(|region| !region.is_empty())
        {
            return Some(Region::new((*region).to_owned()));
        }
    }
    None
}

fn explicit_endpoint(
    base_url: &str,
    configured_region: Option<&str>,
    profile_configured: bool,
) -> Option<String> {
    let value = base_url.trim().trim_end_matches('/');
    if value.is_empty() {
        return None;
    }
    if standard_bedrock_endpoint_region(value).is_some()
        && (configured_region.is_some() || profile_configured)
    {
        return None;
    }
    Some(value.to_owned())
}

fn standard_bedrock_endpoint_region(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let rest = host
        .strip_prefix("bedrock-runtime.")
        .or_else(|| host.strip_prefix("bedrock-runtime-fips."))?;
    let region = rest
        .strip_suffix(".amazonaws.com")
        .or_else(|| rest.strip_suffix(".amazonaws.com.cn"))?;
    (!region.is_empty()).then(|| region.to_owned())
}

fn format_bedrock_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    let retention_hint = if text.to_ascii_lowercase().contains("data retention mode") {
        " See https://docs.aws.amazon.com/bedrock/latest/userguide/data-retention.html for supported data retention modes."
    } else {
        ""
    };
    for (name, prefix) in [
        ("InternalServerException", "Internal server error"),
        ("ModelStreamErrorException", "Model stream error"),
        ("ValidationException", "Validation error"),
        ("ThrottlingException", "Throttling error"),
        ("ServiceUnavailableException", "Service unavailable"),
    ] {
        if text.contains(name) {
            return format!("{prefix}: {text}{retention_hint}");
        }
    }
    format!("{text}{retention_hint}")
}

fn map_stop_reason(reason: &str) -> (PiStopReason, Option<String>) {
    match reason {
        "end_turn" | "stop_sequence" => (PiStopReason::Stop, None),
        "max_tokens" => (PiStopReason::Length, None),
        "tool_use" => (PiStopReason::ToolUse, None),
        "model_context_window_exceeded" => (
            PiStopReason::Error,
            Some("model_context_window_exceeded".to_owned()),
        ),
        other => (
            PiStopReason::Error,
            Some(format!("Bedrock stop reason: {other}")),
        ),
    }
}

fn ensure_text(
    output: &mut PiAssistantMessage,
    slots: &mut HashMap<i32, usize>,
    wire: i32,
) -> (usize, bool) {
    if let Some(index) = slots.get(&wire) {
        return (*index, false);
    }
    let index = output.content.len();
    slots.insert(wire, index);
    output.content.push(PiAssistantBlock::Text {
        text: String::new(),
        text_signature: None,
    });
    (index, true)
}
fn ensure_thinking(
    output: &mut PiAssistantMessage,
    slots: &mut HashMap<i32, usize>,
    wire: i32,
) -> (usize, bool) {
    if let Some(index) = slots.get(&wire) {
        return (*index, false);
    }
    let index = output.content.len();
    slots.insert(wire, index);
    output.content.push(PiAssistantBlock::Thinking {
        thinking: String::new(),
        thinking_signature: Some(String::new()),
        redacted: None,
    });
    (index, true)
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
fn nonnegative(value: i32) -> u64 {
    u64::try_from(value).unwrap_or_default()
}
fn index_u64(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod request_tests {
    use seekdeep_llm::{CallId, ProviderId};

    use crate::{
        adapter::{PiExecutionRequest, PiStreamOptions},
        catalog::builtin_catalog,
        config::resolve_profiles,
        context::{
            PiContext, PiMessage, PiToolResultMessage, PiToolResultRole, PiUserContent,
            PiUserContentBlock, PiUserMessage, PiUserRole,
        },
    };

    use super::*;

    fn request(model_id: &str, model_name: &str) -> PiExecutionRequest {
        let profiles =
            resolve_profiles(Some(&json!({"amazon-bedrock":{}})), builtin_catalog()).unwrap();
        let profile = &profiles["amazon-bedrock"];
        let mut model = profile.pi_provider.models[0].clone();
        model.id = seekdeep_llm::ModelId::new(model_id);
        model.name = model_name.to_owned();
        model.provider = ProviderId::new("amazon-bedrock");
        model.reasoning = true;
        model.max_tokens = 64_000;
        PiExecutionRequest {
            provider: profile.pi_provider.clone(),
            model,
            context: PiContext {
                system_prompt: Some("system".to_owned()),
                messages: vec![PiMessage::User(PiUserMessage {
                    role: PiUserRole::User,
                    content: PiUserContent::Text("hello".to_owned()),
                    timestamp: 0,
                })],
                tools: None,
            },
            options: PiStreamOptions::default(),
        }
    }

    #[test]
    fn cache_points_follow_model_support_and_long_ttl() {
        let request = request("anthropic.claude-sonnet-4-20250514-v1:0", "Claude Sonnet 4");
        let system = bedrock_system(
            request.context.system_prompt.as_deref(),
            &request.model,
            PiCacheRetention::Long,
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(system[0], SystemContentBlock::Text(_)));
        let SystemContentBlock::CachePoint(point) = &system[1] else {
            panic!("expected system cache point")
        };
        assert_eq!(point.ttl(), Some(&CacheTtl::OneHour));

        let messages = bedrock_messages(
            &request.context,
            &request.model,
            PiCacheRetention::Long,
            &HashMap::new(),
        )
        .unwrap();
        assert!(matches!(
            messages[0].content().last(),
            Some(ContentBlock::CachePoint(point)) if point.ttl() == Some(&CacheTtl::OneHour)
        ));
    }

    #[test]
    fn reasoning_fields_cover_budget_adaptive_native_effort_and_govcloud() {
        let mut budget = request(
            "anthropic.claude-3-7-sonnet-20250219-v1:0",
            "Claude 3.7 Sonnet",
        );
        budget.options.reasoning = Some(PiThinkingLevel::High);
        budget.options.max_tokens = Some(2_048);
        assert_eq!(bedrock_max_tokens(&budget), 18_432);
        assert_eq!(
            bedrock_additional_fields(&budget).unwrap(),
            json!({
                "thinking":{"type":"enabled","budget_tokens":16384.0,"display":"summarized"},
                "anthropic_beta":["interleaved-thinking-2025-05-14"]
            })
        );

        let mut adaptive = request("anthropic.claude-opus-4-7-v1:0", "Claude Opus 4.7");
        adaptive.options.reasoning = Some(PiThinkingLevel::XHigh);
        assert_eq!(
            bedrock_additional_fields(&adaptive).unwrap(),
            json!({
                "thinking":{"type":"adaptive","display":"summarized"},
                "output_config":{"effort":"xhigh"}
            })
        );
        adaptive
            .options
            .auth_environment
            .insert("AWS_REGION".to_owned(), "us-gov-west-1".to_owned());
        assert_eq!(
            bedrock_additional_fields(&adaptive).unwrap(),
            json!({
                "thinking":{"type":"adaptive"},
                "output_config":{"effort":"xhigh"}
            })
        );
    }

    #[test]
    fn consecutive_tool_results_group_images_and_empty_content() {
        let mut request = request("amazon.nova-pro-v1:0", "Nova Pro");
        request.context.messages = vec![
            PiMessage::ToolResult(PiToolResultMessage {
                role: PiToolResultRole::ToolResult,
                tool_call_id: CallId::new("first"),
                tool_name: "one".to_owned(),
                content: vec![PiUserContentBlock::Image {
                    data: "AQ==".to_owned(),
                    mime_type: "image/png".to_owned(),
                }],
                is_error: false,
                timestamp: 0,
            }),
            PiMessage::ToolResult(PiToolResultMessage {
                role: PiToolResultRole::ToolResult,
                tool_call_id: CallId::new("second"),
                tool_name: "two".to_owned(),
                content: vec![PiUserContentBlock::Text {
                    text: " ".to_owned(),
                }],
                is_error: true,
                timestamp: 0,
            }),
        ];
        let messages = bedrock_messages(
            &request.context,
            &request.model,
            PiCacheRetention::None,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content().len(), 2);
        let ContentBlock::ToolResult(first) = &messages[0].content()[0] else {
            panic!("expected first tool result")
        };
        assert!(matches!(
            first.content()[0],
            ToolResultContentBlock::Image(_)
        ));
        let ContentBlock::ToolResult(second) = &messages[0].content()[1] else {
            panic!("expected second tool result")
        };
        assert!(matches!(
            &second.content()[0],
            ToolResultContentBlock::Text(text) if text == "<empty>"
        ));
        assert_eq!(second.status(), Some(&ToolResultStatus::Error));
    }

    #[test]
    fn custom_headers_override_case_insensitively_and_never_replace_sigv4_fields() {
        let mut request = request("anthropic.claude-sonnet-4-v1:0", "Claude Sonnet 4");
        request.model.extra.insert(
            "headers".to_owned(),
            json!({
                "Authorization":"forbidden",
                "Host":"forbidden",
                "X-Amz-Date":"forbidden",
                "X-Deployment":"old"
            }),
        );
        request
            .options
            .headers
            .insert("x-deployment".to_owned(), "new".to_owned());
        request
            .options
            .headers
            .insert("x-trace".to_owned(), "trace".to_owned());
        let headers = bedrock_headers(&request).unwrap();
        let values = headers
            .into_iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.to_str().unwrap().to_owned()))
            .collect::<HashMap<_, _>>();
        assert_eq!(values.len(), 2);
        assert_eq!(values["x-deployment"], "new");
        assert_eq!(values["x-trace"], "trace");
    }

    #[test]
    fn output_cap_reserves_context_safety_using_pi_ai_utf16_estimator() {
        let mut request = request("amazon.nova-pro-v1:0", "Nova Pro");
        request.model.context_window = 5_000;
        request.model.reasoning = false;
        request.context.system_prompt = None;
        request.context.messages = vec![PiMessage::User(PiUserMessage {
            role: PiUserRole::User,
            content: PiUserContent::Text("x".repeat(400)),
            timestamp: 0,
        })];
        assert_eq!(estimate_context_tokens(&request.context), 100);
        assert_eq!(bedrock_max_tokens(&request), 804);
        request.context.messages = vec![PiMessage::User(PiUserMessage {
            role: PiUserRole::User,
            content: PiUserContent::Text("😀".to_owned()),
            timestamp: 0,
        })];
        assert_eq!(estimate_context_tokens(&request.context), 1);
    }

    #[test]
    fn standard_endpoint_is_pinned_only_without_explicit_region_or_profile() {
        let standard = "https://bedrock-runtime.eu-west-1.amazonaws.com";
        assert_eq!(
            standard_bedrock_endpoint_region(standard).as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(
            explicit_endpoint(standard, None, false).as_deref(),
            Some(standard)
        );
        assert!(explicit_endpoint(standard, Some("us-west-2"), false).is_none());
        assert!(explicit_endpoint(standard, None, true).is_none());
        assert_eq!(
            explicit_endpoint(
                "https://bedrock.proxy.test/runtime",
                Some("us-west-2"),
                true
            )
            .as_deref(),
            Some("https://bedrock.proxy.test/runtime")
        );
    }

    #[test]
    fn proxy_client_uses_target_scheme_and_validates_snapshot_proxy_url() {
        let https = HashMap::from([
            (
                "HTTPS_PROXY".to_owned(),
                "http://proxy.example.test:8080".to_owned(),
            ),
            ("NO_PROXY".to_owned(), "localhost,.internal".to_owned()),
        ]);
        assert!(
            bedrock_http_client(&https, "https://bedrock-runtime.us-east-1.amazonaws.com")
                .unwrap()
                .is_some()
        );
        assert!(
            bedrock_http_client(&HashMap::new(), "https://bedrock.example")
                .unwrap()
                .is_none()
        );
        assert!(
            bedrock_http_client(
                &HashMap::from([("HTTP_PROXY".to_owned(), "not a url".to_owned())]),
                "http://bedrock.example"
            )
            .is_err()
        );
        assert!(
            bedrock_http_client(
                &HashMap::from([("AWS_BEDROCK_FORCE_HTTP1".to_owned(), "1".to_owned())]),
                "https://bedrock.example"
            )
            .unwrap()
            .is_some()
        );
    }
}
