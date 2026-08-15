//! Normalization of arbitrary adapter failures into serializable facts.

use crate::{HarnessError, LlmFailure, error::LlmError};

/// Converts a Rust adapter error into the provider-neutral terminal facts.
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
    let message = error.to_string();
    LlmFailure {
        message: if message.is_empty() {
            "LLM adapter failed".to_owned()
        } else {
            message
        },
        code: "UNKNOWN".to_owned(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}
