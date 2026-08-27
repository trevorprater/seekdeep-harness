//! Source-compatible profile layer composition and live configuration boot.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use indexmap::IndexMap;
use path_clean::PathClean as _;
use seekdeep_app_boot::{
    BootOptions, BootPrepare, BootUserPatchWatchOptions, BootedApplication, ConfigRefreshFailure,
    ConfigWatchRegistry, OwnedConfigWatcher, PROFILE_PATCH_FILENAME, PatchComposer, Profile, boot,
    compose_entries, load_optional_patches, load_overlay_patches, watch_boot_user_patches,
};
use seekdeep_cordis::FiberState;
use seekdeep_cordis_timer::TIMER;
use seekdeep_hmr::HMR;
use seekdeep_loader::{
    Entry, EntryId, EntryParent, LOADER, PluginCatalog, PluginSpecifier,
    profile_patch::{ProfileEntry, ProfileNode, ProfilePatch, ProfilePatchWarning},
};

use crate::profile_support;

const NAME: &str = "seekdeep";
const TELEMETRY_ROW_ID: &str = "session-telemetry-otel";
const AGENT_PRESETS_ROW_ID: &str = "agent-presets";
const FRAMEWORK_TIMER_ID: &str = "profile-watch-timer";
const FRAMEWORK_HMR_ID: &str = "profile-watch-hmr";

/// Privacy switch applied after every user-controlled profile layer.
pub const TELEMETRY_DISABLED_ENV: &str = "SEEKDEEP_TELEMETRY_DISABLED";

/// Fully resolved profile layer stack and its pre-launch row index.
#[derive(Clone, Debug)]
pub struct ProfileBootPlan {
    profile: Profile,
    bundle_patches: Vec<ProfilePatch>,
    home_patch_path: PathBuf,
    home_patches: Vec<ProfilePatch>,
    overlays: Vec<ProfilePatch>,
    rows: IndexMap<String, ProfileEntry>,
    warnings: Vec<ProfilePatchWarning>,
}

/// Booted profile plus its explicitly ordered live-config owners.
pub struct ProfileBootApplication {
    application: BootedApplication,
    watchers: Vec<OwnedConfigWatcher>,
}

impl std::fmt::Debug for ProfileBootApplication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProfileBootApplication")
            .field("application", &self.application)
            .field("watcher_count", &self.watchers.len())
            .finish()
    }
}

impl ProfileBootApplication {
    /// Root context carrying the mounted profile services.
    #[must_use]
    pub fn context(&self) -> &seekdeep_cordis::Context {
        self.application.context()
    }

    /// Loaded composition, absent when an app disposed itself during startup.
    #[must_use]
    pub fn composition(&self) -> Option<&seekdeep_loader::LoadedComposition> {
        self.application.composition()
    }

    /// Drains config watchers before disposing the Loader tree.
    ///
    /// # Errors
    ///
    /// Aggregates every watcher and application cleanup failure after attempting all of them.
    pub async fn dispose(self) -> anyhow::Result<()> {
        dispose_profile_parts(self.application, self.watchers).await
    }
}

impl ProfileBootPlan {
    /// Loaded profile identity, paths, bundle layers, and initial user layer.
    #[must_use]
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Home-wide patch file applied after the profile-owned user layer.
    #[must_use]
    pub fn home_patch_path(&self) -> &Path {
        &self.home_patch_path
    }

    /// Effective pre-launch row by exact profile id.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&ProfileEntry> {
        self.rows.get(id)
    }

    /// Skipped-patch diagnostics produced while indexing the launcher's own rows.
    #[must_use]
    pub fn warnings(&self) -> &[ProfilePatchWarning] {
        &self.warnings
    }

    /// Complete initial patch list in source application order.
    #[must_use]
    pub fn all_patches(&self) -> Vec<ProfilePatch> {
        self.bundle_patches
            .iter()
            .chain(&self.profile.patches)
            .chain(&self.home_patches)
            .chain(&self.overlays)
            .cloned()
            .collect()
    }

    fn live_patches(&self) -> anyhow::Result<Vec<ProfilePatch>> {
        let profile = load_optional_patches(NAME, &self.profile.patch_path)?.unwrap_or_default();
        let home = load_optional_patches(NAME, &self.home_patch_path)?.unwrap_or_default();
        Ok(self
            .bundle_patches
            .iter()
            .chain(&profile)
            .chain(&home)
            .chain(&self.overlays)
            .cloned()
            .collect())
    }
}

