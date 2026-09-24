//! Normalization of arbitrary adapter failures into serializable facts.

use crate::{HarnessError, LlmFailure, error::LlmError};

/// A value rejected by a final adapter iterator boundary.
///
/// Rust-native adapters normally use [`Self::Native`]. Compatibility adapters
/// use [`Self::Thrown`] after safely applying the foreign runtime's string
/// coercion, or [`Self::ForeignError`] after reading own data properties without
/// invoking accessors. This keeps arbitrary JavaScript rejection behavior
/// representable without turning middleware and consumer errors into terminal
/// model chunks.
#[derive(Debug)]
pub enum AdapterRejection {
    /// A normal Rust error, including concrete [`LlmError`] and
    /// [`HarnessError`] values.
    Native(anyhow::Error),
    /// A non-Error thrown value after contained string coercion. `None` means
    /// coercion itself failed; an empty string uses the canonical fallback.
    Thrown(Option<String>),
    /// A foreign Error instance represented only by safely-read own data.
    ForeignError {
        /// Safely-read Error message; absent for a missing, invalid, or hostile
        /// accessor-backed value.
        message: Option<String>,
        /// Safely-read own data-backed `code`.
        own_code: Option<String>,
        /// Safely-read, detached own data-backed `failure` snapshot.
        failure: Option<LlmFailure>,
    },
}

impl AdapterRejection {
    /// Represents a non-Error rejection whose foreign string coercion
    /// succeeded.
    #[must_use]
    pub fn thrown(rendered: impl Into<String>) -> Self {
        Self::Thrown(Some(rendered.into()))
    }

    /// Represents a non-Error rejection whose foreign string coercion threw.
    #[must_use]
    pub const fn unrenderable() -> Self {
        Self::Thrown(None)
    }

    /// Represents a foreign Error after accessor-free own-property reads.
    #[must_use]
    pub fn foreign_error(
        message: Option<String>,
        own_code: Option<String>,
        failure: Option<LlmFailure>,
    ) -> Self {
        Self::ForeignError {
            message,
            own_code,
            failure,
        }
    }

    /// Converts the rejection to an `anyhow` error without wrapping an already
    /// native error, preserving concrete downcast identity for direct adapter
    /// callers.
    #[must_use]
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Native(error) => error,
            rejection => anyhow::Error::new(rejection),
        }
    }
}

impl From<anyhow::Error> for AdapterRejection {
    fn from(error: anyhow::Error) -> Self {
        Self::Native(error)
    }
}

impl std::fmt::Display for AdapterRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(error) => std::fmt::Display::fmt(error, formatter),
            Self::Thrown(Some(message)) if !message.is_empty() => formatter.write_str(message),
            Self::Thrown(_) => formatter.write_str("LLM adapter failed"),
            Self::ForeignError { message, .. } => formatter.write_str(
                message
                    .as_deref()
                    .filter(|message| !message.is_empty())
                    .unwrap_or("LLM adapter failed"),
            ),
        }
    }
}

impl std::error::Error for AdapterRejection {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(error) => error.source(),
            Self::Thrown(_) | Self::ForeignError { .. } => None,
        }
    }
}

/// Converts a Rust adapter error into provider-neutral terminal facts.
#[must_use]
pub fn normalize_llm_failure(error: &anyhow::Error) -> LlmFailure {
    if let Some(error) = error.downcast_ref::<LlmError>() {
        return error.failure().clone();
    }
    if let Some(error) = error.downcast_ref::<HarnessError>() {
        return LlmFailure {
            message: error.message().to_owned(),
            code: error.code().to_owned(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
        };
    }
    fallback_failure(error.to_string(), "UNKNOWN")
}

/// Converts an arbitrary adapter rejection into provider-neutral terminal
/// facts.
#[must_use]
pub fn normalize_adapter_rejection(rejection: &AdapterRejection) -> LlmFailure {
    match rejection {
        AdapterRejection::Native(error) => normalize_llm_failure(error),
        AdapterRejection::Thrown(rendered) => {
            fallback_failure(rendered.clone().unwrap_or_default(), "UNKNOWN")
        }
        AdapterRejection::ForeignError {
            message,
            own_code,
            failure,
        } => {
            if let Some(failure) = failure
                && valid_failure_snapshot(failure)
                && own_code.as_deref() == Some(failure.code.as_str())
            {
                return failure.clone();
            }
            fallback_failure(message.clone().unwrap_or_default(), "UNKNOWN")
        }
    }
}

fn fallback_failure(message: String, code: &str) -> LlmFailure {
    LlmFailure {
        message: if message.is_empty() {
            "LLM adapter failed".to_owned()
        } else {
            message
        },
        code: code.to_owned(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}

fn valid_failure_snapshot(failure: &LlmFailure) -> bool {
    !failure.message.is_empty()
        && !failure.code.is_empty()
        && failure
            .status
            .is_none_or(|status| (100..=599).contains(&status))
        && failure
            .provider_retry_after_ms
            .is_none_or(|delay| delay.is_finite() && delay > 0.0)
        && failure
            .request_id
            .as_ref()
            .is_none_or(|request_id| !request_id.as_str().is_empty())
}

#[cfg(test)]
mod tests {
    use crate::ProviderRequestId;

    use super::*;

    fn unknown(message: &str) -> LlmFailure {
        LlmFailure {
            message: message.to_owned(),
            code: "UNKNOWN".to_owned(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
        }
    }

    #[test]
    fn arbitrary_non_error_rejections_use_contained_foreign_coercion() {
        assert_eq!(
            normalize_adapter_rejection(&AdapterRejection::thrown("plain provider failure")),
            unknown("plain provider failure")
        );
        assert_eq!(
            normalize_adapter_rejection(&AdapterRejection::thrown("")),
            unknown("LLM adapter failed")
        );
        assert_eq!(
            normalize_adapter_rejection(&AdapterRejection::thrown("null")),
            unknown("null")
        );
        assert_eq!(
            normalize_adapter_rejection(&AdapterRejection::unrenderable()),
            unknown("LLM adapter failed")
        );
    }

    #[test]
    fn foreign_failure_snapshot_requires_valid_facts_and_matching_own_code() {
        let facts = LlmFailure {
            message: "provider busy".to_owned(),
            code: "RATE_LIMIT".to_owned(),
            status: Some(429),
            provider_retry_after_ms: Some(1_500.0),
            request_id: Some(ProviderRequestId::new("req-7")),
        };
        assert_eq!(
            normalize_adapter_rejection(&AdapterRejection::foreign_error(
                Some("outer".to_owned()),
                Some("RATE_LIMIT".to_owned()),
                Some(facts.clone()),
            )),
            facts
        );

        let malformed = LlmFailure {
            request_id: Some(ProviderRequestId::new("")),
            ..facts.clone()
        };
        for rejection in [
            AdapterRejection::foreign_error(
                Some("provider failed".to_owned()),
                Some("OTHER".to_owned()),
                Some(facts),
            ),
            AdapterRejection::foreign_error(
                Some("provider failed".to_owned()),
                Some("RATE_LIMIT".to_owned()),
                Some(malformed),
            ),
            AdapterRejection::foreign_error(Some("provider failed".to_owned()), None, None),
        ] {
            assert_eq!(
                normalize_adapter_rejection(&rejection),
                unknown("provider failed")
            );
        }
    }
}
