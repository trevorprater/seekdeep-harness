//! Per-session Agent preset discovery, authoring, standing mounts, and durable identity.

pub mod authoring;
pub mod discovery;
pub mod invariant;
pub mod metadata;
pub mod mount;
pub mod preset;
pub mod session;

pub use authoring::{
    InvalidPresetIdError, PresetExistsError, PresetNotWritableError, copy_composition,
    delete_composition, read_composition, writable_root,
};
pub use discovery::{COMPOSITION_FILE, USER_PRESET_DIR, discover_presets, scan_root};
pub use invariant::register_invariant;
pub use metadata::{METADATA_FILE, PresetMetadata, read_preset_metadata, render_preset_metadata};
pub use mount::{
    AGENT_PRESETS, AgentPresetRegistry, AgentPresetRegistryConfig, SETTINGS_NAMESPACE,
};
pub use preset::{
    AgentPreset, AgentPresetConfig, PresetMountError, PresetRoot, PresetTrust, UnknownPresetError,
    valid_preset_id,
};
pub use session::resolve_session_preset;
