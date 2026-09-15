//! Conversation call-configuration comparison utilities.

use crate::types::LlmCallConfig;

/// Field-wise equality for every request-epoch configuration field.
#[must_use]
pub fn call_config_equals(left: &LlmCallConfig, right: &LlmCallConfig) -> bool {
    left.provider == right.provider
        && left.model == right.model
        && left.reasoning_effort == right.reasoning_effort
        && left.temperature == right.temperature
        && left.max_tokens == right.max_tokens
        && left.stop == right.stop
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> LlmCallConfig {
        LlmCallConfig {
            provider: crate::ProviderId::new("p"),
            model: crate::ModelId::new("m"),
            reasoning_effort: None,
            temperature: None,
            max_tokens: None,
            stop: None,
        }
    }

    #[test]
    fn compares_stop_lists_element_by_element() {
        let mut left = base();
        let mut right = base();
        left.stop = Some(vec!["a".to_owned(), "b".to_owned()]);
        right.stop = Some(vec!["a".to_owned(), "b".to_owned()]);
        assert!(call_config_equals(&left, &right));
        right.stop = Some(vec!["b".to_owned(), "a".to_owned()]);
        assert!(!call_config_equals(&left, &right));
    }
}
