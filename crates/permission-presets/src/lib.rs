//! User-facing permission presets over the independent sandbox-mode and
//! approval-policy knobs.

pub mod client;
pub mod index;
pub mod invariant;
pub mod types;

pub use index::{
    CUSTOM_PRESET, Config, KnobState, PERMISSION_PRESETS, PermissionPresetService,
    PermissionSettings, PresetSpec, apply, apply_knob_event, config_schema,
    effective_permission_preset, plugin,
};
pub use types::{PermissionSelect, PresetOption};
