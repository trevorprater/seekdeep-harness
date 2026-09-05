//! DeepSeek search through an Anthropic-compatible Messages model call with the native
//! `web_search_20250305` server tool.
//!
//! Each search costs a model turn, but returns structured result blocks; absence of those blocks
//! is an error rather than a prose-scraping fallback. The wire format and native HTTP client are
//! provider-private and do not use `ctx.llm`.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_credentials::CredentialRef;
use seekdeep_llm::AbortSignal;
use seekdeep_web::{
    WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource, web_error,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::types::{AnthropicError, AnthropicResponse, ContentBlock};

/// Stable id this provider registers under.
pub const DEEPSEEK_PROVIDER_ID: &str = "deepseek-official";

/// Default endpoint: `DeepSeek`'s Anthropic-compatible API, `/v1` included (`/messages` is
/// appended). This is NOT the chat-completions base (`https://api.deepseek.com`) that
/// `llm-deepseek` uses, so this provider does NOT reuse `$DEEPSEEK_BASE_URL` — only the API
/// key is shared.
pub const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com/anthropic/v1";

/// Default Anthropic-format model name (aligned with the repo's `DeepSeek` model vocabulary).
pub const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// Default `anthropic-version` header value.
pub const DEEPSEEK_DEFAULT_API_VERSION: &str = "2023-06-01";

/// Default upper bound on generated tokens for the Messages request.
pub const DEEPSEEK_DEFAULT_MAX_TOKENS: u64 = 4096;

/// Default maximum `web_search` server-tool uses per request.
pub const DEEPSEEK_DEFAULT_MAX_USES: u64 = 5;

/// Attribution header sent on every request. Bump with the package version.
const USER_AGENT: &str = "deepseek-harness/0.0.1";

/// Resolves the current `DeepSeek` API key for one search operation.
pub type ResolveApiKey =
    Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<Option<String>>> + Send + Sync + 'static>;

/// Records the exact secret-free request immediately before dispatch.
pub type RecordRequest =
    Arc<dyn Fn(&DeepSeekSearchLlmRequest) -> anyhow::Result<()> + Send + Sync + 'static>;

/// Exact secret-free `DeepSeek` Messages request recorded immediately before one auxiliary search
/// dispatch.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeepSeekSearchLlmRequest {
    /// Fully resolved Messages endpoint.
    pub endpoint: String,
    /// `anthropic-version` header value.
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    /// Exact JSON body sent to the provider.
    pub body: Value,
}

/// Resolved provider options (the plugin's `apply` supplies credential and constant defaults).
#[derive(Clone)]
pub struct DeepSeekSearchProviderOptions {
    /// Literal `DeepSeek` API key; when present it wins over `resolve_api_key`.
    pub api_key: Option<String>,
    /// Resolve the current `DeepSeek` API key for one search operation.
    pub resolve_api_key: Option<ResolveApiKey>,
    /// Credential reference named by missing-credential diagnostics.
    pub api_key_env: Option<CredentialRef>,
    /// Endpoint base; `/messages` is appended.
    pub base_url: String,
    /// Anthropic-format model name.
    pub model: String,
    /// `anthropic-version` header value.
    pub api_version: String,
    /// Upper bound on generated tokens for the Messages request.
    pub max_tokens: u64,
    /// Maximum `web_search` server-tool uses per request.
    pub max_uses: u64,
    /// Record the exact secret-free request immediately before dispatch. A failure prevents
    /// dispatch so model-visible auxiliary input cannot escape logging.
    pub record_request: Option<RecordRequest>,
}

/// Build a `url → cited_text` map from every `text` block's `citations[]`. This is the
/// snippet source: Anthropic `web_search_result` items carry `url`/`title`/`page_age` but
/// typically NO inline snippet — the excerpt lives in a separate `text` block's citation, keyed
/// by `url` (first occurrence wins).
#[must_use]
pub fn citation_snippets(blocks: &[ContentBlock]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for block in blocks {
        if !block.is_text() {
            continue;
        }
        for cite in block.citations.iter().flatten() {
            let Some(url) = cite.url.as_deref().filter(|url| !url.is_empty()) else {
                continue;
            };
            let Some(text) = cite.cited_text.as_deref().filter(|text| !text.is_empty()) else {
                continue;
            };
            if !map.contains_key(url) {
                map.insert(url.to_owned(), text.to_owned());
            }
        }
    }
    map
}

