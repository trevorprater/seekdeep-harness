//! Closed semantic LSP request, provider, and result vocabulary.

use async_trait::async_trait;
use indexmap::IndexMap;
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};

use crate::LspProviderId;

/// Four semantic queries exposed by the capability seam.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    /// Navigate to a definition.
    GoToDefinition,
    /// Find references, including declarations.
    FindReferences,
    /// Navigate to an implementation.
    GoToImplementation,
    /// Read hover content.
    Hover,
}

impl LspOperation {
    /// Exact source-compatible operation spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GoToDefinition => "goToDefinition",
            Self::FindReferences => "findReferences",
            Self::GoToImplementation => "goToImplementation",
            Self::Hover => "hover",
        }
    }
}

/// Zero-based UTF-16 cursor coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspPosition {
    /// Zero-based line.
    #[serde(serialize_with = "serialize_js_number")]
    pub line: f64,
    /// Zero-based UTF-16 code-unit offset.
    #[serde(serialize_with = "serialize_js_number")]
    pub character: f64,
}

/// Serializes a JavaScript number with integral values using JSON's integer spelling.
///
/// # Errors
///
/// Returns the selected serializer's numeric failure.
pub fn serialize_js_number<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if *value == 0.0 {
        return serializer.serialize_i64(0);
    }
    if value.is_finite() && value.fract() == 0.0 {
        if *value >= -9_223_372_036_854_775_808.0 && *value < 9_223_372_036_854_775_808.0 {
            #[allow(clippy::cast_possible_truncation)]
            return serializer.serialize_i64(*value as i64);
        }
        if *value > 0.0 && *value < 18_446_744_073_709_551_616.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            return serializer.serialize_u64(*value as u64);
        }
    }
    serializer.serialize_f64(*value)
}

/// Zero-based UTF-16 half-open range.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspRange {
    /// Inclusive start.
    pub start: LspPosition,
    /// Exclusive end.
    pub end: LspPosition,
}

/// Caller-authored normalized semantic query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspQueryRequest {
    /// Semantic operation.
    pub operation: LspOperation,
    /// Relative or absolute source path.
    pub file_path: String,
    /// Zero-based UTF-16 position.
    pub position: LspPosition,
    /// Required workspace root.
    pub workspace_root: String,
}

/// Provider-facing query with a registry-derived language id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspProviderQuery {
    /// Semantic operation.
    pub operation: LspOperation,
    /// Relative or absolute source path.
    pub file_path: String,
    /// Zero-based UTF-16 position.
    pub position: LspPosition,
    /// Required workspace root.
    pub workspace_root: String,
    /// Provider registration's language id.
    pub language_id: String,
}

impl LspProviderQuery {
    pub(crate) fn new(request: LspQueryRequest, language_id: String) -> Self {
        Self {
            operation: request.operation,
            file_path: request.file_path,
            position: request.position,
            workspace_root: request.workspace_root,
            language_id,
        }
    }
}

/// One resolved server location.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspLocation {
    /// Target document URI.
    pub uri: String,
    /// Target range.
    pub range: LspRange,
}

/// Normalized hover content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspHover {
    /// Markdown or plaintext content.
    pub contents: String,
    /// Optional server-supplied range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<LspRange>,
}

/// Closed semantic query result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LspQueryResult {
    /// Navigation locations plus the provider's canonical workspace URI.
    Locations {
        /// Ordered normalized locations.
        locations: Vec<LspLocation>,
        /// Canonical provider-side workspace URI.
        #[serde(rename = "resolvedWorkspaceUri")]
        resolved_workspace_uri: String,
    },
    /// Hover content, or no hover at the position.
    Hover {
        /// Normalized hover response.
        hover: Option<LspHover>,
    },
}

/// Language-server backend registered on the LSP seam.
#[async_trait]
pub trait LspProvider: std::fmt::Debug + Send + Sync {
    /// Stable provider identity.
    fn id(&self) -> &LspProviderId;

    /// Lowercase-leading-dot extension mappings before registry normalization.
    fn extension_to_language(&self) -> &IndexMap<String, String>;

    /// Runs one registry-resolved semantic query.
    async fn query(
        &self,
        request: LspProviderQuery,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult>;
}
