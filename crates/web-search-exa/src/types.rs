//! Wire types for the Exa search API (`POST https://api.exa.ai/search`). Exa returns a flat
//! `results[]`; each entry carries a URL, optional title, optional `publishedDate`, and (when
//! highlights are requested) a `highlights[]` array of salient sentences.

use serde::Deserialize;

/// One entry of Exa's flat `results[]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExaResult {
    /// Result URL.
    pub url: String,
    /// Optional title.
    pub title: Option<String>,
    /// Optional publication date.
    pub published_date: Option<String>,
    /// Optional highlight sentences.
    pub highlights: Option<Vec<String>>,
}

/// Exa's search response envelope.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ExaSearchResponse {
    /// Flat result entries.
    pub results: Option<Vec<ExaResult>>,
}

/// Exa's error response envelope (best-effort; fields vary by failure).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ExaError {
    /// Primary error detail.
    pub error: Option<String>,
    /// Fallback error detail.
    pub message: Option<String>,
}
