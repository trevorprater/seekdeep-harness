//! Provider-private wire types for DeepSeek's Anthropic-compatible Messages API.
//!
//! Citeable result items and citation excerpts arrive in separate blocks; the provider joins
//! them by URL. These types do not create a dependency on `ctx.llm`.

use serde::{Deserialize, Serialize};

/// A `web_search_result` item inside a `web_search_tool_result` block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchResultItem {
    /// The result-item discriminator (only `web_search_result` is consumed).
    #[serde(rename = "type")]
    pub item_type: String,
    /// Citeable source URL.
    pub url: String,
    /// Optional title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Provider-supplied page age/recency string (mapped to `publishedAt`).
    #[serde(default, rename = "page_age", skip_serializing_if = "Option::is_none")]
    pub page_age: Option<String>,
}

/// One citation location inside a `text` block (the snippet source).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationLocation {
    /// Optional location discriminator.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub location_type: Option<String>,
    /// The cited URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The excerpt attributed to that URL.
    #[serde(
        default,
        rename = "cited_text",
        skip_serializing_if = "Option::is_none"
    )]
    pub cited_text: Option<String>,
}

/// Any content block; only `web_search_tool_result` and `text` are consumed.
///
/// Unknown block types round-trip through the fallback fields so a response the provider does
/// not understand still deserializes losslessly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    /// The block discriminator.
    #[serde(rename = "type")]
    pub block_type: String,
    /// `text` block prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `text` block citations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<CitationLocation>>,
    /// `web_search_tool_result` block results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<WebSearchResultItem>>,
}

impl ContentBlock {
    /// True when this block is a `text` block.
    #[must_use]
    pub fn is_text(&self) -> bool {
        self.block_type == "text"
    }

    /// True when this block is a `web_search_tool_result` block.
    #[must_use]
    pub fn is_web_search_tool_result(&self) -> bool {
        self.block_type == "web_search_tool_result"
    }
}

/// `DeepSeek`'s Anthropic Messages response envelope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicResponse {
    /// The content blocks (absent when the provider returned no blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
}

/// `DeepSeek`'s error response envelope (best-effort; fields vary).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicError {
    /// The provider error, either a message string or an object carrying one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AnthropicErrorDetail>,
    /// A top-level message fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The two shapes `error` can take on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicErrorDetail {
    /// A plain string message.
    String(String),
    /// An object carrying an optional message.
    Object {
        /// The provider message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl AnthropicError {
    /// The most specific provider message, if any, per the source precedence:
    /// string error, then object error message, then top-level message.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match &self.error {
            Some(AnthropicErrorDetail::String(message)) => non_empty(message),
            Some(AnthropicErrorDetail::Object { message }) => {
                message.as_deref().and_then(non_empty)
            }
            None => self.message.as_deref().and_then(non_empty),
        }
    }
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}