/// Map a `DeepSeek` Anthropic Messages response to a normalized search result. Walks
/// `web_search_tool_result` blocks for citeable `web_search_result` items, joins each to its
/// citation excerpt as `snippet`, and dedupes by `url` (a `max_uses > 1` request can surface
/// the same URL across searches). The web service owns the final `maxResults` truncation, so
/// `truncated` is always `false` here.
///
/// # Errors
///
/// Returns a `WEB_PROVIDER_ERROR` when native search produced no result block.
pub fn map_anthropic_response(response: &AnthropicResponse) -> anyhow::Result<WebSearchResult> {
    let blocks = response.content.as_deref().unwrap_or_default();
    let result_blocks: Vec<&ContentBlock> = blocks
        .iter()
        .filter(|block| block.is_web_search_tool_result())
        .collect();
    if result_blocks.is_empty() {
        anyhow::bail!(web_error(
            "DeepSeek returned no web_search_tool_result blocks; the request may not have triggered native web search",
            "WEB_PROVIDER_ERROR",
        ));
    }

    let snippets = citation_snippets(blocks);
    let mut seen = std::collections::HashSet::new();
    let mut sources = Vec::new();
    for block in result_blocks {
        for item in block.content.iter().flatten() {
            if item.item_type != "web_search_result"
                || item.url.is_empty()
                || !seen.insert(item.url.clone())
            {
                continue;
            }
            let snippet = snippets.get(&item.url);
            sources.push(WebSearchSource {
                url: item.url.clone(),
                title: item
                    .title
                    .as_deref()
                    .filter(|title| !title.is_empty())
                    .map(ToOwned::to_owned),
                snippet: snippet
                    .filter(|snippet| !snippet.is_empty())
                    .map(|snippet| (*snippet).clone()),
                published_at: item
                    .page_age
                    .as_deref()
                    .filter(|page_age| !page_age.is_empty())
                    .map(ToOwned::to_owned),
            });
        }
    }
    Ok(WebSearchResult {
        content: None,
        sources,
        truncated: false,
    })
}

/// The DeepSeek-backed search provider; HTTP redirects fail as `WEB_PROVIDER_ERROR`.
pub struct DeepSeekSearchProvider {
    resolve_options: Arc<dyn Fn() -> DeepSeekSearchProviderOptions + Send + Sync>,
    http: reqwest::Client,
}

