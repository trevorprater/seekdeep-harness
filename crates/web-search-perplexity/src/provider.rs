//! `PerplexitySearchProvider`: a `WebSearchProvider` over Perplexity's OpenAI-compatible
//! chat-completions endpoint. The generated answer becomes `content`; sources prefer structured
//! `search_results[]` and fall back to URL-only `citations[]`.

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;
use seekdeep_web::{
    WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource, web_error,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::types::{
    PerplexityError, PerplexityErrorDetail, PerplexityResponse, PerplexitySearchResult,
};

/// Stable id this provider registers under.
pub const PERPLEXITY_PROVIDER_ID: &str = "perplexity";

/// Default Perplexity endpoint; `/chat/completions` is the operation.
pub const PERPLEXITY_DEFAULT_BASE_URL: &str = "https://api.perplexity.ai";

/// Default search model.
pub const PERPLEXITY_DEFAULT_MODEL: &str = "sonar";

/// Default upper bound on generated answer tokens.
pub const PERPLEXITY_DEFAULT_MAX_TOKENS: f64 = 1024.0;

/// Attribution header sent on every request. Bump with the package version.
const USER_AGENT: &str = "deepseek-harness/0.0.1";

/// Recency filter values Perplexity accepts for `search_recency_filter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PerplexityRecency {
    /// One day.
    Day,
    /// One week.
    Week,
    /// One month.
    Month,
    /// One year.
    Year,
}

/// Resolved provider options (the plugin's `apply` supplies env-var and constant defaults).
#[derive(Clone, Debug)]
pub struct PerplexitySearchProviderOptions {
    /// Perplexity API key. Empty/absent makes the provider unavailable.
    pub api_key: String,
    /// Endpoint base; `/chat/completions` is appended.
    pub base_url: String,
    /// Search model name.
    pub model: String,
    /// Upper bound on generated answer tokens.
    pub max_tokens: f64,
    /// Optional recency window sent as `search_recency_filter`.
    pub search_recency: Option<PerplexityRecency>,
}

/// Map one structured Perplexity search result to a normalized source; blank fields are omitted.
#[must_use]
pub fn map_perplexity_result(result: &PerplexitySearchResult) -> WebSearchSource {
    WebSearchSource {
        url: result.url.clone(),
        title: result
            .title
            .as_deref()
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned),
        snippet: result
            .snippet
            .as_deref()
            .filter(|snippet| !snippet.is_empty())
            .map(ToOwned::to_owned),
        published_at: result
            .date
            .as_deref()
            .filter(|date| !date.is_empty())
            .map(ToOwned::to_owned),
    }
}

/// Map a Perplexity response envelope to a normalized search result. Prefers structured
/// `search_results[]`; falls back to URL-only `citations[]` only when `search_results` is
/// absent.
#[must_use]
pub fn map_perplexity_response(response: &PerplexityResponse) -> WebSearchResult {
    let content = response
        .choices
        .iter()
        .flatten()
        .next()
        .and_then(|choice| choice.message.as_ref())
        .and_then(|message| message.content.as_deref())
        .filter(|content| !content.is_empty())
        .map(ToOwned::to_owned);
    let sources: Vec<WebSearchSource> = match &response.search_results {
        Some(results) => results.iter().map(map_perplexity_result).collect(),
        None => response
            .citations
            .iter()
            .flatten()
            .map(|url| WebSearchSource {
                url: url.clone(),
                title: None,
                snippet: None,
                published_at: None,
            })
            .collect(),
    };
    WebSearchResult {
        content,
        sources,
        truncated: false,
    }
}

/// The Perplexity-backed search provider; HTTP redirects fail as `WEB_PROVIDER_ERROR`.
pub struct PerplexitySearchProvider {
    options: PerplexitySearchProviderOptions,
    http: reqwest::Client,
}

impl PerplexitySearchProvider {
    /// Builds a provider over resolved options with a default redirect-rejecting HTTP client.
    ///
    /// # Panics
    ///
    /// Panics if the default redirect-rejecting HTTP client cannot be constructed. This is
    /// unreachable for the fixed builder defaults.
    #[must_use]
    pub fn new(options: PerplexitySearchProviderOptions) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("default HTTP client construction cannot fail");
        Self { options, http }
    }

    async fn http_error(
        &self,
        response: reqwest::Response,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Error {
        let status = response.status().as_u16();
        let mut message = format!("Perplexity API error (HTTP {status})");
        match read_response_body(response, signal).await {
            Ok(bytes) => {
                if let Ok(parsed) = serde_json::from_slice::<PerplexityError>(&bytes) {
                    let detail = match parsed.error {
                        Some(PerplexityErrorDetail::String(detail)) => Some(detail),
                        Some(PerplexityErrorDetail::Object { message }) => {
                            message.or(parsed.message)
                        }
                        None => parsed.message,
                    };
                    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
                        message.clear();
                        message.push_str(&detail);
                    }
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
                "Perplexity returned an unprocessable response body",
                "WEB_PROVIDER_ERROR",
            )
            .into());
        };
        match serde_json::from_slice::<PerplexityResponse>(&bytes) {
            Ok(payload) => Ok(map_perplexity_response(&payload)),
            Err(error) => {
                if signal.is_some_and(AbortSignal::is_aborted) {
                    return Err(search_aborted(signal.unwrap()));
                }
                Err(web_error(
                    format!("Perplexity returned an unprocessable response body: {error}"),
                    "WEB_PROVIDER_ERROR",
                )
                .into())
            }
        }
    }
}

#[async_trait]
impl WebSearchProvider for PerplexitySearchProvider {
    fn id(&self) -> &str {
        PERPLEXITY_PROVIDER_ID
    }

    fn available(&self) -> bool {
        !self.options.api_key.is_empty()
            && url::Url::parse(&self.options.base_url).is_ok()
            && is_positive_integer(self.options.max_tokens)
    }

    async fn search(
        &self,
        request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        let mut body = json!({
            "model": &self.options.model,
            "max_tokens": self.options.max_tokens,
            "messages": [{ "role": "user", "content": &request.query }],
        });
        if let Some(recency) = self.options.search_recency {
            body["search_recency_filter"] = json!(recency);
        }

        let endpoint = format!("{}/chat/completions", self.options.base_url);
        let payload = serde_json::to_vec(&body).map_err(anyhow::Error::from)?;
        let send = self
            .http
            .post(&endpoint)
            .header("authorization", format!("Bearer {}", self.options.api_key))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("user-agent", USER_AGENT)
            .body(payload)
            .send();

        let response = match signal.as_ref() {
            Some(signal) => tokio::select! {
                biased;
                () = signal.cancelled() => return Err(search_aborted(signal)),
                result = send => result,
            },
            None => send.await,
        };
        let response = response.map_err(|error| {
            if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                search_aborted(signal.as_ref().unwrap())
            } else {
                web_error(
                    format!("Perplexity search request failed: {error}"),
                    "WEB_PROVIDER_ERROR",
                )
                .into()
            }
        })?;

        if !response.status().is_success() {
            return Err(self.http_error(response, signal.as_ref()).await);
        }
        self.parse_success(response, signal.as_ref()).await
    }
}

/// True for a positive whole number.
fn is_positive_integer(value: f64) -> bool {
    value.fract() == 0.0 && value > 0.0
}

/// Build the provider's stable cancellation error.
fn search_aborted(_signal: &AbortSignal) -> anyhow::Error {
    web_error("Perplexity search aborted", "WEB_ABORTED").into()
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
            "Perplexity search response body read failed",
            "WEB_PROVIDER_ERROR",
        )
        .into()
    })
}
