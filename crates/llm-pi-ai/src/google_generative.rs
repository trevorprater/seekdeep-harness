//! Google Generative AI native protocol engine.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::StreamExt as _;
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use seekdeep_llm::CallId;
use seekdeep_llm_deepseek::sse::{ByteStream, parse_sse};
use serde_json::{Map, Value, json};

use crate::{
    adapter::{BoxPiEventStream, PiExecutionRequest, PiProtocolExecutor},
    catalog::{PiModel, PiThinkingLevel},
    context::{PiContext, PiMessage, PiUserContent, PiUserContentBlock},
    replay::{
        PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiResponseId, PiStopReason,
        PiUsage,
    },
    stream::{PiAssistantEvent, PiToolCall},
};

static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Reqwest-backed Google Generative AI engine.
#[derive(Clone, Debug)]
pub struct GoogleGenerativeExecutor {
    http: reqwest::Client,
    flavor: GoogleFlavor,
    vertex_project: Option<String>,
    vertex_location: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoogleFlavor {
    Public,
    Vertex,
}
impl GoogleGenerativeExecutor {
    /// Creates an executor using one reusable HTTP client.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: GoogleFlavor::Public,
            vertex_project: None,
            vertex_location: None,
        }
    }

    /// Creates the Vertex AI project/location/ADC flavor.
    #[must_use]
    pub fn new_vertex(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: GoogleFlavor::Vertex,
            vertex_project: None,
            vertex_location: None,
        }
    }

    /// Creates a Vertex flavor with explicit project and location facts.
    #[must_use]
    pub fn new_vertex_configured(
        http: reqwest::Client,
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        Self {
            http,
            flavor: GoogleFlavor::Vertex,
            vertex_project: Some(project.into()),
            vertex_location: Some(location.into()),
        }
    }
}

impl PiProtocolExecutor for GoogleGenerativeExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        let expected_api = if self.flavor == GoogleFlavor::Public {
            "google-generative-ai"
        } else {
            "google-vertex"
        };
        if request.model.api.as_str() != expected_api {
            anyhow::bail!(
                "native Google executor cannot dispatch api \"{}\"",
                request.model.api.as_str()
            );
        }
        let api_key = request.options.api_key.clone().filter(|key| {
            !key.is_empty()
                && key != "gcp-vertex-credentials"
                && !(key.starts_with('<') && key.ends_with('>'))
        });
        if self.flavor == GoogleFlavor::Public && api_key.is_none() {
            anyhow::bail!(
                "No API key for provider: {}",
                request.model.provider.as_str()
            );
        }
        let output = Arc::new(Mutex::new(empty_assistant(&request.model)));
        let signal = request.options.signal.clone();
        let native = native_events(
            self.http.clone(),
            request,
            api_key,
            output.clone(),
            self.flavor,
            self.vertex_project.clone(),
            self.vertex_location.clone(),
        );
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenBlock {
    Text(usize),
    Thinking(usize),
}

struct GoogleTarget {
    url: String,
    api_key: Option<String>,
    bearer: Option<String>,
}

