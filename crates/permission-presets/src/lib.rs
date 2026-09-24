//! User-facing permission presets over the independent sandbox-mode and
//! approval-policy knobs.

pub mod client;
pub mod index;
pub mod invariant;
pub mod types;

pub use client::{FULL_ACCESS_PRESET, display_permission_preset, display_preset_name};
pub use index::{
    CUSTOM_PRESET, Config, KnobState, PERMISSION_PRESETS, PERMISSION_SETTINGS_NAMESPACE,
    PermissionPresetService, PermissionSettings, PresetSpec, apply, apply_knob_event,
    config_schema, effective_permission_preset, plugin,
};
pub use types::{PermissionSelect, PresetOption};
