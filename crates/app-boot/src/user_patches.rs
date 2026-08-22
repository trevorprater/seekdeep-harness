//! User patch-layer watch composition over transactional reload.

use std::{path::PathBuf, sync::Arc};

use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_loader::{EntryId, LOADER, profile_patch::ProfilePatch};

use crate::{
    ConfigDumpLayer, ConfigRefresh, ConfigRefreshFailure, ConfigWatchRegistry,
    RegisteredConfigWatcher, ReloadableComposition, load_optional_patches, render_config_dump,
};

/// Builds the complete patch list for one fresh user-layer generation.
pub type PatchComposer = Arc<dyn Fn(Vec<ProfilePatch>) -> Vec<ProfilePatch> + Send + Sync>;

/// Inputs for live user patch reconciliation.
pub struct UserPatchWatchOptions {
    /// Diagnostic prefix.
    pub bin_name: String,
    /// Base configuration file whose entries receive patches.
    pub base_config: PathBuf,
    /// Exact optional user patch file.
    pub filename: PathBuf,
    /// Fresh-generation patch composition; identity when omitted by callers.
    pub compose: PatchComposer,
    /// Current whole-tree generation owner.
    pub reload: Arc<ReloadableComposition>,
    /// Canonical duplicate-path registry.
    pub registry: Arc<ConfigWatchRegistry>,
    /// Contained parse/activation/watch failure observer.
    pub failure: ConfigRefreshFailure,
}

/// Inputs for the root file carrier installed by [`crate::boot`].
pub struct BootUserPatchWatchOptions {
    /// Diagnostic prefix.
    pub bin_name: String,
    /// Exact optional user patch file.
    pub filename: PathBuf,
    /// Fresh-generation patch composition; identity when omitted by callers.
    pub compose: PatchComposer,
    /// Booted root context carrying the live Loader service.
    pub context: Context,
    /// Canonical duplicate-path registry.
    pub registry: Arc<ConfigWatchRegistry>,
    /// Contained parse/activation/watch failure observer.
    pub failure: ConfigRefreshFailure,
}

impl std::fmt::Debug for BootUserPatchWatchOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootUserPatchWatchOptions")
            .field("bin_name", &self.bin_name)
            .field("filename", &self.filename)
            .finish_non_exhaustive()
    }
}

/// Root-owned user-patch watcher with an explicit joined disposer.
pub struct OwnedConfigWatcher {
    effect: EffectHandle,
}

impl std::fmt::Debug for OwnedConfigWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedConfigWatcher")
            .finish_non_exhaustive()
    }
}

impl OwnedConfigWatcher {
    /// Stops observation and waits for every admitted refresh.
    ///
    /// # Errors
    ///
    /// Returns watcher worker or refresh-drain failures.
    pub async fn dispose(self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

impl std::fmt::Debug for UserPatchWatchOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserPatchWatchOptions")
            .field("bin_name", &self.bin_name)
            .field("base_config", &self.base_config)
            .field("filename", &self.filename)
            .finish_non_exhaustive()
    }
}

/// Watches and transactionally reapplies one optional user patch layer.
///
/// # Errors
///
/// Returns canonicalization, duplicate registration, or watcher setup failures.
pub fn watch_user_patches(
    options: UserPatchWatchOptions,
) -> anyhow::Result<RegisteredConfigWatcher> {
    let bin_name = options.bin_name.clone();
    let base_config = options.base_config.clone();
    let filename = options.filename.clone();
    let compose = options.compose.clone();
    let reload = options.reload.clone();
    let refresh: ConfigRefresh = Arc::new(move || {
        let bin_name = bin_name.clone();
        let base_config = base_config.clone();
        let filename = filename.clone();
        let compose = compose.clone();
        let reload = reload.clone();
        Box::pin(async move {
            let user = load_optional_patches(&bin_name, &filename)?.unwrap_or_default();
            let patches = compose(user);
            let rendered = render_config_dump(
                &bin_name,
                &base_config,
                &[ConfigDumpLayer {
                    label: filename.display().to_string(),
                    patches,
                }],
                |_| {},
            )?;
            reload.replace(rendered).await
        })
    });
    options
        .registry
        .register(&options.filename, refresh, options.failure)
}

/// Watches the optional user layer attached to app boot's root file carrier.
///
/// # Errors
///
/// Returns a missing Loader, invalid fixed id, canonicalization, duplicate
/// registration, or watcher setup failure. Refresh failures are reported to
/// `failure`; the watcher remains active on the last-good generation.
pub async fn watch_boot_user_patches(
    options: BootUserPatchWatchOptions,
) -> anyhow::Result<OwnedConfigWatcher> {
    let loader = options.context.get(LOADER).ok_or_else(|| {
        anyhow::anyhow!(
            "{}: user patch-layer watching requires the Cordis Loader service",
            options.bin_name
        )
    })?;
    let include = EntryId::new("include")?;
    if !loader
        .entries()?
        .iter()
        .any(|entry| entry.id == include && entry.plugin.as_str() == "cordis:include")
    {
        anyhow::bail!(
            "{}: user patch-layer watching requires the root Include entry",
            options.bin_name
        );
    }
    let bin_name = options.bin_name.clone();
    let filename = options.filename.clone();
    let compose = options.compose.clone();
    let refresh: ConfigRefresh = Arc::new(move || {
        let bin_name = bin_name.clone();
        let filename = filename.clone();
        let compose = compose.clone();
        let loader = loader.clone();
        let include = include.clone();
        Box::pin(async move {
            let user = load_optional_patches(&bin_name, &filename)?.unwrap_or_default();
            loader
                .update_include_patches(&include, compose(user))
                .await?;
            Ok(())
        })
    });
    let watcher = options
        .registry
        .register(&options.filename, refresh, options.failure)?;
    let state = Arc::new(tokio::sync::Mutex::new(Some(watcher)));
    let disposal_state = state.clone();
    let effect = EffectHandle::new("app-boot user patch watcher", move || {
        let state = disposal_state.clone();
        Box::pin(async move {
            if let Some(watcher) = state.lock().await.take() {
                watcher.dispose().await?;
            }
            Ok(())
        })
    });
    if let Ok(effect) = options.context.own(effect.clone()) {
        Ok(OwnedConfigWatcher { effect })
    } else {
        if let Some(watcher) = state.lock().await.take() {
            watcher.dispose().await?;
        }
        Ok(OwnedConfigWatcher {
            effect: EffectHandle::synchronous("disposed user patch watcher", || Ok(())),
        })
    }
}