async fn google_target(
    model: &PiModel,
    flavor: GoogleFlavor,
    api_key: Option<&str>,
    configured_project: Option<&str>,
    configured_location: Option<&str>,
    configured_credentials_path: Option<&str>,
) -> anyhow::Result<GoogleTarget> {
    if flavor == GoogleFlavor::Public {
        return Ok(GoogleTarget {
            url: format!(
                "{}/models/{}:streamGenerateContent",
                model.base_url.trim_end_matches('/'),
                model.id.as_str()
            ),
            api_key: api_key.map(str::to_owned),
            bearer: None,
        });
    }
    let custom_base = (!model.base_url.trim().is_empty() && !model.base_url.contains("{location}"))
        .then(|| model.base_url.trim().trim_end_matches('/').to_owned());
    let needs_version = custom_base
        .as_deref()
        .is_none_or(|base| !base_url_includes_api_version(base));
    let version = if needs_version { "/v1" } else { "" };
    if let Some(api_key) = api_key {
        let base = custom_base.unwrap_or_else(|| "https://aiplatform.googleapis.com".to_owned());
        return Ok(GoogleTarget {
            url: format!(
                "{base}{version}/publishers/google/models/{}:streamGenerateContent",
                model.id.as_str()
            ),
            api_key: Some(api_key.to_owned()),
            bearer: None,
        });
    }
    let project = configured_project
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("GOOGLE_CLOUD_PROJECT")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| std::env::var("GCLOUD_PROJECT").ok().filter(|value| !value.is_empty()))
        })
        .ok_or_else(|| anyhow::anyhow!(
            "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options."
        ))?;
    let location = configured_location
        .map(str::to_owned)
        .or_else(|| std::env::var("GOOGLE_CLOUD_LOCATION").ok().filter(|value| !value.is_empty()))
        .ok_or_else(|| anyhow::anyhow!(
            "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options."
        ))?;
    let base =
        custom_base.unwrap_or_else(|| format!("https://{location}-aiplatform.googleapis.com"));
    let bearer = Some(vertex_access_token(configured_credentials_path).await?);
    Ok(GoogleTarget {
        url: format!(
            "{base}{version}/projects/{project}/locations/{location}/publishers/google/models/{}:streamGenerateContent",
            model.id.as_str()
        ),
        api_key: None,
        bearer,
    })
}

async fn vertex_access_token(credentials_path: Option<&str>) -> anyhow::Result<String> {
    use google_cloud_auth::credentials::{
        AccessTokenCredentials, external_account, impersonated, service_account, user_account,
    };

    const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
    let credentials: AccessTokenCredentials = if let Some(path) = credentials_path {
        let value: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
        match value.get("type").and_then(Value::as_str) {
            Some("authorized_user") => user_account::Builder::new(value)
                .with_scopes([SCOPE])
                .build_access_token_credentials()?,
            Some("service_account") => service_account::Builder::new(value)
                .with_access_specifier(service_account::AccessSpecifier::from_scopes([SCOPE]))
                .build_access_token_credentials()?,
            Some("impersonated_service_account") => impersonated::Builder::new(value)
                .with_scopes([SCOPE])
                .build_access_token_credentials()?,
            Some("external_account") => external_account::Builder::new(value)
                .with_scopes([SCOPE])
                .build_access_token_credentials()?,
            Some(kind) => anyhow::bail!("unsupported Google credential type: {kind}"),
            None => anyhow::bail!("Google credential file has no type field"),
        }
    } else {
        google_cloud_auth::credentials::Builder::default()
            .with_scopes([SCOPE])
            .build_access_token_credentials()?
    };
    Ok(credentials.access_token().await?.token)
}

fn base_url_includes_api_version(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).map_or_else(
        |_| base_url.split('/').any(is_api_version_segment),
        |url| {
            url.path_segments()
                .is_some_and(|segments| segments.into_iter().any(is_api_version_segment))
        },
    )
}

fn is_api_version_segment(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let suffix = &rest[digits..];
    suffix.is_empty()
        || suffix
            .strip_prefix("beta")
            .is_some_and(|tail| tail.bytes().all(|byte| byte.is_ascii_digit()))
}

