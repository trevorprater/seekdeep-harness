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

/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "agent-presets";
/// The roster mounts standing compositions through the active Loader generation.
pub const PLUGIN_INJECT: &[&str] = &["loader"];

/// Builds the Loader-compatible Agent preset roster plugin over the product catalog.
#[must_use]
pub fn plugin(catalog: seekdeep_loader::PluginCatalog) -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        move |context, config| {
            let catalog = catalog.clone();
            Box::pin(async move {
                anyhow::ensure!(
                    context.get(seekdeep_loader::LOADER).is_some(),
                    "agent-presets requires loader"
                );
                let roster = serde_json::from_value(config)?;
                let registry = AgentPresetRegistry::new(
                    &context,
                    catalog,
                    AgentPresetRegistryConfig {
                        roster,
                        user_root: None,
                    },
                )?;
                registry.provide(&context)?;
                Ok(())
            })
        },
    )
}
