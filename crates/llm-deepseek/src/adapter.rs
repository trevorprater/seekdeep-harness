//! `DeepSeek` HTTP transport, catalog, failure mapping, and stream ownership.

use std::{pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{FutureExt as _, Stream, StreamExt as _, future::BoxFuture};
use seekdeep_anonymous_user_id::AnonymousUserId;
use seekdeep_credentials::CredentialRef;
use seekdeep_llm::{
    AdapterStream, CONTEXT_WINDOW_EXCEEDED_CODE, GenerateOptions, LlmAdapter, LlmError,
    LlmModelContext, LlmModelInfo, LlmModelReasoningInfo, LlmProviderInfo, LlmReasoningEffortInfo,
    LlmResolvedModelInfo, ModelId, ModelModality, ProviderId, ProviderRequestId,
    QUOTA_EXCEEDED_CODE, ReasoningEffortId, ResolvedRetryPolicy, StreamChunk, attribution_headers,
    is_context_window_exceeded_error, is_quota_exceeded_error,
};

use crate::{
    serialize::{RequestDefaults, serialize_request},
    sse::{ByteStream, parse_sse},
    translate::translate,
    types::{WireError, WireErrorDetail},
};

/// Default maximum idle interval while a stream read is outstanding.
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: f64 = 300_000.0;
/// Default combined request/response context capacity.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
/// Default per-request output-token cap.
pub const DEFAULT_MAX_TOKENS: u64 = 256_000;

/// One advertised model entry for the direct-fetch adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDeepSeekCatalogModel {
    /// Wire model id.
    pub id: String,
    /// Optional selector label.
    pub name: Option<String>,
    /// Optional selector detail.
    pub description: Option<String>,
    /// Exact model context capacity.
    pub context_window: Option<u64>,
    /// Exact model output cap.
    pub max_tokens: Option<u64>,
}

/// Validated connection facts frozen once per operation.
#[derive(Clone, Debug, PartialEq)]
pub struct DeepSeekConnectionOptions {
    /// Endpoint base; `/chat/completions` is appended.
    pub base_url: String,
    /// Credential reference from the same configuration generation.
    pub api_key_env: CredentialRef,
    /// Thinking defaults.
    pub defaults: RequestDefaults,
    /// Route-wide output cap.
    pub max_tokens: u64,
    /// Fallback context capacity.
    pub default_context_window: u64,
    /// Advisory model catalog.
    pub models: Vec<ResolvedDeepSeekCatalogModel>,
    /// Maximum idle interval while a body read is pending.
    pub stream_idle_timeout_ms: f64,
    /// Provider-owned retry policy.
    pub retry_policy: ResolvedRetryPolicy,
}

/// Current connection snapshot resolver.
pub type ConnectionResolver =
    Arc<dyn Fn() -> Arc<DeepSeekConnectionOptions> + Send + Sync + 'static>;
/// Per-request credential resolver tied to one connection snapshot.
pub type ApiKeyResolver = Arc<
    dyn Fn(Arc<DeepSeekConnectionOptions>) -> BoxFuture<'static, anyhow::Result<String>>
        + Send
        + Sync
        + 'static,
>;
/// Lazy anonymous-user-id resolver.
pub type UserIdResolver = Arc<dyn Fn() -> anyhow::Result<AnonymousUserId> + Send + Sync + 'static>;

/// Constructor dependencies for [`DeepSeekAdapter`].
#[derive(Clone)]
pub struct DeepSeekAdapterOptions {
    /// Current validated connection facts.
    pub options: ConnectionResolver,
    /// Current bearer token for those exact facts.
    pub resolve_api_key: ApiKeyResolver,
    /// Harness-home anonymous identity.
    pub resolve_user_id: UserIdResolver,
    /// HTTP client; injection keeps transport tests and embedding explicit.
    pub http: reqwest::Client,
}

impl std::fmt::Debug for DeepSeekAdapterOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeepSeekAdapterOptions")
            .field("options", &"<resolver>")
            .field("resolve_api_key", &"<resolver>")
            .field("resolve_user_id", &"<resolver>")
            .field("http", &self.http)
            .finish()
    }
}

/// Direct OpenAI-compatible `DeepSeek` adapter.
#[derive(Clone, Debug)]
pub struct DeepSeekAdapter {
    config: DeepSeekAdapterOptions,
}

impl DeepSeekAdapter {
    /// Creates a transport over operation-local resolution hooks.
    #[must_use]
    pub fn new(config: DeepSeekAdapterOptions) -> Self {
        Self { config }
    }

