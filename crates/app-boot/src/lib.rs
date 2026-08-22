//! Profile discovery, initialization, and patch-layer composition.

pub mod boot;
pub mod config_dump;
pub mod fail_loud;
pub mod profile;
pub mod reload;
pub mod source_section;
pub mod user_patches;
pub mod watch;

pub use boot::{
    ActivationEntry, BootOptions, BootPrepare, BootedApplication, activation_entries,
    assert_entries_activated, boot,
};

pub use config_dump::{
    ConfigDumpLayer, load_overlay_patches, render_config_dump, render_config_dump_stderr,
    resolve_config_path,
};

pub use fail_loud::{
    FAIL_LOUD_RELEASE_TIMEOUT, FailLoudController, FailLoudProcess, FailLoudRelease, FailLoudTimer,
    TokioFailLoudTimer,
};

pub use profile::{
    DEFAULT_PROFILE_BUNDLES, LoadProfileOptions, PROFILE_PATCH_FILENAME, PROFILE_TEMPLATES,
    PROFILES_DIR, Profile, ProfileLayer, ProfileManifest, ProfileName, SeekDeepBundleManifest,
    SeekDeepManifestSection, SeekDeepProfileManifest, compose_entries,
    heal_profiles_module_fallback, init_profile, load_optional_patches, load_profile,
    normalize_shipped_profile, read_profile_manifest, resolve_bundle_dir, resolve_profile_dir,
    write_profile_manifest,
};
pub use reload::ReloadableComposition;
pub use source_section::{HARNESS_SOURCE_SECTION, add_harness_source_section};
pub use user_patches::{
    BootUserPatchWatchOptions, OwnedConfigWatcher, PatchComposer, UserPatchWatchOptions,
    watch_boot_user_patches, watch_user_patches,
};
pub use watch::{ConfigRefresh, ConfigRefreshFailure, ExactConfigWatcher, canonical_watch_key};
pub use watch::{ConfigWatchRegistry, RegisteredConfigWatcher};
