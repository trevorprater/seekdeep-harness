//! Per-session Agent preset discovery, authoring, standing mounts, and durable identity.

pub mod discovery;
pub mod metadata;
pub mod preset;
pub mod session;

pub use discovery::{COMPOSITION_FILE, USER_PRESET_DIR, discover_presets, scan_root};
pub use metadata::{METADATA_FILE, PresetMetadata, read_preset_metadata, render_preset_metadata};
pub use preset::{
    AgentPreset, AgentPresetConfig, PresetMountError, PresetRoot, PresetTrust, UnknownPresetError,
    valid_preset_id,
};
pub use session::resolve_session_preset;
