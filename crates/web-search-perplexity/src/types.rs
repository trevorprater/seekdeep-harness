//! Wire types for the Perplexity search API (`POST https://api.perplexity.ai/chat/completions`,
//! an OpenAI-compatible chat shape). Results prefer structured `search_results` and fall back to
//! URL-only `citations`.

use serde::{Deserialize, Deserializer};

/// One structured search result (the preferred citation shape).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PerplexitySearchResult {
    /// Result URL.
    pub url: String,
    /// Optional title.
    pub title: Option<String>,
    /// Optional snippet.
    pub snippet: Option<String>,
    /// Optional publication date.
    pub date: Option<String>,
}

/// One completion choice.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PerplexityChoice {
    /// Choice message.
    pub message: Option<PerplexityMessage>,
}

/// The generated assistant message.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PerplexityMessage {
    /// Generated answer text.
    pub content: Option<String>,
}

/// Perplexity's response envelope.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PerplexityResponse {
    /// Completion choices; the first carries the generated answer.
    pub choices: Option<Vec<PerplexityChoice>>,
    /// Structured citation data (preferred). Rejects `null` to match the source's
    /// `search_results.map` `TypeError` on a null array.
    #[serde(default, deserialize_with = "deserialize_search_results")]
    pub search_results: Option<Vec<PerplexitySearchResult>>,
    /// URL-only citation fallback.
    pub citations: Option<Vec<String>>,
}

/// Perplexity's error detail: either a bare string or an object carrying a message.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum PerplexityErrorDetail {
    /// A bare error string.
    String(String),
    /// An error object with an optional message.
    Object {
        /// Error message.
        message: Option<String>,
    },
}

/// Perplexity's error response envelope (best-effort; fields vary).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PerplexityError {
    /// Primary error detail (string or object).
    pub error: Option<PerplexityErrorDetail>,
    /// Fallback error message.
    pub message: Option<String>,
}

/// Deserializes `search_results` while rejecting an explicit `null` (which the source treats as
/// a `TypeError`, surfaced as `WEB_PROVIDER_ERROR`) rather than silently collapsing to `None`.
fn deserialize_search_results<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<PerplexitySearchResult>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Err(serde::de::Error::custom("search_results must be an array")),
        other => serde_json::from_value(other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}
