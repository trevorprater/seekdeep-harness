//! Pure types of the permission domain.

use serde::{Deserialize, Serialize};

/// The select-option shape a presentation layer advertises for one preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetOption {
    /// Stable option value: the table key, or custom.
    pub value: String,
    /// The display label.
    pub name: String,
    /// One user-facing sentence; omitted when not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Whole permissions projection value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSelect {
    /// Switchable presets, plus custom appended exactly while current.
    pub options: Vec<PresetOption>,
    /// The effective current value.
    pub current_value: String,
}
