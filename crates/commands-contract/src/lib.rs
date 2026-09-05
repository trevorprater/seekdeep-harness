//! Target-portable human-command wire contracts.

use serde::{Deserialize, Serialize};

/// Immutable metadata for optional unstructured input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputDescriptor {
    /// Placeholder shown before free-form input.
    pub hint: String,
}

/// Handler-free immutable discovery view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandDescriptor {
    /// Lowercase command name without slash.
    pub name: String,
    /// Human-readable summary.
    pub description: String,
    /// Optional free-form input metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<CommandInputDescriptor>,
}