    fn model_info(provider: &str, model: &ResolvedDeepSeekCatalogModel) -> LlmModelInfo {
        LlmModelInfo {
            provider: ProviderId::new(provider),
            id: ModelId::new(model.id.clone()),
            name: model.name.clone().unwrap_or_else(|| model.id.clone()),
            description: model.description.clone(),
            input_modalities: Some(vec![ModelModality("text".to_owned())]),
        }
    }

    async fn request(
        &self,
        options: &GenerateOptions,
        connection: Arc<DeepSeekConnectionOptions>,
        api_key: &str,
        user_id: &AnonymousUserId,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamChunk>> + Send>>> {
        let body = serialize_request(options, connection.defaults)?;
        let payload = serde_json::to_vec(&body)?;
        let endpoint = format!("{}/chat/completions", connection.base_url);
        let mut request = self
            .config
            .http
            .post(&endpoint)
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("x-seekdeep-harness-user-id", user_id.as_str())
            .body(payload);
        for (name, value) in attribution_headers() {
            request = request.header(name, value);
        }
        if let Some(session_id) = &options.session_id {
            request = request.header("x-seekdeep-harness-session-id", session_id.as_str());
        }
        if options.purpose == Some(seekdeep_llm::LlmRequestPurpose::Compaction) {
            request = request.header("x-seekdeep-harness-compact", "1");
        }

        let signal = options.signal.clone();
        let response = if let Some(signal) = &signal {
            tokio::select! {
                biased;
                () = signal.cancelled() => return Err(aborted_error().into()),
                result = request.send() => result,
            }
        } else {
            request.send().await
        }
        .map_err(|error| {
            LlmError::simple(
                format!("DeepSeek API request to {} failed", connection.base_url),
                "TRANSPORT",
            )
            .with_cause(error)
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(http_error(response).await.into());
        }

        let body_stream = response_body_stream(response, signal, connection.clone());
        Ok(translate(parse_sse(body_stream, None)))
    }
}

#[async_trait]
impl LlmAdapter for DeepSeekAdapter {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: ProviderId::new(provider),
            name: "DeepSeek".to_owned(),
        }
    }

    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some((self.config.options)().retry_policy.clone())
    }

    async fn list_models(&self, provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        Ok((self.config.options)()
            .models
            .iter()
            .map(|model| Self::model_info(provider, model))
            .collect())
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let connection = (self.config.options)();
        let configured = connection.models.iter().find(|entry| entry.id == model);
        let info = configured.map_or_else(
            || LlmModelInfo {
                provider: ProviderId::new(provider),
                id: ModelId::new(model),
                name: model.to_owned(),
                description: None,
                input_modalities: Some(vec![ModelModality("text".to_owned())]),
            },
            |configured| Self::model_info(provider, configured),
        );
        let off = ReasoningEffortId::new("off");
        let high = ReasoningEffortId::new("high");
        let max = ReasoningEffortId::new("max");
        let reasoning =
            if connection.defaults.thinking == Some(crate::types::ThinkingMode::Disabled) {
                LlmModelReasoningInfo {
                    efforts: vec![effort(off.clone(), "Off")],
                    default_effort: Some(off),
                }
            } else {
                let default_effort = match connection.defaults.reasoning_effort {
                    Some(crate::serialize::ReasoningEffort::Off) => off.clone(),
                    Some(crate::serialize::ReasoningEffort::Max) => max.clone(),
                    Some(crate::serialize::ReasoningEffort::High) | None => high.clone(),
                };
                LlmModelReasoningInfo {
                    efforts: vec![effort(off, "Off"), effort(high, "High"), effort(max, "Max")],
                    default_effort: Some(default_effort),
                }
            };
        Ok(LlmResolvedModelInfo {
            provider: info.provider,
            id: info.id,
            name: info.name,
            description: info.description,
            input_modalities: info.input_modalities,
            context: Some(LlmModelContext {
                context_window: configured
                    .and_then(|entry| entry.context_window)
                    .unwrap_or(connection.default_context_window),
            }),
            default_max_tokens: Some(
                configured
                    .and_then(|entry| entry.max_tokens)
                    .unwrap_or(connection.max_tokens),
            ),
            reasoning: Some(reasoning),
        })
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let adapter = self.clone();
        AdapterStream::new(async_stream::try_stream! {
            let connection = (adapter.config.options)();
            let api_key = (adapter.config.resolve_api_key)(connection.clone()).await?;
            let user_id = (adapter.config.resolve_user_id)()?;
            let stream = adapter.request(&options, connection.clone(), &api_key, &user_id).await;
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    if options.signal.as_ref().is_some_and(seekdeep_llm::AbortSignal::is_aborted) {
                        Err::<(), anyhow::Error>(aborted_error().into())?;
                    }
                    Err::<(), anyhow::Error>(error)?;
                    unreachable!();
                }
            };
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => yield chunk,
                    Err(error) => {
                        if options.signal.as_ref().is_some_and(seekdeep_llm::AbortSignal::is_aborted) {
                            Err::<(), anyhow::Error>(aborted_error().into())?;
                        }
                        Err::<(), anyhow::Error>(error)?;
                    }
                }
            }
        })
    }
}

