//! Root application boot transaction and post-settlement activation audit.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_cordis::{Context, Fiber, FiberState};
use seekdeep_loader::profile_patch::{
    ProfilePatch, apply_entry_patches_with_warning_sink, parse_entry_list_yaml,
    render_entry_list_yaml,
};
use seekdeep_loader::{Entry, EntryId, EntryParent, LoadedComposition, PluginCatalog};

/// Host preparation callback run before any configuration row mounts.
pub type BootPrepare = Arc<dyn Fn(Context) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Inputs for one transactional application boot.
#[derive(Clone, Default)]
pub struct BootOptions {
    /// Flattened overlays applied over the base file in declaration order.
    pub patches: Vec<ProfilePatch>,
    /// Host services installed before any config-tree entry mounts.
    pub prepare: Option<BootPrepare>,
    /// Sink for skipped-patch diagnostics.
    pub warn: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl std::fmt::Debug for BootOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootOptions")
            .field("patches", &self.patches)
            .field("has_prepare", &self.prepare.is_some())
            .field("has_warn", &self.warn.is_some())
            .finish()
    }
}

/// One loaded entry's stable activation facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivationEntry {
    /// Plugin name used in diagnostics.
    pub name: String,
    /// Current Cordis lifecycle state.
    pub state: FiberState,
    /// Required services absent from this entry's own context.
    pub missing_services: Vec<String>,
    /// Startup error retained by a failed fiber.
    pub error: Option<String>,
}

/// Successfully booted application context and its live composition.
pub struct BootedApplication {
    context: Context,
    composition: Option<LoadedComposition>,
}

impl std::fmt::Debug for BootedApplication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootedApplication")
            .field("fiber_state", &self.context.fiber().state())
            .field(
                "entry_count",
                &self
                    .composition
                    .as_ref()
                    .map_or(0, |composition| composition.fibers().len()),
            )
            .finish()
    }
}

impl BootedApplication {
    /// Boot context carrying every mounted service.
    #[must_use]
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Loaded composition, absent when the application disposed itself during startup.
    #[must_use]
    pub fn composition(&self) -> Option<&LoadedComposition> {
        self.composition.as_ref()
    }

    /// Disposes the complete application tree exactly once.
    ///
    /// # Errors
    ///
    /// Returns aggregated Cordis cleanup failures.
    pub async fn dispose(self) -> anyhow::Result<()> {
        self.context.fiber().dispose().await
    }
}

/// Captures activation state after Loader settlement.
#[must_use]
pub fn activation_entries(composition: &LoadedComposition) -> Vec<ActivationEntry> {
    composition
        .fibers()
        .iter()
        .map(|fiber| ActivationEntry {
            name: fiber.plugin_name().to_owned(),
            state: fiber.fiber().state(),
            missing_services: fiber
                .inject()
                .iter()
                .filter(|service| !fiber.context().has_named(service))
                .cloned()
                .collect(),
            error: fiber.error(),
        })
        .collect()
}

/// Rejects every enabled entry that did not reach active state.
///
/// # Errors
///
/// Returns a labelled aggregate retaining failures and pending dependencies.
pub fn assert_entries_activated(
    composition: &LoadedComposition,
    bin_name: &str,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for entry in activation_entries(composition) {
        match entry.state {
            FiberState::Active => {}
            FiberState::Failed => failures.push(format!(
                "{}: {}",
                entry.name,
                entry.error.as_deref().unwrap_or("plugin activation failed")
            )),
            FiberState::Pending => {
                let subject = if entry.missing_services.len() == 1 {
                    "service"
                } else {
                    "services"
                };
                let missing = if entry.missing_services.is_empty() {
                    "unknown".to_owned()
                } else {
                    entry.missing_services.join(", ")
                };
                failures.push(format!(
                    "{}: pending (waiting for {subject}: {missing})",
                    entry.name
                ));
            }
            state => failures.push(format!("{}: fiber state {state:?}", entry.name)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        let noun = if failures.len() == 1 {
            "entry"
        } else {
            "entries"
        };
        anyhow::bail!(
            "{bin_name}: {} {noun} did not activate\n{}",
            failures.len(),
            failures.join("\n")
        )
    }
}

fn render_boot_config(
    bin_name: &str,
    path: &std::path::Path,
    options: &BootOptions,
) -> anyhow::Result<String> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to read config {}: {error}",
            path.display()
        )
    })?;
    let base = parse_entry_list_yaml(&source).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to parse config {}: {error}",
            path.display()
        )
    })?;
    let warn = options.warn.clone();
    let entries = apply_entry_patches_with_warning_sink(&base, &options.patches, move |warning| {
        if let Some(warn) = &warn {
            warn(format!("{bin_name}: {warning}"));
        }
    })?;
    Ok(render_entry_list_yaml(&entries)?)
}

