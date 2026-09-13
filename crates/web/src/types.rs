//! Vocabulary for the web capability seam (ctx.web).

use async_trait::async_trait;
use seekdeep_llm::{AbortSignal, HarnessError};
use serde::{Deserialize, Serialize};

/// The JavaScript error-class name of the web error subclass.
pub const WEB_ERROR_NAME: &str = "WebError";

/// Creates a typed web error with a machine-routable open-string code.
#[must_use]
pub fn web_error(message: impl Into<String>, code: impl Into<String>) -> HarnessError {
    HarnessError::named(WEB_ERROR_NAME, message, code)
}

/// What one search-capable backend is asked to search.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchRequest {
    /// The model-facing query.
    pub query: String,
    /// Upper bound on returned sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u64>,
}

/// One citeable source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchSource {
    /// Source URL.
    pub url: String,
    /// Optional title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional snippet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Publication/crawl timestamp as an ISO-8601 string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

/// Normalized search outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    /// Optional provider-generated answer text or summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Citeable sources, already truncated to the request's maxResults.
    pub sources: Vec<WebSearchSource>,
    /// True when the seam dropped sources to honor maxResults.
    pub truncated: bool,
}

/// What one fetch-capable backend is asked to retrieve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebFetchRequest {
    /// The URL to retrieve.
    pub url: String,
}

/// The decoded body of a fetched resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WebFetchBody {
    /// HTML document text.
    Html {
        /// Decoded body content.
        content: String,
    },
    /// Plain text body.
    Text {
        /// Decoded body content.
        content: String,
    },
}

/// Normalized fetch outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebFetchResult {
    /// Final URL after allowed redirects.
    pub url: String,
    /// HTTP status code.
    pub status_code: u16,
    /// Decoded body, classified by content kind.
    pub body: WebFetchBody,
    /// True when the provider capped the decoded body.
    pub truncated: bool,
}

/// A search-capable backend.
#[async_trait]
pub trait WebSearchProvider: Send + Sync + 'static {
    /// Stable provider id.
    fn id(&self) -> &str;
    /// Cheap local usability check; must not make network calls.
    fn available(&self) -> bool;
    /// Runs one search, honoring cancellation.
    async fn search(
        &self,
        request: &WebSearchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebSearchResult>;
}

/// A fetch-capable backend.
#[async_trait]
pub trait WebFetchProvider: Send + Sync + 'static {
    /// Stable provider id.
    fn id(&self) -> &str;
    /// Cheap local usability check; must not make network calls.
    fn available(&self) -> bool;
    /// Retrieves one URL, honoring cancellation.
    async fn fetch(
        &self,
        request: &WebFetchRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<WebFetchResult>;
}