#[allow(clippy::too_many_lines)] // One ordered provider stream state machine.
fn native_events(
    http: reqwest::Client,
    request: PiExecutionRequest,
    api_key: Option<String>,
    shared: Arc<Mutex<PiAssistantMessage>>,
    flavor: GoogleFlavor,
    vertex_project: Option<String>,
    vertex_location: Option<String>,
) -> BoxPiEventStream {
    Box::pin(async_stream::try_stream! {
        let body = build_request(&request.model, &request.context, &request.options);
        if body
            .get("contents")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            Err::<(), _>(anyhow::anyhow!("contents are required"))?;
        }
        let target = google_target(
            &request.model,
            flavor,
            api_key.as_deref(),
            vertex_project.as_deref().or_else(|| request.options.auth_environment
                .get("GOOGLE_CLOUD_PROJECT").map(String::as_str)),
            vertex_location.as_deref().or_else(|| request.options.auth_environment
                .get("GOOGLE_CLOUD_LOCATION").map(String::as_str)),
            request.options.auth_environment
                .get("GOOGLE_APPLICATION_CREDENTIALS").map(String::as_str),
        ).await?;
        let mut builder = http.post(target.url).query(&[("alt", "sse")])
            .headers(request_headers(&request)?)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body)?);
        if let Some(api_key) = target.api_key.as_deref() {
            builder = builder.header("x-goog-api-key", api_key);
        }
        if let Some(bearer) = target.bearer.as_deref() {
            builder = builder.bearer_auth(bearer);
        }
        if let Some(timeout_ms) = request.options.timeout_ms { builder = builder.timeout(Duration::from_millis(timeout_ms)); }
        let response_result: anyhow::Result<reqwest::Response> = tokio::select! {
            biased;
            () = request.options.signal.cancelled() => Err(anyhow::anyhow!("Request aborted")),
            response = builder.send() => response.map_err(anyhow::Error::from),
        };
        let response = response_result?;
        let response = if response.status().is_success() { response } else {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text).ok()
                .and_then(|body| body.get("error")?.get("message")?.as_str().map(str::to_owned))
                .filter(|message| !message.is_empty()).unwrap_or(text);
            Err::<reqwest::Response, _>(anyhow::anyhow!("Google API error ({status}): {detail}"))?
        };
        let bytes: ByteStream = Box::pin(response.bytes_stream().map(|result| result.map_err(anyhow::Error::from)));
        let mut payloads = parse_sse(bytes, None);
        let mut output = shared.lock().clone();
        let mut current = None::<OpenBlock>;
        let mut saw_finish = false;
        yield PiAssistantEvent::Start { partial: output.clone() };
        loop {
            let payload = match payloads.next().await {
                Some(Ok(payload)) => payload,
                Some(Err(error)) if error.to_string().contains("without [DONE]") => break,
                Some(Err(error)) => Err::<String, _>(error)?,
                None => break,
            };
            let chunk: Value = serde_json::from_str(&payload)?;
            if output.response_id.is_none() {
                output.response_id = chunk.get("responseId").and_then(Value::as_str).map(PiResponseId::new);
            }
            if let Some(candidate) = chunk.get("candidates").and_then(Value::as_array).and_then(|values| values.first()) {
                if let Some(parts) = candidate.get("content").and_then(|value| value.get("parts")).and_then(Value::as_array) {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            let thinking = part.get("thought").and_then(Value::as_bool) == Some(true);
                            let expected = if thinking { "thinking" } else { "text" };
                            if !matches!(current, Some(OpenBlock::Thinking(_)) if thinking)
                                && !matches!(current, Some(OpenBlock::Text(_)) if !thinking)
                            {
                                if let Some(block) = current.take() { yield close_block(block, &output); }
                                let index = output.content.len();
                                current = Some(if thinking {
                                    output.content.push(PiAssistantBlock::Thinking {
                                        thinking: String::new(),
                                        thinking_signature: part.get("thoughtSignature").and_then(Value::as_str).map(str::to_owned),
                                        redacted: None,
                                    });
                                    PiAssistantEvent::ThinkingStart { content_index: index_u64(index), partial: output.clone() }
                                } else {
                                    output.content.push(PiAssistantBlock::Text {
                                        text: String::new(), text_signature: part.get("thoughtSignature").and_then(Value::as_str).map(str::to_owned),
                                    });
                                    PiAssistantEvent::TextStart { content_index: index_u64(index), partial: output.clone() }
                                }.into_open_block(index));
                                yield if expected == "thinking" {
                                    PiAssistantEvent::ThinkingStart { content_index: index_u64(index), partial: output.clone() }
                                } else {
                                    PiAssistantEvent::TextStart { content_index: index_u64(index), partial: output.clone() }
                                };
                            }
                            let Some(OpenBlock::Text(index) | OpenBlock::Thinking(index)) = current else {
                                unreachable!("text part opened a text or thinking block")
                            };
                            match &mut output.content[index] {
                                PiAssistantBlock::Text { text: value, text_signature } => {
                                    value.push_str(text);
                                    retain_signature(text_signature, part);
                                    yield PiAssistantEvent::TextDelta { content_index: index_u64(index), delta: text.to_owned(), partial: output.clone() };
                                }
                                PiAssistantBlock::Thinking { thinking: value, thinking_signature, .. } => {
                                    value.push_str(text);
                                    retain_signature(thinking_signature, part);
                                    yield PiAssistantEvent::ThinkingDelta { content_index: index_u64(index), delta: text.to_owned(), partial: output.clone() };
                                }
                                PiAssistantBlock::ToolCall { .. } => unreachable!(),
                            }
                        }
                        if let Some(call) = part.get("functionCall") {
                            if let Some(block) = current.take() { yield close_block(block, &output); }
                            let name = call.get("name").and_then(Value::as_str).unwrap_or_default();
                            let provided = call.get("id").and_then(Value::as_str);
                            let id = provided
                                .filter(|id| !output.content.iter().any(|block| matches!(block, PiAssistantBlock::ToolCall { id: held, .. } if held.as_str() == *id)))
                                .map_or_else(
                                    || format!("{name}_{}_{}", chrono::Utc::now().timestamp_millis(), TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)),
                                    str::to_owned,
                                );
                            let arguments = call.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
                            let thought_signature = part.get("thoughtSignature").and_then(Value::as_str).map(str::to_owned);
                            let index = output.content.len();
                            output.content.push(PiAssistantBlock::ToolCall {
                                id: CallId::new(id.clone()), name: name.to_owned(), arguments: arguments.clone(), thought_signature: thought_signature.clone(),
                            });
                            yield PiAssistantEvent::ToolCallStart { content_index: index_u64(index), partial: output.clone() };
                            yield PiAssistantEvent::ToolCallDelta {
                                content_index: index_u64(index), delta: crate::json::stringify_object(&arguments)?, partial: output.clone(),
                            };
                            yield PiAssistantEvent::ToolCallEnd {
                                content_index: index_u64(index),
                                tool_call: PiToolCall { id: CallId::new(id), name: name.to_owned(), arguments, thought_signature }, partial: output.clone(),
                            };
                        }
                    }
                }
                if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                    output.stop_reason = match reason {
                        "STOP" => PiStopReason::Stop,
                        "MAX_TOKENS" => PiStopReason::Length,
                        _ => PiStopReason::Error,
                    };
                    if output.content.iter().any(|block| matches!(block, PiAssistantBlock::ToolCall { .. })) {
                        output.stop_reason = PiStopReason::ToolUse;
                    }
                    saw_finish = true;
                }
            }
            if let Some(usage) = chunk.get("usageMetadata") {
                let cache_read = number(usage.get("cachedContentTokenCount"));
                let thoughts = number(usage.get("thoughtsTokenCount"));
                output.usage = PiUsage {
                    input: number(usage.get("promptTokenCount")).saturating_sub(cache_read),
                    output: number(usage.get("candidatesTokenCount")).saturating_add(thoughts),
                    cache_read, cache_write: 0,
                    total_tokens: number(usage.get("totalTokenCount")), cost: PiCost::default(),
                };
            }
            publish(&shared, &output);
        }
        if let Some(block) = current.take() { yield close_block(block, &output); }
        if request.options.signal.is_aborted() { Err::<(), _>(anyhow::anyhow!("Request aborted"))?; }
        if output.stop_reason == PiStopReason::Error && saw_finish { Err::<(), _>(anyhow::anyhow!("An unknown error occurred"))?; }
        publish(&shared, &output);
        yield PiAssistantEvent::Done { reason: output.stop_reason, message: output };
    })
}

