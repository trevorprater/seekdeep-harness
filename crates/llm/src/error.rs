//! Harness errors and provider-neutral failure classification.

use std::{collections::HashSet, sync::OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::brand::ProviderRequestId;

/// Model context capacity was exceeded.
pub const CONTEXT_WINDOW_EXCEEDED_CODE: &str = "CONTEXT_WINDOW_EXCEEDED";
/// Account quota or balance was exhausted.
pub const QUOTA_EXCEEDED_CODE: &str = "QUOTA";
/// Successful provider completion carried no content.
pub const EMPTY_RESPONSE_CODE: &str = "EMPTY_RESPONSE";
/// A supplied credential cannot be carried in an HTTP header.
pub const INVALID_CREDENTIAL_CODE: &str = "INVALID_CREDENTIAL";

/// Serializable provider or transport failure facts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmFailure {
    /// Human-readable summary.
    pub message: String,
    /// Stable provider-neutral code.
    pub code: String,
    /// Provider HTTP status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Provider-requested retry delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_retry_after_ms: Option<f64>,
    /// Opaque provider request identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<ProviderRequestId>,
}

/// Harness error carrying a stable machine-routable code.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct HarnessError {
    name: String,
    message: String,
    code: String,
    #[source]
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl HarnessError {
    /// Creates an error with optional chained cause.
    #[must_use]
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            name: "HarnessError".to_owned(),
            message: message.into(),
            code: code.into(),
            cause: None,
        }
    }

    /// Creates a named harness-error subclass equivalent.
    #[must_use]
    pub fn named(
        name: impl Into<String>,
        message: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
            code: code.into(),
            cause: None,
        }
    }

    /// Adds a source error.
    #[must_use]
    pub fn with_cause(mut self, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// Stable routing code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Stable JavaScript error-class name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human-readable message without its cause chain.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Validated LLM error with detached provider facts.
#[derive(Debug, Error)]
#[error("{failure_message}")]
pub struct LlmError {
    failure_message: String,
    failure: Box<LlmFailure>,
    #[source]
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl LlmError {
    /// Creates a provider-neutral error without optional provider facts.
    #[must_use]
    pub fn simple(message: impl Into<String>, code: impl Into<String>) -> Self {
        let message = message.into();
        let code = code.into();
        Self {
            failure_message: message.clone(),
            failure: Box::new(LlmFailure {
                message,
                code,
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            }),
            cause: None,
        }
    }

    /// Validates provider facts and creates an LLM error.
    ///
    /// # Errors
    ///
    /// Returns a validation error for empty fields, invalid status, or a zero retry delay.
    pub fn new(
        message: impl Into<String>,
        code: impl Into<String>,
        status: Option<u16>,
        provider_retry_after_ms: Option<f64>,
        request_id: Option<ProviderRequestId>,
    ) -> anyhow::Result<Self> {
        let message = message.into();
        let code = code.into();
        anyhow::ensure!(
            !message.is_empty(),
            "LlmError message must be a non-empty string"
        );
        anyhow::ensure!(!code.is_empty(), "LlmError code must be a non-empty string");
        if let Some(status) = status {
            anyhow::ensure!(
                (100..=599).contains(&status),
                "LlmError status must be an integer from 100 through 599"
            );
        }
        if let Some(delay) = provider_retry_after_ms {
            anyhow::ensure!(
                delay.is_finite() && delay > 0.0,
                "LlmError providerRetryAfterMs must be a positive finite number"
            );
        }
        if let Some(request_id) = &request_id {
            anyhow::ensure!(
                !request_id.as_str().is_empty(),
                "LlmError requestId must be a non-empty string"
            );
        }
        Ok(Self {
            failure_message: message.clone(),
            failure: Box::new(LlmFailure {
                message,
                code,
                status,
                provider_retry_after_ms,
                request_id,
            }),
            cause: None,
        })
    }

    /// Attaches the original provider or transport failure.
    #[must_use]
    pub fn with_cause(mut self, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// Serializable provider-neutral facts.
    #[must_use]
    pub fn failure(&self) -> &LlmFailure {
        &self.failure
    }

    /// Stable routing code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.failure.code
    }

    /// Stable JavaScript error-class name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        "LlmError"
    }

    /// Human-readable message without its cause chain.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.failure.message
    }
}