fn effort(id: ReasoningEffortId, name: &str) -> LlmReasoningEffortInfo {
    LlmReasoningEffortInfo {
        id,
        name: name.to_owned(),
        description: None,
    }
}

fn response_body_stream(
    mut response: reqwest::Response,
    signal: Option<seekdeep_llm::AbortSignal>,
    connection: Arc<DeepSeekConnectionOptions>,
) -> ByteStream {
    Box::pin(async_stream::try_stream! {
        let timeout = Duration::from_secs_f64(connection.stream_idle_timeout_ms / 1_000.0);
        loop {
            let read = response.chunk().fuse();
            tokio::pin!(read);
            let next = if let Some(signal) = &signal {
                tokio::select! {
                    biased;
                    () = signal.cancelled() => Err(aborted_error()),
                    () = tokio::time::sleep(timeout) => Err(timeout_error(connection.stream_idle_timeout_ms)),
                    result = &mut read => result.map_err(|error| stream_transport_error(&connection.base_url, error)),
                }
            } else {
                tokio::select! {
                    () = tokio::time::sleep(timeout) => Err(timeout_error(connection.stream_idle_timeout_ms)),
                    result = &mut read => result.map_err(|error| stream_transport_error(&connection.base_url, error)),
                }
            }?;
            let Some(bytes) = next else {
                return;
            };
            yield bytes;
        }
    })
}

fn aborted_error() -> LlmError {
    LlmError::simple("DeepSeek request aborted by caller", "ABORTED")
}

fn timeout_error(timeout_ms: f64) -> LlmError {
    LlmError::simple(
        format!("DeepSeek stream idle timeout after {timeout_ms}ms"),
        "TIMEOUT",
    )
}

fn stream_transport_error(base_url: &str, cause: reqwest::Error) -> LlmError {
    LlmError::simple(
        format!("DeepSeek API stream from {base_url} failed"),
        "TRANSPORT",
    )
    .with_cause(cause)
}

async fn http_error(response: reqwest::Response) -> LlmError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(provider_retry_after_ms);
    let request_id = response
        .headers()
        .get("x-request-id")
        .or_else(|| response.headers().get("x-deepseek-request-id"))
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(ProviderRequestId::new);
    let fallback = format!("DeepSeek API error (HTTP {status})");
    let parsed = response
        .bytes()
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WireError>(&bytes).ok());
    let detail = parsed.as_ref().and_then(|body| body.error.as_ref());
    let message = detail
        .and_then(|detail| detail.message.clone())
        .unwrap_or(fallback);
    LlmError::new(
        message,
        http_error_code(status, detail),
        Some(status),
        retry_after,
        request_id,
    )
    .expect("HTTP status and normalized optional facts are valid")
}

fn provider_retry_after_ms(value: &str) -> Option<f64> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value.parse::<f64>().ok()?;
        let delay = seconds * 1_000.0;
        return (delay.is_finite() && delay > 0.0).then_some(delay);
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now = chrono::Utc::now();
    let delay = (at.with_timezone(&chrono::Utc) - now)
        .num_milliseconds()
        .to_string()
        .parse::<f64>()
        .ok()?;
    (delay.is_finite() && delay > 0.0).then_some(delay)
}

/// Maps one non-success HTTP response to a stable provider-neutral code.
#[must_use]
pub fn http_error_code(status: u16, error: Option<&WireErrorDetail>) -> String {
    if status == 401 || status == 403 {
        return "AUTH".to_owned();
    }
    let detail = [
        error.and_then(|value| value.code.as_deref()),
        error.and_then(|value| value.kind.as_deref()),
        error.and_then(|value| value.message.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    if is_quota_exceeded_error(&detail) {
        return QUOTA_EXCEEDED_CODE.to_owned();
    }
    if status == 429 {
        return "RATE_LIMIT".to_owned();
    }
    if status == 400 {
        return if is_context_window_exceeded_error(&detail) {
            CONTEXT_WINDOW_EXCEEDED_CODE.to_owned()
        } else {
            "INVALID_REQUEST".to_owned()
        };
    }
    if status >= 500 {
        return "SERVER".to_owned();
    }
    format!("HTTP_{status}")
}