trait EventOpenBlock {
    fn into_open_block(self, index: usize) -> OpenBlock;
}
impl EventOpenBlock for PiAssistantEvent {
    fn into_open_block(self, index: usize) -> OpenBlock {
        match self {
            PiAssistantEvent::ThinkingStart { .. } => OpenBlock::Thinking(index),
            PiAssistantEvent::TextStart { .. } => OpenBlock::Text(index),
            _ => unreachable!(),
        }
    }
}

fn build_request(
    model: &PiModel,
    context: &PiContext,
    options: &crate::adapter::PiStreamOptions,
) -> Value {
    let mut config = Map::new();
    if let Some(temperature) = options.temperature {
        config.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        config.insert("maxOutputTokens".to_owned(), Value::from(max_tokens));
    }
    if model.reasoning {
        let thinking = options.reasoning.map_or_else(
            || disabled_thinking(model),
            |level| enabled_thinking(model, level, options.thinking_budgets.as_ref()),
        );
        config.insert("thinkingConfig".to_owned(), thinking);
    }
    let mut request = Map::from_iter([
        (
            "contents".to_owned(),
            Value::Array(convert_messages(model, context)),
        ),
        ("generationConfig".to_owned(), Value::Object(config)),
    ]);
    if let Some(system) = &context.system_prompt {
        request.insert(
            "systemInstruction".to_owned(),
            json!({"parts":[{"text":system}],"role":"user"}),
        );
    }
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        request.insert(
            "tools".to_owned(),
            json!([{"functionDeclarations":tools.iter().map(|tool| json!({
                "name":tool.name,"description":tool.description,"parametersJsonSchema":tool.parameters
            })).collect::<Vec<_>>() }]),
        );
    }
    Value::Object(request)
}