/// Narrows a concrete Rust error to either shared harness-error class.
///
/// Like source `instanceof`, structurally similar foreign errors do not
/// classify; only the actual [`HarnessError`] and [`LlmError`] types do.
#[must_use]
pub fn is_harness_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.is::<HarnessError>() || error.is::<LlmError>()
}

/// API-key normalization verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiKeyCheck {
    /// Trimmed printable-ASCII key.
    Usable(String),
    /// Empty after trimming.
    Empty,
    /// Contains a character outside `!` through `~`.
    IllegalCharacters,
}

/// Trims and validates one supplied provider API key.
#[must_use]
pub fn normalize_api_key(raw: &str) -> ApiKeyCheck {
    let value = raw.trim_matches(is_ecmascript_trim_character);
    if value.is_empty() {
        ApiKeyCheck::Empty
    } else if value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) && value.is_ascii() {
        ApiKeyCheck::Usable(value.to_owned())
    } else {
        ApiKeyCheck::IllegalCharacters
    }
}

const fn is_ecmascript_trim_character(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            ..='\u{000d}'
                | '\u{0020}'
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Returns a usable key or a secret-free diagnostic naming its source.
///
/// # Errors
///
/// Returns [`LlmError`] for blank or illegal supplied credentials.
pub fn assert_usable_api_key(
    raw: &str,
    package: &str,
    reference: &str,
) -> Result<String, LlmError> {
    match normalize_api_key(raw) {
        ApiKeyCheck::Usable(value) => Ok(value),
        ApiKeyCheck::Empty => Err(invalid_credential(format!(
            "{package}: the API key resolved from {reference} is blank; set {reference} to the raw key (the web Models page writes it) or export it in the launching environment"
        ))),
        ApiKeyCheck::IllegalCharacters => Err(invalid_credential(format!(
            "{package}: the API key resolved from {reference} contains characters no HTTP header can carry; set {reference} to the raw key alone (the web Models page writes it)"
        ))),
    }
}

fn invalid_credential(message: String) -> LlmError {
    LlmError {
        failure_message: message.clone(),
        failure: Box::new(LlmFailure {
            message,
            code: INVALID_CREDENTIAL_CODE.to_owned(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
        }),
        cause: None,
    }
}

/// Recognizes provider diagnostics that identify model context overflow.
///
/// # Panics
///
/// Panics only if one of the compile-time constant classifier expressions is invalid.
#[must_use]
pub fn is_context_window_exceeded_error(detail: &str) -> bool {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            [
                r"(?i)(?:^|[^a-z0-9])context[\s_-](?:length|window)[\s_-](?:exceed(?:ed|s)?|overflow(?:ed)?|limit[\s_-]exceeded)(?:$|[^a-z0-9])",
                r"(?i)\b(?:maximum|max)(?:\s+(?:allowed|supported))?\s+context\s+(?:length|window)\b",
                r"(?i)\b(?:request|prompt|input|messages?)\s+(?:is\s+|are\s+)?too\s+(?:large|long)\s+for\s+(?:(?:this|the)\s+)?(?:model(?:'s)?\s+)?context(?:\s+window)?\b",
                r"(?i)\b(?:input|prompt|request)\s+(?:is\s+)?too\s+(?:long|large)\s+for\s+(?:this|the)\s+model\b",
                r"(?i)\b(?:input|prompt|request|messages?)\b.{0,40}\b(?:exceed(?:s|ed)?|overflows?|is\s+larger\s+than)\b.{0,40}\b(?:the\s+)?(?:model(?:'s)?\s+)?context(?:\s+(?:length|window))?\b",
            ]
            .into_iter()
            .map(|pattern| Regex::new(pattern).expect("static overflow regex is valid"))
            .collect()
        })
        .iter()
        .any(|pattern| pattern.is_match(detail))
}

