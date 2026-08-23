//! Durable Session record of the Agent preset that produced its model-visible history.

use seekdeep_core::session::{SessionEvent, SessionHeader};
use serde_json::Value;

/// Resolves the latest logged selection over the creation-time header.
#[must_use]
pub fn resolve_session_preset(header: &SessionHeader, events: &[SessionEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|event| {
            (event.event_type == "agent-preset/selected")
                .then(|| event.data.get("agentPreset").and_then(Value::as_str))
                .flatten()
                .map(ToOwned::to_owned)
        })
        .or_else(|| header.agent_preset.clone())
}