/// Resolves the privacy opt-out into the fixed telemetry-row patch.
///
/// Every non-empty value, including `0` and `false`, disables telemetry. A
/// profile without the telemetry row needs no patch.
#[must_use]
pub fn resolve_telemetry_patch(
    disabled_environment: Option<&str>,
    has_row: bool,
) -> Option<ProfilePatch> {
    if disabled_environment.unwrap_or_default().is_empty() || !has_row {
        return None;
    }
    Some(ProfilePatch::from_fields(IndexMap::from([
        (
            "id".to_owned(),
            ProfileNode::String(TELEMETRY_ROW_ID.to_owned()),
        ),
        ("disabled".to_owned(), ProfileNode::Bool(true)),
    ])))
}

fn agent_presets_patch(row: &ProfileEntry, shipped_root: &Path) -> ProfilePatch {
    let mut config = row
        .config()
        .and_then(ProfileNode::as_mapping)
        .cloned()
        .unwrap_or_default();
    config.insert(
        "roots".to_owned(),
        ProfileNode::Sequence(vec![ProfileNode::Mapping(IndexMap::from([
            (
                "path".to_owned(),
                ProfileNode::String(shipped_root.to_string_lossy().into_owned()),
            ),
            ("trust".to_owned(), ProfileNode::String("system".to_owned())),
        ]))]),
    );
    ProfilePatch::from_fields(IndexMap::from([
        (
            "id".to_owned(),
            ProfileNode::String(AGENT_PRESETS_ROW_ID.to_owned()),
        ),
        ("config".to_owned(), ProfileNode::Mapping(config)),
    ]))
}

/// Resolves and composes one profile without mounting its plugin tree.
///
/// # Errors
///
/// Returns profile, bundle, overlay, home-layer, or patch-composition failures.
#[allow(clippy::too_many_arguments)]
pub fn compose_profile_at(
    profile_name: &str,
    overlay_files: &[PathBuf],
    cwd: &Path,
    home: &Path,
    install_anchor: &Path,
    shipped_preset_root: &Path,
    telemetry_disabled: Option<&str>,
) -> anyhow::Result<ProfileBootPlan> {
    let profile = profile_support::prepare_profile_at(profile_name, true, home, install_anchor)?;
    let home_patch_path = home.join(PROFILE_PATCH_FILENAME);
    let home_patches = load_optional_patches(NAME, &home_patch_path)?.unwrap_or_default();
    let mut overlays = Vec::new();
    for filename in overlay_files {
        let absolute = if filename.is_absolute() {
            filename.clone()
        } else {
            cwd.join(filename)
        }
        .clean();
        overlays.extend(load_overlay_patches(NAME, &absolute)?);
    }
    let bundle_patches = profile
        .layers
        .iter()
        .flat_map(|layer| layer.patches.iter().cloned())
        .collect::<Vec<_>>();
    let indexed = compose_entries(&[
        bundle_patches.clone(),
        profile.patches.clone(),
        home_patches.clone(),
        overlays.clone(),
    ])?;
    let (entries, warnings) = indexed.into_parts();
    let rows = entries
        .into_iter()
        .filter_map(|entry| entry.id().map(|id| (id.as_str().to_owned(), entry)))
        .collect::<IndexMap<_, _>>();
    if let Some(row) = rows.get(AGENT_PRESETS_ROW_ID) {
        overlays.push(agent_presets_patch(row, shipped_preset_root));
    }
    if let Some(patch) =
        resolve_telemetry_patch(telemetry_disabled, rows.contains_key(TELEMETRY_ROW_ID))
    {
        overlays.push(patch);
    }
    Ok(ProfileBootPlan {
        profile,
        bundle_patches,
        home_patch_path,
        home_patches,
        overlays,
        rows,
        warnings,
    })
}

/// Registers the compiled framework plugins needed by profile boot and config HMR.
///
/// # Errors
///
/// Returns duplicate or invalid catalog-registration failures.
pub fn register_profile_framework_plugins(catalog: &PluginCatalog) -> anyhow::Result<()> {
    let timer = seekdeep_cordis_timer::plugin();
    catalog.register_named("cordis-plugin-timer", timer.clone())?;
    catalog.register_named("@seekdeep-ai/cordis-plugin-timer", timer)?;
    let restart: seekdeep_hmr::RestartHook = Arc::new(|| Box::pin(async { Ok(()) }));
    let hmr = seekdeep_hmr::plugin(restart);
    catalog.register_named("cordis-plugin-hmr", hmr.clone())?;
    catalog.register_named("@seekdeep-ai/cordis-plugin-hmr", hmr)?;
    Ok(())
}

fn unused_entry_id(
    base: &str,
    loader: &seekdeep_loader::LoaderSettlement,
) -> anyhow::Result<EntryId> {
    let entries = loader.entries()?;
    for suffix in 1_u64.. {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base}-{suffix}")
        };
        let id = EntryId::new(candidate)?;
        if entries.iter().all(|entry| entry.id != id) {
            return Ok(id);
        }
    }
    unreachable!("the integer suffix space is unbounded for practical profiles")
}

