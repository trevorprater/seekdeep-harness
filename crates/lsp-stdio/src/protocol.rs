//! LSP wire subset used by the generic stdio host.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Zero-based UTF-16 wire position.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WirePosition {
    /// Zero-based line.
    pub line: f64,
    /// Zero-based UTF-16 character offset.
    pub character: f64,
}

/// LSP wire range.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireRange {
    /// Inclusive start.
    pub start: WirePosition,
    /// Exclusive end.
    pub end: WirePosition,
}

/// LSP `Location`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireLocation {
    /// Target document URI.
    pub uri: String,
    /// Target range.
    pub range: WireRange,
}

/// LSP `LocationLink`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireLocationLink {
    /// Target document URI.
    pub target_uri: String,
    /// Selection range to focus.
    pub target_selection_range: WireRange,
    /// Optional broader target range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_range: Option<WireRange>,
}

/// Markup encoding used by a hover body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireMarkupKind {
    /// Markdown.
    Markdown,
    /// Plaintext.
    Plaintext,
}

/// LSP `MarkupContent`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireMarkupContent {
    /// Markup kind.
    pub kind: WireMarkupKind,
    /// Markup source.
    pub value: String,
}

/// Object form of a `MarkedString`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireMarkedStringObject {
    /// Code language.
    pub language: String,
    /// Code text.
    pub value: String,
}

/// One `MarkedString` form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireMarkedString {
    /// Bare string.
    String(String),
    /// Language-tagged object.
    Object(WireMarkedStringObject),
}

/// Hover content's three protocol encodings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireHoverContents {
    /// Markup content.
    Markup(WireMarkupContent),
    /// One marked string.
    Marked(WireMarkedString),
    /// A marked-string sequence.
    MarkedList(Vec<WireMarkedString>),
}

/// LSP `Hover`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireHover {
    /// Protocol hover content.
    pub contents: WireHoverContents,
    /// Optional hover range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<WireRange>,
}

/// Legacy `textDocumentSync` enum values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireTextDocumentSyncKind {
    /// No synchronization.
    None,
    /// Full document synchronization.
    Full,
    /// Incremental synchronization.
    Incremental,
}

impl Serialize for WireTextDocumentSyncKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(match self {
            Self::None => 0,
            Self::Full => 1,
            Self::Incremental => 2,
        })
    }
}

impl<'de> Deserialize<'de> for WireTextDocumentSyncKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match u8::deserialize(deserializer)? {
            0 => Ok(Self::None),
            1 => Ok(Self::Full),
            2 => Ok(Self::Incremental),
            value => Err(serde::de::Error::custom(format!(
                "invalid textDocumentSync kind {value}"
            ))),
        }
    }
}

/// Options form of `textDocumentSync`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireTextDocumentSyncOptions {
    /// Whether the server accepts open/close notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_close: Option<bool>,
    /// Change synchronization mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<WireTextDocumentSyncKind>,
}

/// Either `textDocumentSync` wire form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireTextDocumentSync {
    /// Legacy enum.
    Kind(WireTextDocumentSyncKind),
    /// Options object.
    Options(WireTextDocumentSyncOptions),
}

/// Boolean or options-object provider capability.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireProviderCapability {
    /// Boolean support declaration.
    Bool(bool),
    /// Provider options object.
    Options(Map<String, Value>),
}

/// Server capability fields inspected by the stdio host.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServerCapabilities {
    /// Position encoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_encoding: Option<String>,
    /// Document synchronization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_document_sync: Option<WireTextDocumentSync>,
    /// Definition support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_provider: Option<WireProviderCapability>,
    /// References support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references_provider: Option<WireProviderCapability>,
    /// Implementation support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation_provider: Option<WireProviderCapability>,
    /// Hover support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<WireProviderCapability>,
}

/// `initialize` result envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireInitializeResult {
    /// Server capabilities.
    pub capabilities: WireServerCapabilities,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn protocol_unions_accept_only_the_declared_wire_forms() {
        for value in [json!(0), json!(1), json!(2)] {
            assert!(serde_json::from_value::<WireTextDocumentSyncKind>(value).is_ok());
        }
        assert!(serde_json::from_value::<WireTextDocumentSyncKind>(json!(3)).is_err());
        assert!(serde_json::from_value::<WireMarkupKind>(json!("markdown")).is_ok());
        assert!(serde_json::from_value::<WireMarkupKind>(json!("html")).is_err());
        let hover = serde_json::from_value::<WireHover>(json!({
            "contents": [{"language": "rust", "value": "fn main() {}"}, "plain"]
        }))
        .unwrap();
        assert!(matches!(hover.contents, WireHoverContents::MarkedList(_)));
    }
}
