//! Agent-preset vocabulary shared by discovery, mounting, and consumers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Trust inherited from the root that supplied a preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetTrust {
    /// Ships with the deployment.
    System,
    /// Authored locally with the same trust as shell access.
    #[default]
    User,
}

/// One directory holding a mountable or visibly broken Agent composition.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentPreset {
    /// Stable directory-name identity.
    pub id: String,
    /// Root-derived trust.
    pub trust: PresetTrust,
    /// Absolute composition path.
    pub path: PathBuf,
    /// Optional display name.
    pub name: Option<String>,
    /// Optional one-sentence purpose.
    pub description: Option<String>,
    /// Optional declared roster order.
    pub order: Option<f64>,
    /// Why this occupied slot cannot mount.
    pub broken: Option<String>,
}

/// One scanned preset root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetRoot {
    /// Directory holding one subdirectory per preset.
    pub path: String,
    /// Trust applied to every discovered child.
    #[serde(default)]
    pub trust: PresetTrust,
}

/// Roster configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetConfig {
    /// Preset mounted when a caller names none.
    pub default: String,
    /// Roots in first-wins precedence order.
    #[serde(default)]
    pub roots: Vec<PresetRoot>,
    /// Whether the harness-home user root follows configured roots.
    #[serde(default = "default_include_user_root")]
    pub include_user_root: bool,
}

const fn default_include_user_root() -> bool {
    true
}

/// Whether an id is a safe lowercase directory segment.
#[must_use]
pub fn valid_preset_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// No configured root supplies one requested preset.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("agent-presets: preset \"{preset_id}\" not found (available: {available_display})")]
pub struct UnknownPresetError {
    /// Requested identity.
    pub preset_id: String,
    /// Current roster identities.
    pub available: Vec<String>,
    available_display: String,
}

impl UnknownPresetError {
    /// Creates one roster miss with a stable candidate list.
    #[must_use]
    pub fn new(preset_id: impl Into<String>, available: Vec<String>) -> Self {
        let preset_id = preset_id.into();
        let available_display = if available.is_empty() {
            "none".to_owned()
        } else {
            available.join(", ")
        };
        Self {
            preset_id,
            available,
            available_display,
        }
    }
}

/// A known preset's composition cannot be installed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("agent-presets: preset \"{preset_id}\" failed to mount: {reason}")]
pub struct PresetMountError {
    /// Preset whose composition failed.
    pub preset_id: String,
    /// Failure without the package prefix.
    pub reason: String,
}

impl PresetMountError {
    /// Creates one stable mount refusal.
    #[must_use]
    pub fn new(preset_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            preset_id: preset_id.into(),
            reason: reason.into(),
        }
    }
}
