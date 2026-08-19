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
    use seekdeep_llm::{ReasoningEffortId, ToolSchema};
    use serde_json::{Map, Value, json};

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

    fn header_event(seq: u64, header: Value) -> SessionEvent {
        let mut data = Map::new();
        data.insert("header".to_owned(), header);
        SessionEvent {
            event_type: "request/header".to_owned(),
            seq,
            time: i64::try_from(seq).expect("test seq"),
            data: Value::Object(data),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn fold_returns_baseline_without_a_snapshot_and_skips_unrelated_events() {
        let baseline = EpochHeader {
            config: config(),
            adapter_defaults: None,
            system: Some("baseline".to_owned()),
            tools: None,
        };
        let unrelated = SessionEvent {
            event_type: "turn/start".to_owned(),
            seq: 0,
            time: 1,
            data: json!({"turn": 1}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        assert_eq!(fold_request_header(&[], None), None);
        assert_eq!(
            fold_request_header(&[unrelated], Some(baseline.clone())),
            Some(baseline.clone())
        );
    }

    #[test]
    fn fold_takes_the_latest_full_snapshot_and_canonicalizes_it() {
        let config = config();
        let events = vec![
            header_event(0, json!({"config": config, "system": "first"})),
            header_event(1, json!({"config": config, "tools": []})),
        ];
        assert_eq!(
            fold_request_header(&events, None),
            Some(EpochHeader {
                config,
                adapter_defaults: None,
                system: None,
                tools: None,
            })
        );
    }

    #[test]
    fn treats_absent_and_empty_tools_as_equivalent_absence() {
        let a = EpochHeader {
            config: config(),
            adapter_defaults: None,
            system: None,
            tools: None,
        };
        let mut b = a.clone();
        b.tools = Some(Vec::new());
        assert!(header_equals(&a, &b));
    }

    #[test]
    fn compares_every_canonical_field() {
        let tool = |name: &str, description: &str| ToolSchema {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: Map::new(),
        };
        let base = canonical_header(EpochHeader {
            config: config(),
            adapter_defaults: None,
            system: Some("s".to_owned()),
            tools: Some(vec![tool("a", "d")]),
        });
        assert!(header_equals(&base, &base.clone()));

        let mut other_model = base.clone();
        other_model.config.model = "other".into();
        assert!(!header_equals(&base, &other_model));

        let mut other_reasoning = base.clone();
        other_reasoning.config.reasoning_effort = Some(ReasoningEffortId::new("high"));
        assert!(!header_equals(&base, &other_reasoning));

        let mut resolved_max = base.clone();
        resolved_max.config.max_tokens = Some(256_000);
        let mut unresolved_max = resolved_max.clone();
        unresolved_max.adapter_defaults = Some(AdapterDefaults {
            reasoning_effort: None,
            max_tokens: Some(true),
        });
        assert!(!header_equals(&resolved_max, &unresolved_max));

        let mut other_system = base.clone();
        other_system.system = Some("other".to_owned());
        assert!(!header_equals(&base, &other_system));

        let mut other_tool = base.clone();
        other_tool.tools = Some(vec![tool("a", "changed")]);
        assert!(!header_equals(&base, &other_tool));
    }
}
