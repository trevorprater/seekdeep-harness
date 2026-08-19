//! The durable subagent-child descriptor: the versioned, model-hidden
//! subagent/descriptor event.

use seekdeep_core::session::SessionEvent;
use seekdeep_tools::ToolRestriction;
use serde::{Deserialize, Serialize};

/// The current descriptor format version.
pub const SUBAGENT_DESCRIPTOR_VERSION: u32 = 2;

/// The supported durable subagent identity and optional continuation
/// composition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum SubagentDescriptorData {
    /// A terminal one-shot child.
    #[serde(rename = "one-shot")]
    OneShot {
        /// Descriptor format version.
        version: u32,
        /// Provider name that established the child.
        provider: String,
        /// Optional durable creation label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// A resumable conversation.
    Continuable {
        /// Descriptor format version.
        version: u32,
        /// Provider name that established the child.
        provider: String,
        /// Durable creation label.
        label: String,
        /// Resolved child provider, when declared.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_provider: Option<String>,
        /// Resolved child model, when declared.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_model: Option<String>,
        /// Per-child persona.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persona: Option<String>,
        /// Child tool scoping.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_filter: Option<ToolRestriction>,
    },
}

/// Inputs snapshot_subagent_descriptor validates and detaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum SubagentDescriptorInput {
    /// A one-shot child identity.
    #[serde(rename = "one-shot")]
    OneShot {
        /// Provider name.
        provider: String,
        /// Optional creation label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// A continuable child identity and resumable composition.
    Continuable {
        /// Provider name.
        provider: String,
        /// Creation label.
        label: String,
        /// Requested child provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_provider: Option<String>,
        /// Requested child model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_model: Option<String>,
        /// Requested per-child persona.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persona: Option<String>,
        /// Requested child tool scoping.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_filter: Option<ToolRestriction>,
    },
}

/// Validates and detaches descriptor inputs into the durable payload.
///
/// # Errors
///
/// Returns when the input is not losslessly JSON-serializable.
pub fn snapshot_subagent_descriptor(input: &SubagentDescriptorInput) -> anyhow::Result<SubagentDescriptorData> {
    let data = match input {
        SubagentDescriptorInput::OneShot { provider, label } => SubagentDescriptorData::OneShot {
            version: SUBAGENT_DESCRIPTOR_VERSION,
            provider: provider.clone(),
            label: label.clone(),
        },
        SubagentDescriptorInput::Continuable {
            provider,
            label,
            agent_provider,
            agent_model,
            persona,
            tool_filter,
        } => SubagentDescriptorData::Continuable {
            version: SUBAGENT_DESCRIPTOR_VERSION,
            provider: provider.clone(),
            label: label.clone(),
            agent_provider: agent_provider.clone(),
            agent_model: agent_model.clone(),
            persona: persona.clone(),
            tool_filter: tool_filter.clone(),
        },
    };
    // Round-trip detaches the payload at the lossless-JSON boundary.
    Ok(serde_json::from_value(serde_json::to_value(&data)?)?)
}

/// Folds a persisted child log to its supported descriptor.
///
/// # Errors
///
/// Returns when a current-version persisted payload does not match its
/// declared schema.
pub fn fold_subagent_descriptor(
    events: &[SessionEvent],
) -> anyhow::Result<Option<SubagentDescriptorData>> {
    let Some(event) = events.iter().find(|e| e.event_type == "subagent/descriptor") else {
        return Ok(None);
    };
    let version = event.data.get("version").and_then(|v| v.as_u64());
    if version != Some(u64::from(SUBAGENT_DESCRIPTOR_VERSION)) {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value::<SubagentDescriptorData>(
        event.data.clone(),
    )?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            event_type: "subagent/descriptor".to_owned(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn folds_one_shot_descriptor() {
        let events = vec![event(json!({
            "version": 2, "mode": "one-shot", "provider": "spawn", "label": "audit",
        }))];
        let folded = fold_subagent_descriptor(&events).expect("fold").expect("some");
        assert!(matches!(folded, SubagentDescriptorData::OneShot { .. }));
    }

    #[test]
    fn ignores_unrecognized_version() {
        let events = vec![event(json!({"version": 99, "mode": "one-shot", "provider": "x"}))];
        assert!(fold_subagent_descriptor(&events).expect("fold").is_none());
    }
}