/// Recognizes terminal account quota, balance, credit, budget, or usage-limit wording.
///
/// # Panics
///
/// Panics only if one of the compile-time constant classifier expressions is invalid.
#[must_use]
pub fn is_quota_exceeded_error(detail: &str) -> bool {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            [
                r"(?i)\binsufficient[\s_-]+(?:quota|balance|credits?)\b",
                r"(?i)\b(?:quota|usage[\s_-]+limit)[\s_-]+(?:exceeded|exhausted|reached)\b",
                r"(?i)\bexceed(?:ed|s)?[\s_-]+(?:(?:your|the)[\s_-]+)?(?:current[\s_-]+)?quota\b",
                r"(?i)\b(?:balance|credits?)[\s_-]+(?:exhausted|depleted)\b",
                r"(?i)\bout[\s_-]+of[\s_-]+(?:credits?|budget)\b",
            ]
            .into_iter()
            .map(|pattern| Regex::new(pattern).expect("static quota regex is valid"))
            .collect()
        })
        .iter()
        .any(|pattern| pattern.is_match(detail))
}

/// A safely-inspected foreign value in an [`ErrorChainGraph`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum ErrorChainNode {
    Rendered(String),
    Error {
        name: String,
        message: String,
        members: Vec<usize>,
        cause: Option<usize>,
    },
    Unrenderable,
}

/// Identity-preserving graph used by compatibility bindings to implement the
/// source `errorChain(unknown)` contract.
///
/// Bindings inspect foreign values inside their own exception boundary, append
/// nodes, and preserve object identity by reusing node indexes. That admits
/// primitive throws, own data-backed structured messages, `AggregateError`
/// members, diamond sharing, cycles, and hostile accessors without placing a
/// foreign runtime object inside native Rust error chains.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorChainGraph {
    nodes: Vec<ErrorChainNode>,
}

impl ErrorChainGraph {
    /// Reserves one identity before its properties are inspected, allowing a
    /// binding to preserve self-references and mutually recursive graphs.
    #[must_use]
    pub fn reserve(&mut self) -> usize {
        self.push(ErrorChainNode::Unrenderable)
    }

    /// Fills a previously reserved identity with safely-inspected Error data.
    /// Returns `false` when `index` was not reserved in this graph.
    #[must_use]
    pub fn set_error(
        &mut self,
        index: usize,
        name: impl Into<String>,
        message: impl Into<String>,
        members: Vec<usize>,
        cause: Option<usize>,
    ) -> bool {
        let Some(node) = self.nodes.get_mut(index) else {
            return false;
        };
        *node = ErrorChainNode::Error {
            name: name.into(),
            message: message.into(),
            members,
            cause,
        };
        true
    }

    /// Appends a value whose foreign string coercion succeeded.
    #[must_use]
    pub fn push_rendered(&mut self, rendered: impl Into<String>) -> usize {
        self.push(ErrorChainNode::Rendered(rendered.into()))
    }

    /// Appends a value whose coercion or property inspection threw.
    #[must_use]
    pub fn push_unrenderable(&mut self) -> usize {
        self.push(ErrorChainNode::Unrenderable)
    }

    /// Appends one safely-inspected foreign Error.
    #[must_use]
    pub fn push_error(
        &mut self,
        name: impl Into<String>,
        message: impl Into<String>,
        members: Vec<usize>,
        cause: Option<usize>,
    ) -> usize {
        self.push(ErrorChainNode::Error {
            name: name.into(),
            message: message.into(),
            members,
            cause,
        })
    }

    /// Renders one root node with active-path cycle detection.
    #[must_use]
    pub fn render(&self, root: usize) -> String {
        self.render_node(root, &mut HashSet::new())
    }

    fn push(&mut self, node: ErrorChainNode) -> usize {
        let index = self.nodes.len();
        self.nodes.push(node);
        index
    }