async fn dispose_preserving(context: &Context, primary: anyhow::Error) -> anyhow::Error {
    match context.fiber().dispose().await {
        Ok(()) => primary,
        Err(cleanup) => anyhow::anyhow!("{primary:#}\napp boot cleanup failed: {cleanup:#}"),
    }
}

/// Boots one compiled plugin catalog transactionally from a composed config file.
///
/// # Errors
///
/// Labels preparation separately from plugin-tree failures and disposes partial state.
pub async fn boot(
    bin_name: &str,
    absolute_config_path: &std::path::Path,
    catalog: &PluginCatalog,
    options: BootOptions,
) -> anyhow::Result<BootedApplication> {
    let raw = Context::new();
    let owner = Fiber::active_child("app boot");
    let context = raw.with_root_fiber(owner.clone());
    let composition = catalog
        .load_yaml_at(&context, "[]\n", absolute_config_path)
        .await
        .map_err(|error| anyhow::anyhow!("{bin_name}: host preparation failed: {error}"))?;
    if let Some(prepare) = &options.prepare
        && let Err(error) = prepare(context.clone()).await
    {
        let primary = anyhow::anyhow!("{bin_name}: host preparation failed: {error:#}");
        return Err(dispose_preserving(&context, primary).await);
    }
    if owner.state() == FiberState::Disposed {
        return Ok(BootedApplication {
            context,
            composition: None,
        });
    }
    if let Err(error) = render_boot_config(bin_name, absolute_config_path, &options) {
        let primary = anyhow::anyhow!("{bin_name}: plugin tree failed to load: {error:#}");
        return Err(dispose_preserving(&context, primary).await);
    }
    let include_id = match EntryId::new("include") {
        Ok(include_id) => include_id,
        Err(error) => {
            let primary = anyhow::anyhow!("{bin_name}: plugin tree failed to load: {error:#}");
            return Err(dispose_preserving(&context, primary).await);
        }
    };
    let include = match Entry::file_include(
        include_id,
        absolute_config_path.to_string_lossy(),
        options.patches.clone(),
    ) {
        Ok(include) => include,
        Err(error) => {
            let primary = anyhow::anyhow!("{bin_name}: plugin tree failed to load: {error:#}");
            return Err(dispose_preserving(&context, primary).await);
        }
    };
    if let Err(error) = composition
        .create_entry(include, EntryParent::Root, None)
        .await
    {
        if owner.state() == FiberState::Disposed {
            return Ok(BootedApplication {
                context,
                composition: None,
            });
        }
        let primary = anyhow::anyhow!("{bin_name}: plugin tree failed to load: {error}");
        return Err(dispose_preserving(&context, primary).await);
    }
    tokio::task::yield_now().await;
    if matches!(owner.state(), FiberState::Unloading | FiberState::Disposed) {
        return Ok(BootedApplication {
            context,
            composition: None,
        });
    }
    if let Err(error) = assert_entries_activated(&composition, bin_name) {
        let primary = anyhow::anyhow!("{bin_name}: plugin tree failed to load: {error:#}");
        return Err(dispose_preserving(&context, primary).await);
    }
    Ok(BootedApplication {
        context,
        composition: Some(composition),
    })
}
