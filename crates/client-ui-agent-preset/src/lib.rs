//! Agent preset identity and management UI semantics.

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-agent-preset";
/// Settings namespace resolved at Session creation.
pub const AGENT_PRESET_SETTINGS_NS: &str = "agent-presets";

/// Preset trust boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetTrust {
    /// Deployment-shipped preset.
    System,
    /// Locally authored preset.
    User,
}

/// Roster fields used to resolve display copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetDisplaySource {
    /// Stable preset id/directory name.
    pub id: String,
    /// Trust boundary.
    pub trust: PresetTrust,
    /// Optional file-authored name.
    pub name: Option<String>,
    /// Optional file-authored description.
    pub description: Option<String>,
}

/// Built-in locale-key pair or file-authored copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetDisplayText {
    /// Known shipped preset, localized by caller.
    BuiltIn {
        /// Name locale key.
        name_key: &'static str,
        /// Description locale key.
        description_key: &'static str,
    },
    /// User/unknown-system metadata, never translated.
    File {
        /// Name or id fallback.
        name: String,
        /// Optional description.
        description: Option<String>,
    },
}

/// Resolves known shipped presets to locale keys and preserves every other row's file copy.
#[must_use]
pub fn preset_display_text(preset: &PresetDisplaySource) -> PresetDisplayText {
    let keys = if preset.trust == PresetTrust::System {
        match preset.id.as_str() {
            "standard" => Some(("presetStandardName", "presetStandardDescription")),
            "code" => Some(("presetCodeName", "presetCodeDescription")),
            "minimal" => Some(("presetMinimalName", "presetMinimalDescription")),
            "cordis" => Some(("presetCordisName", "presetCordisDescription")),
            _ => None,
        }
    } else {
        None
    };
    keys.map_or_else(
        || PresetDisplayText::File {
            name: preset.name.clone().unwrap_or_else(|| preset.id.clone()),
            description: preset.description.clone(),
        },
        |(name_key, description_key)| PresetDisplayText::BuiltIn {
            name_key,
            description_key,
        },
    )
}

/// One selectable roster option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPresetOption {
    /// Preset id.
    pub id: String,
    /// Trust boundary.
    pub trust: PresetTrust,
    /// Optional name.
    pub name: Option<String>,
    /// Optional description.
    pub description: Option<String>,
}

/// One exact Host roster row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterPreset {
    /// Preset id.
    pub id: String,
    /// Trust boundary.
    pub trust: PresetTrust,
    /// Optional name.
    pub name: Option<String>,
    /// Optional description.
    pub description: Option<String>,
    /// Load failure, absent when selectable.
    pub broken: Option<String>,
}

/// Filters broken roster rows while preserving order and exact optional fields.
#[must_use]
pub fn preset_options(presets: &[RosterPreset]) -> Vec<AgentPresetOption> {
    presets
        .iter()
        .filter(|preset| preset.broken.is_none())
        .map(|preset| AgentPresetOption {
            id: preset.id.clone(),
            trust: preset.trust,
            name: preset.name.clone(),
            description: preset.description.clone(),
        })
        .collect()
}

/// Open duplicate-preset draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyDraft {
    /// Immutable source preset id.
    pub source_id: String,
    /// New preset id.
    pub id: String,
    /// Optional display name.
    pub name: String,
}

/// Client-side duplicate dialog blocker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftBlocker {
    /// No id supplied.
    IdRequired,
    /// Id violates lowercase/digit/hyphen policy.
    IdInvalid,
    /// Id already exists.
    IdTaken,
}

fn valid_preset_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && id
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// Returns why a copy draft cannot be submitted, or `None` when valid.
#[must_use]
pub fn draft_blocker(draft: &CopyDraft, rows: &[RosterPreset]) -> Option<DraftBlocker> {
    if draft.id.is_empty() {
        Some(DraftBlocker::IdRequired)
    } else if !valid_preset_id(&draft.id) {
        Some(DraftBlocker::IdInvalid)
    } else if rows.iter().any(|row| row.id == draft.id) {
        Some(DraftBlocker::IdTaken)
    } else {
        None
    }
}

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