#[allow(clippy::too_many_lines)] // Closed Google history vocabulary conversion.
fn convert_messages(model: &PiModel, context: &PiContext) -> Vec<Value> {
    let mut contents = Vec::new();
    for message in &context.messages {
        match message {
            PiMessage::User(message) => {
                let parts = match &message.content {
                    PiUserContent::Text(text) => vec![json!({"text":text})],
                    PiUserContent::Blocks(blocks) => blocks.iter().map(google_input).collect(),
                };
                if !parts.is_empty() {
                    contents.push(json!({"role":"user","parts":parts}));
                }
            }
            PiMessage::Assistant(message) => {
                let same = message.provider == model.provider && message.model == model.id;
                let mut parts = Vec::new();
                for block in &message.content {
                    match block {
                        PiAssistantBlock::Text {
                            text,
                            text_signature,
                        } if !text.trim().is_empty() => {
                            let mut part =
                                Map::from_iter([("text".to_owned(), Value::String(text.clone()))]);
                            if same && valid_signature(text_signature.as_deref()) {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String(text_signature.clone().unwrap_or_default()),
                                );
                            }
                            parts.push(Value::Object(part));
                        }
                        PiAssistantBlock::Thinking {
                            thinking,
                            thinking_signature,
                            ..
                        } if !thinking.trim().is_empty() => {
                            let mut part = Map::from_iter([(
                                "text".to_owned(),
                                Value::String(thinking.clone()),
                            )]);
                            if same {
                                part.insert("thought".to_owned(), Value::Bool(true));
                            }
                            if same && valid_signature(thinking_signature.as_deref()) {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String(thinking_signature.clone().unwrap_or_default()),
                                );
                            }
                            parts.push(Value::Object(part));
                        }
                        PiAssistantBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            thought_signature,
                        } => {
                            let mut part = Map::from_iter([(
                                "functionCall".to_owned(),
                                json!({"name":name,"args":arguments}),
                            )]);
                            if requires_tool_id(model.id.as_str()) {
                                part.get_mut("functionCall")
                                    .and_then(Value::as_object_mut)
                                    .unwrap()
                                    .insert(
                                        "id".to_owned(),
                                        Value::String(normalize_id(id.as_str())),
                                    );
                            }
                            if same && valid_signature(thought_signature.as_deref()) {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String(thought_signature.clone().unwrap_or_default()),
                                );
                            }
                            parts.push(Value::Object(part));
                        }
                        PiAssistantBlock::Text { .. } | PiAssistantBlock::Thinking { .. } => {}
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({"role":"model","parts":parts}));
                }
            }
            PiMessage::ToolResult(message) => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        PiUserContentBlock::Text { text } => Some(text.as_str()),
                        PiUserContentBlock::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let key = if message.is_error { "error" } else { "output" };
                let mut response = Map::new();
                response.insert(key.to_owned(), Value::String(text));
                let mut function = Map::from_iter([
                    ("name".to_owned(), Value::String(message.tool_name.clone())),
                    ("response".to_owned(), Value::Object(response)),
                ]);
                if requires_tool_id(model.id.as_str()) {
                    function.insert(
                        "id".to_owned(),
                        Value::String(normalize_id(message.tool_call_id.as_str())),
                    );
                }
                contents.push(json!({"role":"user","parts":[{"functionResponse":function}]}));
            }
        }
    }
    contents
}