impl DeepSeekSearchProvider {
    /// Builds a provider over the next-operation options thunk with a default redirect-rejecting
    /// HTTP client.
    ///
    /// # Panics
    ///
    /// Panics if the default redirect-rejecting HTTP client cannot be constructed. This is
    /// unreachable for the fixed builder defaults.
    #[must_use]
    pub fn new(
        resolve_options: impl Fn() -> DeepSeekSearchProviderOptions + Send + Sync + 'static,
    ) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("default HTTP client construction cannot fail");
        Self {
            resolve_options: Arc::new(resolve_options),
            http,
        }
    }

    /// Resolves one operation's credential without retaining it on the provider.
    async fn api_key(
        &self,
        options: &DeepSeekSearchProviderOptions,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        throw_if_search_aborted(signal)?;
        if let Some(key) = options.api_key.as_deref().filter(|key| !key.is_empty()) {
            return Ok(key.to_owned());
        }
        let resolved = match options.resolve_api_key.as_ref() {
            Some(resolve) => {
                let operation = resolve();
                match abortable(operation, signal).await {
                    Ok(value) => value,
                    Err(error) => {
                        if signal.is_some_and(AbortSignal::is_aborted) {
                            return Err(search_aborted(signal.unwrap()));
                        }
                        return Err(web_error(
                            format!("DeepSeek search credential resolution failed: {error}"),
                            "WEB_PROVIDER_ERROR",
                        )
                        .into());
                    }
                }
            }
            None => None,
        };
        if let Some(key) = resolved.filter(|key| !key.is_empty()) {
            return Ok(key);
        }
        let reference = options
            .api_key_env
            .as_ref()
            .map_or("DEEPSEEK_API_KEY", CredentialRef::as_str);
        Err(web_error(
            format!(
                "DeepSeek search has no API key for \"{reference}\"; store it through the credentials service (the web Models page writes it), export it in the launching environment, or set a literal \"apiKey\" in the web-search-deepseek config"
            ),
            "WEB_PROVIDER_CREDENTIAL_MISSING",
        )
        .into())
    }

    async fn dispatch(
        &self,
        endpoint: &str,
        api_version: &str,
        api_key: &str,
        body: &Value,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<reqwest::Response> {
        let payload = serde_json::to_vec(body).map_err(anyhow::Error::from)?;
        let request = self
            .http
            .post(endpoint)
            .header("x-api-key", api_key)
            .header("authorization", format!("Bearer {api_key}"))
            .header("anthropic-version", api_version)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("user-agent", USER_AGENT)
            .body(payload);
        let response = if let Some(signal) = signal {
            tokio::select! {
                biased;
                () = signal.cancelled() => return Err(search_aborted(signal)),
                result = request.send() => result,
            }
        } else {
            request.send().await
        };
        response.map_err(|error| {
            if signal.is_some_and(AbortSignal::is_aborted) {
                search_aborted(signal.unwrap())
            } else {
                web_error(
                    format!("DeepSeek search request failed: {error}"),
                    "WEB_PROVIDER_ERROR",
                )
                .into()
            }
        })
    }

    async fn http_error(
        &self,
        response: reqwest::Response,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Error {
        let status = response.status().as_u16();
        let mut message = format!("DeepSeek API error (HTTP {status})");
        match read_response_body(response, signal).await {
            Ok(bytes) => {
                if let Ok(parsed) = serde_json::from_slice::<AnthropicError>(&bytes)
                    && let Some(detail) = parsed.detail()
                {
                    message.clear();
                    message.push_str(detail);
                }
            }
            Err(_) => {
                if signal.is_some_and(AbortSignal::is_aborted) {
                    return search_aborted(signal.unwrap());
                }
            }
        }
        web_error(message, "WEB_PROVIDER_ERROR").into()
    }

    async fn parse_success(
        &self,
        response: reqwest::Response,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        let Ok(bytes) = read_response_body(response, signal).await else {
            if signal.is_some_and(AbortSignal::is_aborted) {
                return Err(search_aborted(signal.unwrap()));
            }
            return Err(web_error(
                "DeepSeek returned an unprocessable response body",
                "WEB_PROVIDER_ERROR",
            )
            .into());
        };
        match serde_json::from_slice::<AnthropicResponse>(&bytes) {
            Ok(payload) => map_anthropic_response(&payload),
            Err(error) => {
                if signal.is_some_and(AbortSignal::is_aborted) {
                    return Err(search_aborted(signal.unwrap()));
                }
                Err(web_error(
                    format!("DeepSeek returned an unprocessable response body: {error}"),
                    "WEB_PROVIDER_ERROR",
                )
                .into())
            }
        }
    }
}

#[async_trait]
impl WebSearchProvider for DeepSeekSearchProvider {
    fn id(&self) -> &str {
        DEEPSEEK_PROVIDER_ID
    }

    fn available(&self) -> bool {
        let options = (self.resolve_options)();
        let has_key = options
            .api_key
            .as_deref()
            .is_some_and(|key| !key.is_empty())
            || options.resolve_api_key.is_some();
        has_key
            && url::Url::parse(&options.base_url).is_ok()
            && options.max_tokens > 0
            && options.max_uses > 0
    }

    async fn search(
        &self,
        request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        // One snapshot for the whole operation: credential resolution awaits, and a settings
        // write landing inside that await must not send the key resolved from the old section to
        // the endpoint named by the new one.
        let options = (self.resolve_options)();
        let api_key = self.api_key(&options, signal.as_ref()).await?;
        throw_if_search_aborted(signal.as_ref())?;
        let endpoint = format!("{}/messages", options.base_url);
        let body = build_body(&options, request);
        if let Some(record) = &options.record_request {
            record(&DeepSeekSearchLlmRequest {
                endpoint: endpoint.clone(),
                api_version: options.api_version.clone(),
                body: body.clone(),
            })?;
        }
        throw_if_search_aborted(signal.as_ref())?;

        let response = self
            .dispatch(
                &endpoint,
                &options.api_version,
                &api_key,
                &body,
                signal.as_ref(),
            )
            .await?;
        if !response.status().is_success() {
            return Err(self.http_error(response, signal.as_ref()).await);
        }
        self.parse_success(response, signal.as_ref()).await
    }
}

/// Builds the exact secret-free Messages request body for one search.
fn build_body(options: &DeepSeekSearchProviderOptions, request: &WebSearchRequest) -> Value {
    json!({
        "model": &options.model,
        "max_tokens": options.max_tokens,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": format!("Perform a web search for the query: {}", request.query),
            }],
        }],
        "tools": [{
            "type": "web_search_20250305",
            "name": "web_search",
            "max_uses": options.max_uses,
        }],
    })
}

/// Race a same-process asynchronous preflight against caller cancellation.
async fn abortable(
    operation: BoxFuture<'static, anyhow::Result<Option<String>>>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Option<String>> {
    let Some(signal) = signal else {
        return operation.await;
    };
    if signal.is_aborted() {
        return Err(search_aborted(signal));
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(search_aborted(signal)),
        result = operation => result,
    }
}

/// Throw the provider's stable cancellation error when the caller already aborted.
fn throw_if_search_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if let Some(signal) = signal
        && signal.is_aborted()
    {
        return Err(search_aborted(signal));
    }
    Ok(())
}

/// Build the provider's stable cancellation error.
fn search_aborted(_signal: &AbortSignal) -> anyhow::Error {
    web_error("DeepSeek search aborted", "WEB_ABORTED").into()
}

/// Reads a response body, racing against caller cancellation so an abort during body download
/// surfaces as `WEB_ABORTED` rather than a generic transport error.
async fn read_response_body(
    response: reqwest::Response,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<u8>> {
    let read = response.bytes();
    let result = if let Some(signal) = signal {
        tokio::select! {
            biased;
            () = signal.cancelled() => return Err(search_aborted(signal)),
            result = read => result,
        }
    } else {
        read.await
    };
    result.map(|bytes| bytes.to_vec()).map_err(|_| {
        web_error(
            "DeepSeek search response body read failed",
            "WEB_PROVIDER_ERROR",
        )
        .into()
    })
}