async fn ensure_config_hmr(context: &seekdeep_cordis::Context) -> anyhow::Result<()> {
    if context.get(HMR).is_some() {
        return Ok(());
    }
    let loader = context
        .get(LOADER)
        .ok_or_else(|| anyhow::anyhow!("seekdeep: profile watching requires the Cordis Loader"))?;
    if context.get(TIMER).is_none() {
        loader
            .create_entry(
                Entry::new(
                    unused_entry_id(FRAMEWORK_TIMER_ID, &loader)?,
                    PluginSpecifier::new("@seekdeep-ai/cordis-plugin-timer")?,
                ),
                EntryParent::Root,
                None,
            )
            .await?;
    }
    let mut hmr = Entry::new(
        unused_entry_id(FRAMEWORK_HMR_ID, &loader)?,
        PluginSpecifier::new("@seekdeep-ai/cordis-plugin-hmr")?,
    );
    hmr.config = serde_json::json!({"root": []});
    loader.create_entry(hmr, EntryParent::Root, None).await?;
    loader.wait().await?;
    anyhow::ensure!(
        context.get(HMR).is_some(),
        "seekdeep: watch-only HMR did not activate"
    );
    Ok(())
}

async fn dispose_after_setup_failure(
    application: BootedApplication,
    watchers: Vec<OwnedConfigWatcher>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match dispose_profile_parts(application, watchers).await {
        Ok(()) => primary,
        Err(cleanup) => anyhow::anyhow!("{primary:#}\nprofile setup cleanup failed: {cleanup:#}"),
    }
}

async fn dispose_profile_parts(
    application: BootedApplication,
    mut watchers: Vec<OwnedConfigWatcher>,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    while let Some(watcher) = watchers.pop() {
        if let Err(error) = watcher.dispose().await {
            failures.push(format!("profile watcher cleanup failed: {error:#}"));
        }
    }
    if let Err(error) = application.dispose().await {
        failures.push(format!("profile application cleanup failed: {error:#}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(failures.join("\n"))
    }
}

/// Boots a composed profile and installs last-good live watchers for both user layers.
///
/// # Errors
///
/// Returns profile-tree, fallback-HMR, watcher, activation, or rollback failures.
pub async fn boot_profile(
    plan: ProfileBootPlan,
    catalog: &PluginCatalog,
    prepare: Option<BootPrepare>,
) -> anyhow::Result<ProfileBootApplication> {
    let failure: ConfigRefreshFailure = Arc::new(|path, error| {
        eprintln!(
            "seekdeep: failed to refresh profile patches {}: {error:#}",
            path.display()
        );
    });
    boot_profile_with_failure_handler(plan, catalog, prepare, failure).await
}

/// Boots a profile with an explicit last-good refresh failure observer.
///
/// # Errors
///
/// Returns the same failures as [`boot_profile`].
pub async fn boot_profile_with_failure_handler(
    plan: ProfileBootPlan,
    catalog: &PluginCatalog,
    prepare: Option<BootPrepare>,
    failure: ConfigRefreshFailure,
) -> anyhow::Result<ProfileBootApplication> {
    let warning = Arc::new(|message: String| eprintln!("{message}"));
    let application = boot(
        NAME,
        &plan
            .profile
            .dir
            .join(profile_support::PROFILE_ROOT_FILENAME),
        catalog,
        BootOptions {
            patches: plan.all_patches(),
            prepare,
            warn: Some(warning),
        },
    )
    .await?;
    let context = application.context().clone();
    if application.composition().is_none()
        || context.fiber().state() != FiberState::Active
        || context.get(LOADER).is_none()
    {
        return Ok(ProfileBootApplication {
            application,
            watchers: Vec::new(),
        });
    }
    if let Err(error) = ensure_config_hmr(&context).await {
        return Err(dispose_after_setup_failure(application, Vec::new(), error).await);
    }
    let plan = Arc::new(plan);
    let compose: PatchComposer = Arc::new({
        let plan = plan.clone();
        move |_| plan.live_patches()
    });
    let registry = ConfigWatchRegistry::new();
    let mut watchers = Vec::new();
    for filename in [&plan.profile.patch_path, &plan.home_patch_path] {
        let result = watch_boot_user_patches(BootUserPatchWatchOptions {
            bin_name: NAME.to_owned(),
            filename: filename.clone(),
            compose: compose.clone(),
            context: context.clone(),
            registry: registry.clone(),
            failure: failure.clone(),
        })
        .await;
        match result {
            Ok(watcher) => watchers.push(watcher),
            Err(error) => {
                return Err(dispose_after_setup_failure(application, watchers, error).await);
            }
        }
    }
    Ok(ProfileBootApplication {
        application,
        watchers,
    })
}