fn google_input(block: &PiUserContentBlock) -> Value {
    match block {
        PiUserContentBlock::Text { text } => json!({"text":text}),
        PiUserContentBlock::Image { data, mime_type } => {
            json!({"inlineData":{"mimeType":mime_type,"data":data}})
        }
    }
}

fn request_headers(request: &PiExecutionRequest) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    if let Some(model_headers) = request
        .model
        .extra
        .get("headers")
        .and_then(Value::as_object)
    {
        for (name, value) in model_headers {
            if let Some(value) = value.as_str() {
                headers.insert(
                    HeaderName::from_bytes(name.as_bytes())?,
                    HeaderValue::from_str(value)?,
                );
            }
        }
    }
    for (name, value) in &request.options.headers {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(headers)
}

fn close_block(block: OpenBlock, output: &PiAssistantMessage) -> PiAssistantEvent {
    match block {
        OpenBlock::Text(index) => PiAssistantEvent::TextEnd {
            content_index: index_u64(index),
            content: match &output.content[index] {
                PiAssistantBlock::Text { text, .. } => text.clone(),
                _ => String::new(),
            },
            partial: output.clone(),
        },
        OpenBlock::Thinking(index) => PiAssistantEvent::ThinkingEnd {
            content_index: index_u64(index),
            content: match &output.content[index] {
                PiAssistantBlock::Thinking { thinking, .. } => thinking.clone(),
                _ => String::new(),
            },
            partial: output.clone(),
        },
    }
}