    fn render_node(&self, index: usize, path: &mut HashSet<usize>) -> String {
        let Some(node) = self.nodes.get(index) else {
            return "<unrenderable value>".to_owned();
        };
        if !path.insert(index) {
            return "<circular cause>".to_owned();
        }
        let rendered = match node {
            ErrorChainNode::Rendered(value) => value.clone(),
            ErrorChainNode::Unrenderable => "<unrenderable value>".to_owned(),
            ErrorChainNode::Error {
                name,
                message,
                members,
                cause,
            } => {
                let message = if message.is_empty() { name } else { message };
                let members = if members.is_empty() {
                    String::new()
                } else {
                    format!(
                        " [{}]",
                        members
                            .iter()
                            .map(|member| self.render_node(*member, path))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                };
                let cause = cause
                    .map(|cause| self.render_node(cause, path))
                    .filter(|cause| !cause.is_empty() && cause != message)
                    .map_or_else(String::new, |cause| format!(": {cause}"));
                format!("{message}{members}{cause}")
            }
        };
        path.remove(&index);
        rendered
    }
}

/// Renders a standard Rust error and its source chain, suppressing verbatim
/// repeats at each recursive wrapper boundary.
#[must_use]
pub fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    fn render(error: &(dyn std::error::Error + 'static), path: &mut HashSet<usize>) -> String {
        let identity = std::ptr::from_ref(error).cast::<()>() as usize;
        if !path.insert(identity) {
            return "<circular cause>".to_owned();
        }
        let raw_message = error.to_string();
        let message = if raw_message.is_empty() {
            if let Some(error) = error.downcast_ref::<HarnessError>() {
                error.name().to_owned()
            } else if let Some(error) = error.downcast_ref::<LlmError>() {
                error.name().to_owned()
            } else {
                "Error".to_owned()
            }
        } else {
            raw_message
        };
        let cause = error
            .source()
            .map(|source| render(source, path))
            .filter(|cause| !cause.is_empty() && cause != &message)
            .map_or_else(String::new, |cause| format!(": {cause}"));
        path.remove(&identity);
        format!("{message}{cause}")
    }

    render(error, &mut HashSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_error_graph_handles_aggregates_diamonds_cycles_and_hostile_nodes() {
        let mut graph = ErrorChainGraph::default();
        let shared = graph.push_error("Error", "shared", Vec::new(), None);
        let left = graph.push_error("Error", "a", Vec::new(), Some(shared));
        let right = graph.push_error("Error", "b", Vec::new(), Some(shared));
        let aggregate = graph.push_error("AggregateError", "agg", vec![left, right], None);
        assert_eq!(graph.render(aggregate), "agg [a: shared; b: shared]");

        let hostile = graph.push_unrenderable();
        let outer = graph.push_error("Error", "outer", Vec::new(), Some(hostile));
        assert_eq!(graph.render(outer), "outer: <unrenderable value>");

        let circular = graph.push_error("Error", "cycle", Vec::new(), None);
        let mut cyclic = ErrorChainGraph::default();
        let identity = cyclic.reserve();
        assert!(cyclic.set_error(identity, "Error", "outer", Vec::new(), Some(identity)));
        assert_eq!(cyclic.render(0), "outer: <circular cause>");

        assert_eq!(graph.render(circular), "cycle");
        assert_eq!(graph.render(usize::MAX), "<unrenderable value>");
    }

    #[derive(Debug, Error)]
    #[error("{message}")]
    struct TestCause {
        message: &'static str,
        #[source]
        source: Option<Box<TestCause>>,
    }

    #[test]
    fn native_chain_collapses_a_repeated_nested_wrapper_at_its_own_boundary() {
        let error = TestCause {
            message: "outer",
            source: Some(Box::new(TestCause {
                message: "same",
                source: Some(Box::new(TestCause {
                    message: "same",
                    source: None,
                })),
            })),
        };
        assert_eq!(error_chain(&error), "outer: same");
    }

    #[test]
    fn api_keys_use_printable_ascii_without_spaces() {
        assert_eq!(
            normalize_api_key("  sk-abc\t\n"),
            ApiKeyCheck::Usable("sk-abc".to_owned())
        );
        assert_eq!(normalize_api_key("   "), ApiKeyCheck::Empty);
        assert_eq!(
            normalize_api_key("sk-abc def"),
            ApiKeyCheck::IllegalCharacters
        );
        assert_eq!(
            normalize_api_key("!~"),
            ApiKeyCheck::Usable("!~".to_owned())
        );
        assert_eq!(
            normalize_api_key("\u{feff}sk-bom\u{feff}"),
            ApiKeyCheck::Usable("sk-bom".to_owned())
        );
        assert_eq!(
            normalize_api_key("\u{0085}sk-nel\u{0085}"),
            ApiKeyCheck::IllegalCharacters
        );
    }

    #[test]
    fn secret_refusal_never_echoes_the_key() {
        let error =
            assert_usable_api_key("sk-😀supersecret", "llm", "KEY").expect_err("illegal key");
        assert_eq!(error.code(), INVALID_CREDENTIAL_CODE);
        assert!(!error.to_string().contains("supersecret"));
        assert!(error.to_string().contains("KEY"));
    }
}
