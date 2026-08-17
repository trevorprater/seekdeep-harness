//! Request-header canonicalization, equality, and replay folding.

use seekdeep_llm::{LlmCallConfig, ToolSchema, call_config_equals};
use serde::{Deserialize, Serialize};

use crate::session::SessionEvent;

/// Marks configuration fields materialized by exact adapter resolution.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterDefaults {
    /// Adapter supplied the reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<bool>,
    /// Adapter supplied the output-token ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<bool>,
}

impl AdapterDefaults {
    fn canonical(self) -> Option<Self> {
        if self.reasoning_effort == Some(true) || self.max_tokens == Some(true) {
            Some(self)
        } else {
            None
        }
    }
}

/// Full request state in force for one model-call epoch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EpochHeader {
    /// Provider, model, reasoning, and sampling configuration.
    pub config: LlmCallConfig,
    /// Fields supplied by the resolved adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_defaults: Option<AdapterDefaults>,
    /// Rendered system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Ordered model-visible tool schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
}

/// Normalizes empty optional fields to canonical absence.
#[must_use]
pub fn canonical_header(mut header: EpochHeader) -> EpochHeader {
    header.adapter_defaults = header.adapter_defaults.and_then(AdapterDefaults::canonical);
    if header.system.as_ref().is_some_and(String::is_empty) {
        header.system = None;
    }
    if header.tools.as_ref().is_some_and(Vec::is_empty) {
        header.tools = None;
    }
    header
}

/// Compares all canonical header fields and preserves tool order.
#[must_use]
pub fn header_equals(left: &EpochHeader, right: &EpochHeader) -> bool {
    call_config_equals(&left.config, &right.config)
        && left
            .adapter_defaults
            .as_ref()
            .and_then(|value| value.reasoning_effort)
            == right
                .adapter_defaults
                .as_ref()
                .and_then(|value| value.reasoning_effort)
        && left
            .adapter_defaults
            .as_ref()
            .and_then(|value| value.max_tokens)
            == right
                .adapter_defaults
                .as_ref()
                .and_then(|value| value.max_tokens)
        && left.system == right.system
        && same_tools(
            left.tools.as_deref().unwrap_or_default(),
            right.tools.as_deref().unwrap_or_default(),
        )
}

fn same_tools(left: &[ToolSchema], right: &[ToolSchema]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            serde_json::to_string(left).ok() == serde_json::to_string(right).ok()
        })
}

/// Folds full `request/header` snapshots over an optional prior state.
#[must_use]
pub fn fold_request_header(
    events: &[SessionEvent],
    mut state: Option<EpochHeader>,
) -> Option<EpochHeader> {
    for event in events {
        if event.event_type != "request/header" {
            continue;
        }
        let Some(header) = event.data.get("header") else {
            continue;
        };
        if let Ok(header) = serde_json::from_value::<EpochHeader>(header.clone()) {
            state = Some(canonical_header(header));
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use seekdeep_llm::ToolSchema;
    use serde_json::Map;

    use super::*;

    fn config() -> LlmCallConfig {
        LlmCallConfig {
            provider: "mock".into(),
            model: "m".into(),
            reasoning_effort: None,
            temperature: None,
            max_tokens: None,
            stop: None,
        }
    }

    #[test]
    fn removes_empty_optional_fields() {
        let header = canonical_header(EpochHeader {
            config: config(),
            adapter_defaults: Some(AdapterDefaults::default()),
            system: Some(String::new()),
            tools: Some(Vec::new()),
        });
        assert_eq!(
            header,
            EpochHeader {
                config: config(),
                adapter_defaults: None,
                system: None,
                tools: None,
            }
        );
    }

    #[test]
    fn tool_order_is_significant() {
        let tool = |name: &str| ToolSchema {
            name: name.to_owned(),
            description: "d".to_owned(),
            parameters: Map::new(),
        };
        let left = EpochHeader {
            config: config(),
            adapter_defaults: None,
            system: None,
            tools: Some(vec![tool("a"), tool("b")]),
        };
        let mut right = left.clone();
        right.tools.as_mut().expect("tools").reverse();
        assert!(!header_equals(&left, &right));
    }
}