fn retain_signature(target: &mut Option<String>, part: &Value) {
    if let Some(signature) = part
        .get("thoughtSignature")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        *target = Some(signature.to_owned());
    }
}
fn valid_signature(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.len() % 4 == 0
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    })
}
fn requires_tool_id(model: &str) -> bool {
    model.starts_with("claude-") || model.starts_with("gpt-oss-")
}
fn normalize_id(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn disabled_thinking(model: &PiModel) -> Value {
    let id = model.id.as_str().to_ascii_lowercase();
    if is_gemini3_pro(&id) {
        json!({"thinkingLevel":"LOW"})
    } else if is_gemini3_flash(&id) || is_gemma4(&id) {
        json!({"thinkingLevel":"MINIMAL"})
    } else {
        json!({"thinkingBudget":0})
    }
}
fn enabled_thinking(
    model: &PiModel,
    level: PiThinkingLevel,
    budgets: Option<&crate::config::PiThinkingBudgets>,
) -> Value {
    let id = model.id.as_str().to_ascii_lowercase();
    if is_gemini3_pro(&id) || is_gemini3_flash(&id) || is_gemma4(&id) {
        let level = if is_gemini3_pro(&id) {
            match level {
                PiThinkingLevel::Minimal | PiThinkingLevel::Low => "LOW",
                PiThinkingLevel::Medium
                | PiThinkingLevel::High
                | PiThinkingLevel::XHigh
                | PiThinkingLevel::Max
                | PiThinkingLevel::Off => "HIGH",
            }
        } else if is_gemma4(&id) {
            match level {
                PiThinkingLevel::Minimal | PiThinkingLevel::Low => "MINIMAL",
                PiThinkingLevel::Medium
                | PiThinkingLevel::High
                | PiThinkingLevel::XHigh
                | PiThinkingLevel::Max
                | PiThinkingLevel::Off => "HIGH",
            }
        } else {
            match level {
                PiThinkingLevel::Minimal => "MINIMAL",
                PiThinkingLevel::Low => "LOW",
                PiThinkingLevel::Medium => "MEDIUM",
                PiThinkingLevel::High
                | PiThinkingLevel::XHigh
                | PiThinkingLevel::Max
                | PiThinkingLevel::Off => "HIGH",
            }
        };
        json!({"includeThoughts":true,"thinkingLevel":level})
    } else {
        let custom = budgets.and_then(|budgets| match level {
            PiThinkingLevel::Minimal => budgets.minimal,
            PiThinkingLevel::Low => budgets.low,
            PiThinkingLevel::Medium => budgets.medium,
            PiThinkingLevel::High
            | PiThinkingLevel::XHigh
            | PiThinkingLevel::Max
            | PiThinkingLevel::Off => budgets.high,
        });
        let budget = custom.unwrap_or_else(|| default_thinking_budget(&id, level));
        json!({"includeThoughts":true,"thinkingBudget":budget})
    }
}

fn default_thinking_budget(id: &str, level: PiThinkingLevel) -> f64 {
    let values = if id.contains("2.5-pro") {
        [128.0, 2_048.0, 8_192.0, 32_768.0]
    } else if id.contains("2.5-flash-lite") {
        [512.0, 2_048.0, 8_192.0, 24_576.0]
    } else if id.contains("2.5-flash") {
        [128.0, 2_048.0, 8_192.0, 24_576.0]
    } else {
        return -1.0;
    };
    match level {
        PiThinkingLevel::Minimal => values[0],
        PiThinkingLevel::Low => values[1],
        PiThinkingLevel::Medium => values[2],
        PiThinkingLevel::High
        | PiThinkingLevel::XHigh
        | PiThinkingLevel::Max
        | PiThinkingLevel::Off => values[3],
    }
}

fn is_gemma4(id: &str) -> bool {
    id.contains("gemma-4") || id.contains("gemma4")
}

fn is_gemini3_pro(id: &str) -> bool {
    is_gemini3_variant(id, "pro")
}

fn is_gemini3_flash(id: &str) -> bool {
    is_gemini3_variant(id, "flash")
        || matches!(id, "gemini-flash-latest" | "gemini-flash-lite-latest")
}

fn is_gemini3_variant(id: &str, variant: &str) -> bool {
    id.match_indices("gemini-3").any(|(offset, prefix)| {
        let rest = &id[offset + prefix.len()..];
        if rest
            .strip_prefix('-')
            .is_some_and(|rest| rest.starts_with(variant))
        {
            return true;
        }
        let Some(rest) = rest.strip_prefix('.') else {
            return false;
        };
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0
            && rest[digits..]
                .strip_prefix('-')
                .is_some_and(|rest| rest.starts_with(variant))
    })
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
fn index_u64(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use crate::{catalog::builtin_catalog, config::PiThinkingBudgets};

    use super::*;

    fn google_model(id: &str) -> PiModel {
        builtin_catalog()
            .provider("google")
            .unwrap()
            .models
            .iter()
            .find(|model| model.id.as_str() == id)
            .unwrap()
            .clone()
    }

    #[test]
    fn thinking_configuration_matches_pi_ai_model_families_and_custom_budgets() {
        let flash = google_model("gemini-2.5-flash");
        assert_eq!(
            enabled_thinking(&flash, PiThinkingLevel::High, None),
            json!({"includeThoughts":true,"thinkingBudget":24576.0})
        );
        assert_eq!(
            enabled_thinking(
                &flash,
                PiThinkingLevel::Low,
                Some(&PiThinkingBudgets {
                    minimal: None,
                    low: Some(12.5),
                    medium: None,
                    high: None,
                })
            ),
            json!({"includeThoughts":true,"thinkingBudget":12.5})
        );

        let pro = google_model("gemini-3.1-pro-preview");
        assert_eq!(disabled_thinking(&pro), json!({"thinkingLevel":"LOW"}));
        assert_eq!(
            enabled_thinking(&pro, PiThinkingLevel::Minimal, None),
            json!({"includeThoughts":true,"thinkingLevel":"LOW"})
        );
    }

    #[test]
    fn custom_vertex_base_api_version_detection_matches_collection_scope() {
        for base in [
            "https://example.test/v1",
            "https://example.test/root/v1beta",
            "https://example.test/root/v1beta2/models",
        ] {
            assert!(base_url_includes_api_version(base), "{base}");
        }
        for base in [
            "https://example.test/version1",
            "https://example.test/root/vbeta",
            "https://example.test/root/models",
        ] {
            assert!(!base_url_includes_api_version(base), "{base}");
        }
    }
}
