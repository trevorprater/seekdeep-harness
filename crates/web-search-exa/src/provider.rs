//! `ExaSearchProvider`: a `WebSearchProvider` backed by the Exa search API (`POST /search`
//! with highlight contents). It maps the first non-blank highlight to `snippet`, maps
//! `publishedDate` to `publishedAt`, drops entries without a snippet, and omits `content`
//! because Exa returns no generated answer.

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;
use seekdeep_web::{
    WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource, web_error,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::types::{ExaError, ExaResult, ExaSearchResponse};

/// Stable id this provider registers under.
pub const EXA_PROVIDER_ID: &str = "exa";

/// Default Exa search endpoint; `/search` is the operation.
pub const EXA_DEFAULT_BASE_URL: &str = "https://api.exa.ai";

/// Default number of highlight sentences requested per result.
pub const EXA_DEFAULT_HIGHLIGHTS_PER_RESULT: f64 = 1.0;

/// Attribution header sent on every request. Bump with the package version.
const USER_AGENT: &str = "deepseek-harness/0.0.1";

/// Retrieval mode sent as Exa's `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchType {
    /// Exa picks between keyword and neural search.
    Auto,
    /// Keyword search.
    Keyword,
    /// Neural (embeddings) search.
    Neural,
}

/// Default retrieval mode: let Exa pick between keyword and neural search.
pub const EXA_DEFAULT_SEARCH_TYPE: SearchType = SearchType::Auto;

/// Resolved provider options (the plugin's `apply` supplies env-var and constant defaults).
#[derive(Clone, Debug)]
pub struct ExaSearchProviderOptions {
    /// Exa API key. Empty/absent makes the provider unavailable.
    pub api_key: String,
    /// Endpoint base; `/search` is appended.
    pub base_url: String,
    /// Retrieval mode sent as Exa's `type`.
    pub search_type: SearchType,
    /// Default result count when a request carries no `maxResults`.
    pub num_results: Option<f64>,
    /// Highlight sentences requested per result.
    pub highlights_per_result: f64,
}

/// Map one Exa result to a normalized source, or `None` when it carries no portable snippet (an
/// entry with no non-blank highlight is dropped).
#[must_use]
pub fn map_exa_result(result: &ExaResult) -> Option<WebSearchSource> {
    let snippet = result
        .highlights
        .iter()
        .flatten()
        .find(|highlight| !highlight.trim().is_empty())?;
    Some(WebSearchSource {
        url: result.url.clone(),
        title: result
            .title
            .as_deref()
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned),
        snippet: Some(snippet.clone()),
        published_at: result
            .published_date
            .as_deref()
            .filter(|date| !date.is_empty())
            .map(ToOwned::to_owned),
    })
}

/// Map an Exa response envelope to a normalized search result; snippet-less entries are dropped.
#[must_use]
pub fn map_exa_response(response: &ExaSearchResponse) -> WebSearchResult {
    let sources: Vec<WebSearchSource> = response
        .results
        .iter()
        .flatten()
        .filter_map(map_exa_result)
        .collect();
    WebSearchResult {
        content: None,
        sources,
        truncated: false,
    }
}

/// The Exa-backed search provider; HTTP redirects fail as `WEB_PROVIDER_ERROR`.
pub struct ExaSearchProvider {
    options: ExaSearchProviderOptions,
    http: reqwest::Client,
}

impl ExaSearchProvider {
    /// Builds a provider over resolved options with a default redirect-rejecting HTTP client.
    ///
    /// # Panics
    ///
    /// Panics if the default redirect-rejecting HTTP client cannot be constructed. This is
    /// unreachable for the fixed builder defaults.
    #[must_use]
    pub fn new(options: ExaSearchProviderOptions) -> Self {
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
        let mut message = format!("Exa API error (HTTP {status})");
        match read_response_body(response, signal).await {
            Ok(bytes) => {
                if let Ok(parsed) = serde_json::from_slice::<ExaError>(&bytes)
                    && let Some(detail) = parsed.error.or(parsed.message).filter(|d| !d.is_empty())
                {
                    message.clear();
                    message.push_str(&detail);
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
                "Exa returned an unprocessable response body",
                "WEB_PROVIDER_ERROR",
            )
            .into());
        };
        match serde_json::from_slice::<ExaSearchResponse>(&bytes) {
            Ok(payload) => Ok(map_exa_response(&payload)),
            Err(error) => {
                if signal.is_some_and(AbortSignal::is_aborted) {
                    return Err(search_aborted(signal.unwrap()));
                }
                Err(web_error(
                    format!("Exa returned an unprocessable response body: {error}"),
                    "WEB_PROVIDER_ERROR",
                )
                .into())
            }
        }
    }
}

#[async_trait]
impl WebSearchProvider for ExaSearchProvider {
    fn id(&self) -> &str {
        EXA_PROVIDER_ID
    }

    fn available(&self) -> bool {
        !self.options.api_key.is_empty()
            && url::Url::parse(&self.options.base_url).is_ok()
            && is_positive_integer(self.options.highlights_per_result)
            && self.options.num_results.is_none_or(is_positive_integer)
    }

    async fn search(
        &self,
        request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult> {
        // A per-request bound wins over the configured default; either may be absent.
        let mut body = json!({
            "query": &request.query,
            "type": self.options.search_type,
            "contents": { "highlights": { "highlightsPerUrl": self.options.highlights_per_result } },
        });
        if let Some(max_results) = request.max_results {
            body["numResults"] = json!(max_results);
        } else if let Some(num_results) = self.options.num_results {
            body["numResults"] = json!(num_results);
        }

        let endpoint = format!("{}/search", self.options.base_url);
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
                    format!("Exa search request failed: {error}"),
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
    web_error("Exa search aborted", "WEB_ABORTED").into()
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
    result
        .map(|bytes| bytes.to_vec())
        .map_err(|_| web_error("Exa search response body read failed", "WEB_PROVIDER_ERROR").into())
}
