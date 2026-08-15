//! Harness errors and provider-neutral failure classification.

use std::sync::OnceLock;

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
    failure: LlmFailure,
}

impl LlmError {
    /// Creates a provider-neutral error without optional provider facts.
    #[must_use]
    pub fn simple(message: impl Into<String>, code: impl Into<String>) -> Self {
        let message = message.into();
        let code = code.into();
        Self {
            failure_message: message.clone(),
            failure: LlmFailure {
                message,
                code,
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            },
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
            failure: LlmFailure {
                message,
                code,
                status,
                provider_retry_after_ms,
                request_id,
            },
        })
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
    let value = raw.trim();
    if value.is_empty() {
        ApiKeyCheck::Empty
    } else if value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) && value.is_ascii() {
        ApiKeyCheck::Usable(value.to_owned())
    } else {
        ApiKeyCheck::IllegalCharacters
    }
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
        failure: LlmFailure {
            message,
            code: INVALID_CREDENTIAL_CODE.to_owned(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
        },
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

/// Renders a standard Rust error and its source chain, suppressing verbatim repeats.
#[must_use]
pub fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut output = error.to_string();
    let mut source = error.source();
    while let Some(current) = source {
        let message = current.to_string();
        if !message.is_empty() && message != output {
            output.push_str(": ");
            output.push_str(&message);
        }
        source = current.source();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

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
