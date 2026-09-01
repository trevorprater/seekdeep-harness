//! Declarative plugin-tree parsing, patch layering, and executable loading.

mod expression;
mod javascript_plugin;
/// Generic Cordis file-launcher lifecycle.
pub mod launcher;
/// Source-compatible profile patch parsing and ordered composition.
pub mod profile_patch;
mod sandbox_service;

pub use expression::ExpressionEnvironment;
pub use javascript_plugin::{DynamicHostGuardFailure, DynamicHostRuntime, LoadedDynamicHostPlugin};
pub use sandbox_service::{
    SANDBOX_SERVICES, SandboxServiceDispatcher, SandboxServiceMethod, SandboxServiceRegistration,
    SandboxServiceRegistry, TokioSandboxServiceDispatcher,
};

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use indexmap::IndexMap;
use parking_lot::RwLock;
use seekdeep_cordis::{Context, Plugin, PluginFiber, ServiceKey, fiber::EffectHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Compiles one model-authored Host async-function body into a guarded
/// Rust-owned JavaScript plugin worker.
///
/// # Errors
///
/// Returns syntax, load, timeout, or plugin-shape failures before activation.
pub fn compile_dynamic_host_plugin(body: &str, timeout_ms: u64) -> anyhow::Result<Plugin> {
    javascript_plugin::load_body(body, timeout_ms)
}

/// Compiles a model-authored Host body together with its exact-run invocation bridge.
///
/// # Errors
///
/// Returns syntax, load, timeout, plugin-shape, or handler-registration failures.
pub fn compile_dynamic_host_runtime(
    body: &str,
    timeout_ms: u64,
) -> anyhow::Result<LoadedDynamicHostPlugin> {
    javascript_plugin::load_body_runtime(body, timeout_ms)
}

/// Compiles a model-authored Host body with its package label for tagged logging.
///
/// # Errors
///
/// Returns syntax, load, timeout, plugin-shape, or handler-registration failures.
pub fn compile_dynamic_host_runtime_named(
    body: &str,
    timeout_ms: u64,
    label: &str,
) -> anyhow::Result<LoadedDynamicHostPlugin> {
    javascript_plugin::load_body_runtime_named(body, timeout_ms, label)
}

impl LoadedDynamicHostPlugin {
    /// Returns the Cordis plugin entrypoint.
    #[must_use]
    pub fn plugin(&self) -> &Plugin {
        &self.plugin
    }

    /// Splits the Cordis plugin from its exact interpreter invocation bridge.
    #[must_use]
    pub fn into_parts(self) -> (Plugin, Arc<DynamicHostRuntime>) {
        (self.plugin, self.runtime)
    }
}

/// Loader-generation service inherited by every entry mounted in one composition.
pub const LOADER: ServiceKey<LoaderSettlement> = ServiceKey::new("loader");

type SettlementOutcome = Option<Result<(), Arc<str>>>;

/// Exact-generation whole-composition settlement barrier.
#[derive(Clone, Debug)]
pub struct LoaderSettlement {
    sender: tokio::sync::watch::Sender<SettlementOutcome>,
    result: tokio::sync::watch::Receiver<SettlementOutcome>,
    generation: Arc<AtomicU64>,
    runtime: Arc<RwLock<Option<std::sync::Weak<CompositionRuntime>>>>,
}

impl LoaderSettlement {
    fn pending() -> (Arc<Self>, LoaderSettlementCompletion) {
        let (sender, result) = tokio::sync::watch::channel(None);
        let settlement = Arc::new(Self {
            sender,
            result,
            generation: Arc::new(AtomicU64::new(0)),
            runtime: Arc::new(RwLock::new(None)),
        });
        let completion = LoaderSettlementCompletion {
            settlement: Arc::downgrade(&settlement),
            generation: 0,
            finished: false,
        };
        (settlement, completion)
    }

    /// Waits for every entry in this exact composition generation to settle.
    ///
    /// # Errors
    ///
    /// Returns the generation's startup/rollback failure or an abandoned-barrier error.
    pub async fn wait(&self) -> anyhow::Result<()> {
        let mut result = self.result.clone();
        loop {
            if let Some(outcome) = result.borrow().clone() {
                return outcome.map_err(|message| anyhow::anyhow!(message.to_string()));
            }
            result.changed().await.map_err(|_| {
                anyhow::anyhow!("loader composition ended without publishing settlement")
            })?;
        }
    }

    fn begin(self: &Arc<Self>) -> LoaderSettlementCompletion {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.sender.send_replace(None);
        LoaderSettlementCompletion {
            settlement: Arc::downgrade(self),
            generation,
            finished: false,
        }
    }

    fn attach(&self, runtime: &Arc<CompositionRuntime>) {
        *self.runtime.write() = Some(Arc::downgrade(runtime));
    }

    fn attached(&self) -> Result<Arc<CompositionRuntime>, LoaderError> {
        self.runtime
            .read()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or(LoaderError::Unavailable)
    }

    /// Snapshots every configured entry in configuration preorder.
    ///
    /// # Errors
    ///
    /// Returns an unavailable error before initial attachment or after disposal.
    pub fn entries(&self) -> Result<Vec<LoaderEntrySnapshot>, LoaderError> {
        Ok(self.attached()?.all_entry_snapshots())
    }

    /// Mounts one in-memory root entry owned by a running plugin effect.
    ///
    /// Unlike declarative tree mutation, this operation may run while the
    /// caller's own declarative generation is activating. The entry remains
    /// visible in ordinary Loader inventory and is disposed with the Loader.
    ///
    /// # Errors
    ///
    /// Returns duplicate id, import, activation, or disposal failures.
    pub async fn create_programmatic_entry(&self, entry: Entry) -> Result<(), LoaderError> {
        self.attached()?.create_programmatic_entry(entry).await
    }

    /// Removes one exact programmatic entry when it still exists.
    ///
    /// # Errors
    ///
    /// Returns lifecycle teardown failures. Absence is a successful `false`,
    /// closing the source store-check race without string matching.
    pub async fn remove_programmatic_entry_if_present(
        &self,
        id: &EntryId,
    ) -> Result<bool, LoaderError> {
        self.attached()?
            .remove_programmatic_entry_if_present(id)
            .await
    }

    /// Creates one live Loader entry.
    ///
    /// # Errors
    ///
    /// Returns parent, import, activation, disposal, or rollback failures.
    pub async fn create_entry(
        &self,
        entry: Entry,
        parent: EntryParent,
        position: Option<usize>,
    ) -> Result<(), LoaderError> {
        self.attached()?.create_entry(entry, parent, position).await
    }

    /// Updates and optionally moves one live Loader entry.
    ///
    /// # Errors
    ///
    /// Returns lookup, import, activation, disposal, or rollback failures.
    pub async fn update_entry(
        &self,
        id: &EntryId,
        update: EntryUpdate,
        parent: EntryParent,
        position: Option<usize>,
    ) -> Result<(), LoaderError> {
        self.attached()?
            .update_entry(id, update, parent, position)
            .await
    }

    /// Removes one live Loader entry and its descendants.
    ///
    /// # Errors
    ///
    /// Returns lookup, disposal, or rollback failures.
    pub async fn remove_entry(&self, id: &EntryId) -> Result<(), LoaderError> {
        self.attached()?.remove_entry(id).await
    }

    /// Re-reads every file-backed include and transactionally reconciles
    /// changed subtrees.
    ///
    /// # Errors
    ///
    /// Returns read, parse, validation, application, disposal, or rollback failures.
    pub async fn refresh_includes(&self) -> Result<(), LoaderError> {
        self.attached()?.refresh_includes().await
    }

    /// Refreshes the file carrier owning one exact path.
    ///
    /// Returns `false` without changing the tree when no Include owns `path`.
    ///
    /// # Errors
    ///
    /// Returns read, parse, validation, application, disposal, or rollback failures.
    pub async fn refresh_include_path(&self, path: impl AsRef<Path>) -> Result<bool, LoaderError> {
        self.attached()?.refresh_include_path(path.as_ref()).await
    }

    /// Replaces one file carrier's patch list and reconciles its subtree.
    ///
    /// # Errors
    ///
    /// Returns lookup, carrier-shape, read, parse, application, or rollback failures.
    pub async fn update_include_patches(
        &self,
        id: &EntryId,
        patches: Vec<profile_patch::ProfilePatch>,
    ) -> Result<(), LoaderError> {
        self.attached()?.update_include_patches(id, patches).await
    }

    /// Classifies and applies one Host module change.
    ///
    /// # Errors
    ///
    /// Returns candidate import, disposal, application, or rollback failures.
    pub async fn reload_module(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<HostHmrOutcome, LoaderError> {
        self.attached()?.reload_module(path.as_ref()).await
    }
}

/// One Loader entry inventory row.
#[derive(Clone, Debug, PartialEq)]
pub struct LoaderEntrySnapshot {
    /// Stable entry id.
    pub id: EntryId,
    /// Configured plugin specifier.
    pub plugin: PluginSpecifier,
    /// Exact raw configuration, including expression nodes.
    pub config: Value,
    /// Whether this entry is an internal group carrier.
    pub group: bool,
    /// Effective disabled state including ancestor groups.
    pub disabled: bool,
    /// Current lifecycle state; absent for disabled rows and internal groups.
    pub state: Option<seekdeep_cordis::FiberState>,
}

/// Result of one Host module-change classification and replacement attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostHmrOutcome {
    /// The changed file belongs to the launcher/framework dependency set.
    FullRestart,
    /// No loaded compatibility plugin depends on the changed file.
    Untracked,
    /// These configured entries committed a replacement generation.
    Reloaded(Vec<EntryId>),
}

/// Payload published after one Host HMR transaction commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostHmrReload {
    /// Canonical changed file that triggered the transaction.
    pub changed: PathBuf,
    /// Configured entries replaced in declaration order.
    pub entries: Vec<EntryId>,
}

struct LoaderSettlementCompletion {
    settlement: std::sync::Weak<LoaderSettlement>,
    generation: u64,
    finished: bool,
}

impl LoaderSettlementCompletion {
    fn finish(mut self, outcome: Result<(), Arc<str>>) {
        self.finished = true;
        if let Some(settlement) = self.settlement.upgrade()
            && settlement.generation.load(Ordering::Acquire) == self.generation
        {
            settlement.sender.send_replace(Some(outcome));
        }
    }
}

impl Drop for LoaderSettlementCompletion {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(settlement) = self.settlement.upgrade()
            && settlement.generation.load(Ordering::Acquire) == self.generation
        {
            settlement.sender.send_replace(Some(Err(Arc::from(
                "loader composition ended without publishing settlement",
            ))));
        }
    }
}

/// Stable declarative entry identifier used by overlay patches.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EntryId(String);

impl EntryId {
    /// Constructs a non-empty identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or whitespace-only identifiers.
    pub fn new(value: impl Into<String>) -> Result<Self, LoaderError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(LoaderError::InvalidEntryId);
        }
        Ok(Self(value))
    }

    /// Borrowed wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<'de> Deserialize<'de> for EntryId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Plugin module/catalog specifier from a declarative entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PluginSpecifier(String);

impl PluginSpecifier {
    /// Constructs a non-empty specifier.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or whitespace-only specifiers.
    pub fn new(value: impl Into<String>) -> Result<Self, LoaderError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(LoaderError::InvalidPluginSpecifier);
        }
        Ok(Self(value))
    }

    /// Borrowed wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginSpecifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<'de> Deserialize<'de> for PluginSpecifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn empty_config() -> Value {
    Value::Object(Map::new())
}

/// One declarative plugin row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Stable row identifier used by overlay patches.
    #[serde(default = "generated_entry_id")]
    pub id: EntryId,
    /// Rust plugin registry key. Source `cordis.yml` calls this field `name`.
    #[serde(rename = "name", alias = "plugin")]
    pub plugin: PluginSpecifier,
    /// Plugin configuration.
    #[serde(default = "empty_config")]
    pub config: Value,
    /// Whether this row is inactive.
    #[serde(default)]
    pub disabled: bool,
    /// Additional services required by this configured row.
    #[serde(default)]
    pub inject: Vec<String>,
    /// Whether this row is a source Loader group carrier.
    #[serde(default)]
    pub group: bool,
    /// Nested rows mounted in the plugin's lifecycle scope.
    #[serde(default)]
    pub children: Vec<Entry>,
    #[serde(skip)]
    disabled_expression: Option<String>,
    #[serde(skip)]
    isolate: IndexMap<String, Option<String>>,
    #[serde(skip)]
    intercept: IndexMap<String, Value>,
    #[serde(skip)]
    include: Option<IncludeOptions>,
}

impl Entry {
    /// Constructs one literal runtime entry with empty configuration.
    #[must_use]
    pub fn new(id: EntryId, plugin: PluginSpecifier) -> Self {
        Self {
            id,
            plugin,
            config: empty_config(),
            disabled: false,
            inject: Vec::new(),
            group: false,
            children: Vec::new(),
            disabled_expression: None,
            isolate: IndexMap::new(),
            intercept: IndexMap::new(),
            include: None,
        }
    }

    /// Constructs the internal file-backed tree carrier used by app boot.
    ///
    /// # Errors
    ///
    /// Returns when a patch value cannot cross the Loader JSON boundary.
    pub fn file_include(
        id: EntryId,
        path: impl Into<String>,
        patches: Vec<profile_patch::ProfilePatch>,
    ) -> Result<Self, LoaderError> {
        let path = path.into();
        let patch_nodes = patches
            .iter()
            .map(|patch| profile_patch::ProfileNode::Mapping(patch.fields().clone()))
            .collect::<Vec<_>>();
        let mut config = IndexMap::from([(
            "path".to_owned(),
            profile_patch::ProfileNode::String(path.clone()),
        )]);
        if !patch_nodes.is_empty() {
            config.insert(
                "patches".to_owned(),
                profile_patch::ProfileNode::Sequence(patch_nodes),
            );
        }
        let config_node = profile_patch::ProfileNode::Mapping(config);
        let mut entry = Self::new(id, PluginSpecifier::new("cordis:include")?);
        entry.config = expression::profile_node_to_raw_json(&config_node)?;
        entry.include = Some(IncludeOptions {
            path,
            patches,
            initial: None,
            base_url: None,
            resolved_path: None,
        });
        Ok(entry)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct IncludeOptions {
    path: String,
    patches: Vec<profile_patch::ProfilePatch>,
    initial: Option<Vec<profile_patch::ProfileEntry>>,
    base_url: Option<String>,
    resolved_path: Option<PathBuf>,
}

/// Partial programmatic update for one Loader entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryUpdate {
    /// Replacement plugin specifier.
    pub plugin: Option<PluginSpecifier>,
    /// Replacement raw configuration.
    pub config: Option<Value>,
    /// Replacement literal disabled state.
    pub disabled: Option<bool>,
    /// Replacement entry-local dependencies.
    pub inject: Option<Vec<String>>,
    /// Replacement group marker.
    pub group: Option<bool>,
    /// Replacement literal descendants.
    pub children: Option<Vec<Entry>>,
}

impl EntryUpdate {
    fn apply(self, entry: &mut Entry) {
        if let Some(plugin) = self.plugin {
            entry.plugin = plugin;
        }
        if let Some(config) = self.config {
            entry.config = config;
        }
        if let Some(disabled) = self.disabled {
            entry.disabled = disabled;
            entry.disabled_expression = None;
        }
        if let Some(inject) = self.inject {
            entry.inject = inject;
        }
        if let Some(group) = self.group {
            entry.group = group;
        }
        if let Some(children) = self.children {
            entry.children = children;
        }
    }
}

fn replace_include_patches(
    entry: &mut Entry,
    patches: Vec<profile_patch::ProfilePatch>,
) -> Result<(), LoaderError> {
    let patch_nodes = patches
        .iter()
        .map(|patch| profile_patch::ProfileNode::Mapping(patch.fields().clone()))
        .collect::<Vec<_>>();
    let patches_empty = patches.is_empty();
    let include = entry.include.as_mut().ok_or_else(|| {
        LoaderError::InvalidDocument(format!("entry {} is not a file include", entry.id))
    })?;
    include.patches = patches;
    let raw =
        expression::profile_node_to_raw_json(&profile_patch::ProfileNode::Sequence(patch_nodes))?;
    let config = entry.config.as_object_mut().ok_or_else(|| {
        LoaderError::InvalidDocument(format!(
            "entry {} include config is not a mapping",
            entry.id
        ))
    })?;
    if patches_empty {
        config.remove("patches");
    } else {
        config.insert("patches".to_owned(), raw);
    }
    Ok(())
}

/// Destination selection for a programmatic Loader entry update.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum EntryParent {
    /// Retain the current parent and position.
    #[default]
    Keep,
    /// Move to the composition root.
    Root,
    /// Move beneath the named group entry.
    Group(EntryId),
}

fn generated_entry_id() -> EntryId {
    static NEXT_FALLBACK_ID: AtomicU64 = AtomicU64::new(0);
    EntryId(format!(
        "{:08x}",
        NEXT_FALLBACK_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A full ordered configuration tree.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigTree {
    /// Top-level rows.
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// Whole-row patch indexed by row id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    /// Replacements and insertions in declaration order.
    #[serde(flatten)]
    pub rows: IndexMap<EntryId, Entry>,
}

impl ConfigTree {
    /// Applies a patch using the source harness's whole-row replacement rule.
    pub fn apply_patch(&mut self, patch: Patch) {
        for (id, mut replacement) in patch.rows {
            replacement.id.clone_from(&id);
            if !replace_entry(&mut self.entries, &id, &replacement) {
                self.entries.push(replacement);
            }
        }
    }
}

fn replace_entry(entries: &mut [Entry], id: &EntryId, replacement: &Entry) -> bool {
    for entry in entries {
        if &entry.id == id {
            entry.clone_from(replacement);
            return true;
        }
        if replace_entry(&mut entry.children, id, replacement) {
            return true;
        }
    }
    false
}

/// Declarative loader failure.
#[derive(Debug, Error)]
pub enum LoaderError {
    /// Entry identifiers cannot be empty.
    #[error("loader: entry id must not be empty")]
    InvalidEntryId,
    /// Plugin specifiers cannot be empty.
    #[error("loader: plugin name must not be empty")]
    InvalidPluginSpecifier,
    /// A live Loader operation raced initial attachment or final disposal.
    #[error("loader service is not attached to a live composition")]
    Unavailable,
    /// Another Loader operation currently owns the mutable tree.
    #[error("loader tree update is already in progress")]
    UpdateInProgress,
    /// A catalog key was registered twice.
    #[error("loader: plugin {0:?} is already registered")]
    DuplicatePlugin(String),
    /// An entry named a plugin absent from the catalog.
    #[error("loader: plugin {0:?} is not registered")]
    UnknownPlugin(String),
    /// A file-backed compatibility plugin failed to import.
    #[error("{0}")]
    ModuleLoad(String),
    /// A document could not be parsed.
    #[error("loader: invalid composition document: {0}")]
    InvalidDocument(String),
    /// The isolated settlement service could not be published.
    #[error("loader: settlement service failed: {0}")]
    SettlementService(String),
    /// A configured plugin could not be resolved before replacement.
    #[error("failed to import loader entry {entry} ({plugin}): {message}")]
    PluginImport {
        /// Stable entry id.
        entry: String,
        /// Plugin specifier.
        plugin: String,
        /// Import/catalog failure detail.
        message: String,
    },
    /// A mounted plugin failed to settle.
    #[error("failed to apply loader entry {entry} ({plugin}): {message}")]
    PluginStartup {
        /// Entry id.
        entry: String,
        /// Plugin specifier.
        plugin: String,
        /// Rendered causal failure.
        message: String,
    },
    /// Rollback or explicit composition disposal failed.
    #[error("loader: composition disposal failed: {0}")]
    Disposal(String),
}

/// Process-local mapping from declarative names to compiled Rust plugins.
#[derive(Clone)]
pub struct PluginCatalog {
    plugins: Arc<RwLock<HashMap<PluginSpecifier, Plugin>>>,
    compatibility_plugins: Arc<RwLock<HashMap<String, Plugin>>>,
    compatibility_dependencies: Arc<RwLock<HashMap<String, BTreeSet<PathBuf>>>>,
    expressions: Arc<ExpressionEnvironment>,
    bare_module_base: Option<PathBuf>,
    hmr_externals: Arc<RwLock<BTreeSet<PathBuf>>>,
    hmr_transaction: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
struct ResolvedPlugin {
    plugin: Plugin,
    module_path: Option<PathBuf>,
}

struct HmrCandidate {
    key: String,
    previous_plugin: Plugin,
    previous_dependencies: BTreeSet<PathBuf>,
    plugin: Plugin,
    dependencies: BTreeSet<PathBuf>,
}

impl Default for PluginCatalog {
    fn default() -> Self {
        Self {
            plugins: Arc::new(RwLock::new(HashMap::new())),
            compatibility_plugins: Arc::new(RwLock::new(HashMap::new())),
            compatibility_dependencies: Arc::new(RwLock::new(HashMap::new())),
            expressions: Arc::new(ExpressionEnvironment::from_process()),
            bare_module_base: None,
            hmr_externals: Arc::new(RwLock::new(BTreeSet::new())),
            hmr_transaction: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

impl fmt::Debug for PluginCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginCatalog")
            .field("len", &self.plugins.read().len())
            .finish_non_exhaustive()
    }
}

impl PluginCatalog {
    /// Creates an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the process snapshot used by loader JavaScript expressions.
    /// Tests and deterministic launchers use this instead of ambient reads.
    #[must_use]
    pub fn with_expression_environment(mut self, environment: ExpressionEnvironment) -> Self {
        self.expressions = Arc::new(environment);
        self
    }

    /// Selects the installed-host anchor used for unknown bare JavaScript packages.
    #[must_use]
    pub fn with_bare_module_base(mut self, base: impl Into<PathBuf>) -> Self {
        self.bare_module_base = Some(base.into());
        self
    }

    /// Replaces the launcher/framework dependency set that requests a full restart.
    #[must_use]
    pub fn with_hmr_externals(self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        *self.hmr_externals.write() = paths
            .into_iter()
            .map(Into::into)
            .map(|path| path.canonicalize().unwrap_or(path))
            .collect();
        self
    }

    /// Registers one compiled plugin under an external composition name.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-name error without replacing the prior plugin.
    pub fn register(&self, specifier: PluginSpecifier, plugin: Plugin) -> Result<(), LoaderError> {
        let mut plugins = self.plugins.write();
        if plugins.contains_key(&specifier) {
            return Err(LoaderError::DuplicatePlugin(specifier.to_string()));
        }
        plugins.insert(specifier, plugin);
        Ok(())
    }

    /// Convenience registration from a checked string.
    ///
    /// # Errors
    ///
    /// Returns invalid or duplicate specifier failures.
    pub fn register_named(&self, specifier: &str, plugin: Plugin) -> Result<(), LoaderError> {
        self.register(PluginSpecifier::new(specifier)?, plugin)
    }

    fn resolve(&self, specifier: &PluginSpecifier) -> Result<Plugin, LoaderError> {
        self.plugins
            .read()
            .get(specifier)
            .cloned()
            .ok_or_else(|| LoaderError::UnknownPlugin(specifier.to_string()))
    }

    fn resolve_entry(
        &self,
        context: &Context,
        specifier: &PluginSpecifier,
    ) -> Result<ResolvedPlugin, LoaderError> {
        if let Some(plugin) = self.plugins.read().get(specifier).cloned() {
            return Ok(ResolvedPlugin {
                plugin,
                module_path: None,
            });
        }
        let path = resolve_plugin_path(
            context,
            specifier.as_str(),
            self.bare_module_base.as_deref(),
        )?;
        let key = path.to_string_lossy().into_owned();
        if let Some(plugin) = self.compatibility_plugins.read().get(&key).cloned() {
            return Ok(ResolvedPlugin {
                plugin,
                module_path: Some(path),
            });
        }
        let loaded = javascript_plugin::load(&path, self.expressions.process_facade())
            .map_err(|error| LoaderError::ModuleLoad(format!("{specifier}: {error:#}")))?;
        let plugin = loaded.plugin;
        self.compatibility_dependencies
            .write()
            .insert(key.clone(), loaded.dependencies);
        self.compatibility_plugins
            .write()
            .insert(key, plugin.clone());
        Ok(ResolvedPlugin {
            plugin,
            module_path: Some(path),
        })
    }

    fn prepare_hmr_candidates(
        &self,
        affected: &[String],
    ) -> Result<Vec<HmrCandidate>, LoaderError> {
        affected
            .iter()
            .map(|key| {
                let loaded =
                    javascript_plugin::load(Path::new(key), self.expressions.process_facade())
                        .map_err(|error| LoaderError::ModuleLoad(format!("{key}: {error:#}")))?;
                let previous_plugin = self
                    .compatibility_plugins
                    .read()
                    .get(key)
                    .cloned()
                    .ok_or_else(|| {
                        LoaderError::ModuleLoad(format!("missing cached plugin {key}"))
                    })?;
                let previous_dependencies = self
                    .compatibility_dependencies
                    .read()
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                Ok(HmrCandidate {
                    key: key.clone(),
                    previous_plugin,
                    previous_dependencies,
                    plugin: loaded.plugin,
                    dependencies: loaded.dependencies,
                })
            })
            .collect()
    }

    fn install_hmr_candidates(&self, candidates: &[HmrCandidate], rollback: bool) {
        let mut plugins = self.compatibility_plugins.write();
        let mut dependencies = self.compatibility_dependencies.write();
        for candidate in candidates {
            if rollback {
                plugins.insert(candidate.key.clone(), candidate.previous_plugin.clone());
                dependencies.insert(
                    candidate.key.clone(),
                    candidate.previous_dependencies.clone(),
                );
            } else {
                plugins.insert(candidate.key.clone(), candidate.plugin.clone());
                dependencies.insert(candidate.key.clone(), candidate.dependencies.clone());
            }
        }
    }

    /// Parses a composition and verifies every enabled plugin name resolves
    /// before any running generation is touched.
    ///
    /// # Errors
    ///
    /// Returns document, entry, or unknown-plugin failures.
    pub fn preflight_yaml(&self, source: &str) -> Result<(), LoaderError> {
        let entries = parse_entries(source)?;
        preflight_entries(self, &entries)
    }

    /// Parses and mounts a source-compatible YAML composition list.
    ///
    /// # Errors
    ///
    /// Returns parse, catalog, startup, or rollback failures. Any mounts made
    /// before an error are disposed before the error returns.
    pub async fn load_yaml(
        &self,
        context: &Context,
        source: &str,
    ) -> Result<LoadedComposition, LoaderError> {
        let mut entries = parse_entries(source)?;
        materialize_includes(context, &mut entries)?;
        validate_unique_tree(&entries)?;
        let generation = context.clone();
        let (settlement, completion) = LoaderSettlement::pending();
        let settlement_effect = generation
            .provide(LOADER, settlement.clone())
            .map_err(|error| LoaderError::SettlementService(error.to_string()))?;
        self.mount(
            &generation,
            &entries,
            settlement,
            settlement_effect,
            completion,
        )
        .await
    }

    /// Parses and mounts a composition with relative-expression resolution
    /// rooted at the containing directory of `path`.
    ///
    /// # Errors
    ///
    /// Returns invalid file URLs plus ordinary load failures.
    pub async fn load_yaml_at(
        &self,
        context: &Context,
        source: &str,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<LoadedComposition> {
        let directory = path
            .as_ref()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("composition path has no containing directory"))?;
        let base_url = url::Url::from_directory_path(directory)
            .map_err(|()| anyhow::anyhow!("composition directory is not an absolute file path"))?;
        let context = context.with_meta("loader.base_url", Value::String(base_url.to_string()));
        Ok(self.load_yaml(&context, source).await?)
    }

    /// Reads and mounts a YAML composition file.
    ///
    /// # Errors
    ///
    /// Returns file, parse, catalog, startup, or rollback failures.
    pub async fn load_file(
        &self,
        context: &Context,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<LoadedComposition> {
        let source = tokio::fs::read_to_string(path.as_ref()).await?;
        self.load_yaml_at(context, &source, path).await
    }

    async fn mount(
        &self,
        context: &Context,
        entries: &[Entry],
        settlement: Arc<LoaderSettlement>,
        settlement_effect: EffectHandle,
        completion: LoaderSettlementCompletion,
    ) -> Result<LoadedComposition, LoaderError> {
        let runtime = Arc::new(CompositionRuntime::new(
            context.clone(),
            self.clone(),
            settlement.clone(),
            Vec::new(),
        ));
        settlement.attach(&runtime);
        let mut mounted = Vec::new();
        if let Err(error) = mount_entries(self, context, entries, &mut mounted).await {
            let mut failures = vec![error.to_string()];
            if let Err(cleanup) = runtime.dispose_programmatic().await {
                failures.push(cleanup.to_string());
            }
            if let Err(cleanup) = dispose_entries(&mut mounted).await {
                failures.push(cleanup.to_string());
            }
            let error = if failures.len() == 1 {
                error
            } else {
                LoaderError::Disposal(failures.join("; "))
            };
            completion.finish(Err(Arc::from(error.to_string())));
            let _ = settlement_effect.dispose().await;
            return Err(error);
        }
        if let Err(error) = await_entries_settled(&mounted).await {
            let mut failures = vec![error.to_string()];
            if let Err(cleanup) = runtime.dispose_programmatic().await {
                failures.push(cleanup.to_string());
            }
            if let Err(cleanup) = dispose_entries(&mut mounted).await {
                failures.push(cleanup.to_string());
            }
            let error = if failures.len() == 1 {
                error
            } else {
                LoaderError::Disposal(failures.join("; "))
            };
            completion.finish(Err(Arc::from(error.to_string())));
            let _ = settlement_effect.dispose().await;
            return Err(error);
        }
        runtime.finish_initial_mount(mounted);
        completion.finish(Ok(()));
        Ok(LoadedComposition {
            runtime,
            settlement_effect: Some(settlement_effect),
        })
    }
}

fn preflight_entries(catalog: &PluginCatalog, entries: &[Entry]) -> Result<(), LoaderError> {
    fn visit(catalog: &PluginCatalog, entries: &[Entry]) -> Result<(), LoaderError> {
        for entry in entries.iter().filter(|entry| !entry.disabled) {
            if !entry.group && entry.include.is_none() {
                let _ = catalog.resolve(&entry.plugin)?;
            }
            visit(catalog, &entry.children)?;
        }
        Ok(())
    }
    validate_unique_tree(entries)?;
    visit(catalog, entries)
}

fn preflight_active_entries(
    catalog: &PluginCatalog,
    context: &Context,
    entries: &[Entry],
) -> Result<(), LoaderError> {
    fn visit(
        catalog: &PluginCatalog,
        context: &Context,
        entries: &[Entry],
    ) -> Result<(), LoaderError> {
        for entry in entries {
            let disabled = effective_disabled(catalog, context, entry)?;
            let entry_context = child_context(context, entry);
            if entry.group || entry.include.is_some() {
                if !disabled {
                    visit(catalog, &entry_context, &entry.children)?;
                }
                continue;
            }
            if disabled {
                continue;
            }
            let _ = catalog
                .resolve_entry(&entry_context, &entry.plugin)
                .map_err(|error| entry_import_failure(entry, error))?;
            visit(catalog, &entry_context, &entry.children)?;
        }
        Ok(())
    }
    validate_unique_tree(entries)?;
    visit(catalog, context, entries)
}

fn validate_unique_tree(entries: &[Entry]) -> Result<(), LoaderError> {
    fn visit(
        entries: &[Entry],
        seen: &mut std::collections::HashSet<EntryId>,
    ) -> Result<(), LoaderError> {
        for entry in entries {
            if !seen.insert(entry.id.clone()) {
                return Err(LoaderError::InvalidDocument(format!(
                    "duplicate loader entry id: {}",
                    entry.id
                )));
            }
            if entry.include.is_some() {
                visit(&entry.children, &mut std::collections::HashSet::new())?;
            } else {
                visit(&entry.children, seen)?;
            }
        }
        Ok(())
    }
    visit(entries, &mut std::collections::HashSet::new())
}

type MountFuture<'a> = Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + 'a>>;

fn mount_entries<'a>(
    catalog: &'a PluginCatalog,
    context: &'a Context,
    entries: &'a [Entry],
    mounted: &'a mut Vec<MountedEntry>,
) -> MountFuture<'a> {
    Box::pin(async move {
        for entry in entries {
            match mount_entry(catalog, context, entry).await {
                Ok(entry) => mounted.push(entry),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })
}

fn mount_entry<'a>(
    catalog: &'a PluginCatalog,
    parent_context: &'a Context,
    entry: &'a Entry,
) -> Pin<Box<dyn Future<Output = Result<MountedEntry, LoaderError>> + Send + 'a>> {
    mount_entry_with_plugin(catalog, parent_context, entry, None)
}

fn mount_entry_with_plugin<'a>(
    catalog: &'a PluginCatalog,
    parent_context: &'a Context,
    entry: &'a Entry,
    resolved: Option<ResolvedPlugin>,
) -> Pin<Box<dyn Future<Output = Result<MountedEntry, LoaderError>> + Send + 'a>> {
    Box::pin(async move {
        let disabled = effective_disabled(catalog, parent_context, entry)?;
        let entry_context = child_context(parent_context, entry);
        if entry.group || entry.include.is_some() {
            let mut children = Vec::new();
            if !disabled
                && let Err(error) =
                    mount_entries(catalog, &entry_context, &entry.children, &mut children).await
            {
                let error = match dispose_entries(&mut children).await {
                    Ok(()) => error,
                    Err(cleanup) => LoaderError::Disposal(format!("{error}; {cleanup}")),
                };
                return Err(error);
            }
            return Ok(MountedEntry {
                options: entry.clone(),
                effective_disabled: disabled,
                parent_context: parent_context.clone(),
                entry_context,
                fiber: None,
                plugin: None,
                module_path: None,
                children,
            });
        }
        if disabled {
            return Ok(MountedEntry {
                options: entry.clone(),
                effective_disabled: true,
                parent_context: parent_context.clone(),
                entry_context,
                fiber: None,
                plugin: None,
                module_path: None,
                children: Vec::new(),
            });
        }
        let environment = catalog.expressions.clone();
        let resolved = resolved
            .map_or_else(|| catalog.resolve_entry(&entry_context, &entry.plugin), Ok)
            .map_err(|error| entry_import_failure(entry, error))?;
        let module_path = resolved.module_path;
        let plugin = resolved
            .plugin
            .with_additional_inject(entry.inject.clone())
            .with_config_resolver(move |context, raw| {
                expression::interpolate_config(&environment, context, raw)
            });
        let fiber = entry_context
            .plugin(plugin.clone(), entry.config.clone())
            .map_err(|error| entry_failure(entry, error))?;
        if let Err(error) = fiber.await_settled().await {
            let startup = entry_failure(entry, error);
            let cleanup = fiber.dispose().await;
            return Err(match cleanup {
                Ok(()) => startup,
                Err(cleanup) => LoaderError::Disposal(format!("{startup}; {cleanup:#}")),
            });
        }
        let mut children = Vec::new();
        if let Err(error) =
            mount_entries(catalog, fiber.context(), &entry.children, &mut children).await
        {
            let mut errors = vec![error.to_string()];
            if let Err(error) = dispose_entries(&mut children).await {
                errors.push(error.to_string());
            }
            if let Err(error) = fiber.dispose().await {
                errors.push(format!("{error:#}"));
            }
            return Err(LoaderError::Disposal(errors.join("; ")));
        }
        Ok(MountedEntry {
            options: entry.clone(),
            effective_disabled: false,
            parent_context: parent_context.clone(),
            entry_context,
            fiber: Some(fiber),
            plugin: Some(plugin),
            module_path,
            children,
        })
    })
}

fn effective_disabled(
    catalog: &PluginCatalog,
    context: &Context,
    entry: &Entry,
) -> Result<bool, LoaderError> {
    entry
        .disabled_expression
        .as_ref()
        .map_or(Ok(entry.disabled), |expression| {
            catalog
                .expressions
                .evaluate(context, expression)
                .map(|value| value.is_some_and(expression::javascript_truthy))
                .map_err(|error| entry_failure(entry, error))
        })
}

fn patched_context(parent: &Context, entry: &Entry) -> Context {
    let mut context = parent.clone();
    for (name, label) in &entry.isolate {
        context = match label {
            Some(label) => context.isolate_named_as(name, label),
            None => context.isolate_named(name),
        };
    }
    for (name, value) in &entry.intercept {
        context = context.intercept(name, value.clone());
    }
    context
}

fn child_context(parent: &Context, entry: &Entry) -> Context {
    let context = patched_context(parent, entry)
        .with_meta(
            "loader.entry_name",
            Value::String(entry.plugin.as_str().to_owned()),
        )
        .with_meta("loader.entry_id", Value::String(entry.id.to_string()));
    entry
        .include
        .as_ref()
        .and_then(|include| include.base_url.as_ref())
        .map_or(context.clone(), |base_url| {
            context.with_meta("loader.base_url", Value::String(base_url.clone()))
        })
}

fn entry_failure(entry: &Entry, error: impl std::fmt::Display) -> LoaderError {
    LoaderError::PluginStartup {
        entry: entry.id.to_string(),
        plugin: entry.plugin.to_string(),
        message: error.to_string(),
    }
}

fn entry_import_failure(entry: &Entry, error: impl std::fmt::Display) -> LoaderError {
    LoaderError::PluginImport {
        entry: entry.id.to_string(),
        plugin: entry.plugin.to_string(),
        message: error.to_string(),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CompositionDocument {
    Entries(Vec<Entry>),
    Tree(ConfigTree),
}

fn parse_entries(source: &str) -> Result<Vec<Entry>, LoaderError> {
    match profile_patch::parse_entry_list_yaml(source) {
        Ok(entries) => entries
            .iter()
            .map(profile_entry_to_runtime)
            .collect::<Result<Vec<_>, _>>(),
        Err(profile_error) => match serde_yml::from_str::<CompositionDocument>(source) {
            Ok(CompositionDocument::Entries(entries)) => Ok(entries),
            Ok(CompositionDocument::Tree(tree)) => Ok(tree.entries),
            Err(_) => Err(LoaderError::InvalidDocument(profile_error.to_string())),
        },
    }
}

fn materialize_includes(context: &Context, entries: &mut [Entry]) -> Result<(), LoaderError> {
    for entry in entries {
        let child_context = if let Some(include) = entry.include.clone() {
            let (path, base_url) = resolve_include_path(context, &include.path)?;
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let initial = include.initial.as_ref().ok_or_else(|| {
                        LoaderError::InvalidDocument(format!(
                            "config file not found: {}",
                            path.display()
                        ))
                    })?;
                    let source = profile_patch::render_entry_list_yaml(initial)
                        .map_err(|error| LoaderError::InvalidDocument(error.to_string()))?;
                    std::fs::write(&path, &source).map_err(|error| {
                        LoaderError::InvalidDocument(format!(
                            "failed to write config file {}: {error}",
                            path.display()
                        ))
                    })?;
                    source
                }
                Err(error) => {
                    return Err(LoaderError::InvalidDocument(format!(
                        "failed to read config file {}: {error}",
                        path.display()
                    )));
                }
            };
            let parsed = profile_patch::parse_entry_list_yaml(&source).map_err(|error| {
                LoaderError::InvalidDocument(format!(
                    "failed to parse config file {}: {error}",
                    path.display()
                ))
            })?;
            let composed = profile_patch::apply_entry_patches_with_warning_sink(
                &parsed,
                &include.patches,
                |warning| tracing::warn!(%warning, path = %path.display(), "include patch skipped"),
            )
            .map_err(|error| LoaderError::InvalidDocument(error.to_string()))?;
            entry.children = composed
                .iter()
                .map(profile_entry_to_runtime)
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(options) = &mut entry.include {
                options.base_url = Some(base_url.clone());
                options.resolved_path = Some(path.canonicalize().unwrap_or_else(|_| path.clone()));
            }
            context.with_meta("loader.base_url", Value::String(base_url))
        } else {
            context.clone()
        };
        materialize_includes(&child_context, &mut entry.children)?;
    }
    Ok(())
}

fn resolve_include_path(
    context: &Context,
    configured: &str,
) -> Result<(PathBuf, String), LoaderError> {
    let configured_path = Path::new(configured);
    let url = if configured_path.is_absolute() {
        url::Url::from_file_path(configured_path).map_err(|()| {
            LoaderError::InvalidDocument(format!(
                "include path is not an absolute file URL: {}",
                configured_path.display()
            ))
        })?
    } else if let Ok(url) = url::Url::parse(configured) {
        url
    } else {
        let base = context
            .meta("loader.base_url")
            .and_then(|value| value.as_str().map(str::to_owned))
            .map_or_else(
                || {
                    std::env::current_dir()
                        .ok()
                        .and_then(|path| url::Url::from_directory_path(path).ok())
                },
                |base| url::Url::parse(&base).ok(),
            )
            .ok_or_else(|| {
                LoaderError::InvalidDocument("loader base URL is unavailable".to_owned())
            })?;
        base.join(configured).map_err(|error| {
            LoaderError::InvalidDocument(format!("invalid include path {configured:?}: {error}"))
        })?
    };
    let path = url
        .to_file_path()
        .map_err(|()| LoaderError::InvalidDocument(format!("include URL must use file: {url}")))?;
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default();
    if !matches!(extension, "json" | "yaml" | "yml") {
        return Err(LoaderError::InvalidDocument(format!(
            "extension \".{extension}\" not supported"
        )));
    }
    let directory = path.parent().ok_or_else(|| {
        LoaderError::InvalidDocument(format!("include path has no directory: {}", path.display()))
    })?;
    let base_url = url::Url::from_directory_path(directory)
        .map_err(|()| {
            LoaderError::InvalidDocument(format!(
                "include directory is not absolute: {}",
                directory.display()
            ))
        })?
        .to_string();
    Ok((path, base_url))
}

fn resolve_plugin_path(
    context: &Context,
    configured: &str,
    bare_module_base: Option<&Path>,
) -> Result<PathBuf, LoaderError> {
    let path = Path::new(configured);
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    if let Ok(url) = url::Url::parse(configured) {
        return url
            .to_file_path()
            .map_err(|()| LoaderError::UnknownPlugin(format!("plugin URL must use file: {url}")));
    }
    if !configured.starts_with('.') {
        return resolve_bare_plugin_path(context, configured, bare_module_base);
    }
    let base = context
        .meta("loader.base_url")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| LoaderError::UnknownPlugin(configured.to_owned()))?;
    let url = url::Url::parse(&base)
        .and_then(|base| base.join(configured))
        .map_err(|error| LoaderError::UnknownPlugin(format!("{configured}: {error}")))?;
    url.to_file_path()
        .map_err(|()| LoaderError::UnknownPlugin(format!("plugin URL must use file: {url}")))
}

fn resolve_bare_plugin_path(
    context: &Context,
    configured: &str,
    explicit_base: Option<&Path>,
) -> Result<PathBuf, LoaderError> {
    let start = explicit_base
        .map(Path::to_path_buf)
        .or_else(|| {
            context
                .meta("loader.base_url")
                .and_then(|value| value.as_str().and_then(|value| url::Url::parse(value).ok()))
                .and_then(|url| url.to_file_path().ok())
        })
        .ok_or_else(|| LoaderError::UnknownPlugin(configured.to_owned()))?;
    let start = if start.is_dir() {
        start
    } else {
        start.parent().map(Path::to_path_buf).ok_or_else(|| {
            LoaderError::UnknownPlugin(format!(
                "bare module base has no directory: {}",
                start.display()
            ))
        })?
    };
    for ancestor in start.ancestors() {
        let package = ancestor.join("node_modules").join(configured);
        let manifest_path = package.join("package.json");
        let Ok(source) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let manifest: Value = serde_json::from_str(&source).map_err(|error| {
            LoaderError::ModuleLoad(format!(
                "failed to parse {}: {error}",
                manifest_path.display()
            ))
        })?;
        let export = manifest
            .get("exports")
            .and_then(package_export)
            .or_else(|| manifest.get("module").and_then(Value::as_str))
            .or_else(|| manifest.get("main").and_then(Value::as_str))
            .unwrap_or("index.js");
        let resolved = package.join(export);
        if resolved.is_file() {
            return Ok(resolved);
        }
        return Err(LoaderError::ModuleLoad(format!(
            "package {configured:?} entry does not exist: {}",
            resolved.display()
        )));
    }
    Err(LoaderError::UnknownPlugin(configured.to_owned()))
}

fn package_export(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => Some(value),
        Value::Object(exports) => exports
            .get(".")
            .and_then(package_export)
            .or_else(|| exports.get("import").and_then(package_export))
            .or_else(|| exports.get("default").and_then(package_export)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => None,
    }
}

fn profile_entry_to_runtime(entry: &profile_patch::ProfileEntry) -> Result<Entry, LoaderError> {
    use profile_patch::ProfileNode;

    let id = entry
        .id()
        .filter(profile_patch::ProfileEntryId::is_truthy)
        .map_or_else(generated_entry_id, |id| EntryId(id.as_str().to_owned()));
    let plugin = entry
        .name()
        .ok_or(LoaderError::InvalidPluginSpecifier)
        .and_then(PluginSpecifier::new)?;
    let group = entry.group().is_some_and(ProfileNode::is_javascript_truthy);
    let disabled_expression = entry
        .disabled()
        .and_then(ProfileNode::as_javascript)
        .map(|expression| expression.as_str().to_owned());
    let disabled = entry
        .disabled()
        .filter(|value| value.as_javascript().is_none())
        .is_some_and(ProfileNode::is_javascript_truthy);
    let inject = node_keys(entry.inject())?;
    let isolate = node_isolation(entry.field("isolate"))?;
    let intercept = node_json_map(entry.field("intercept"))?;
    let config_node = entry
        .config()
        .cloned()
        .unwrap_or_else(|| ProfileNode::Mapping(IndexMap::new()));
    let include = if matches!(
        plugin.as_str(),
        "cordis:include" | "cordis-plugin-include" | "@seekdeep-ai/cordis-plugin-include"
    ) {
        Some(parse_include_options(&config_node)?)
    } else {
        None
    };
    let children = if group {
        profile_node_entries(&config_node)?
    } else {
        entry
            .field("children")
            .map(profile_node_entries)
            .transpose()?
            .unwrap_or_default()
    };
    Ok(Entry {
        id,
        plugin,
        config: expression::profile_node_to_raw_json(&config_node)?,
        disabled,
        inject,
        group,
        children,
        disabled_expression,
        isolate,
        intercept,
        include,
    })
}

fn parse_include_options(node: &profile_patch::ProfileNode) -> Result<IncludeOptions, LoaderError> {
    use profile_patch::ProfileNode;

    let ProfileNode::Mapping(config) = node else {
        return Err(LoaderError::InvalidDocument(
            "cordis:include config must be a mapping".to_owned(),
        ));
    };
    let path = config
        .get("path")
        .and_then(ProfileNode::as_str)
        .ok_or_else(|| {
            LoaderError::InvalidDocument("cordis:include config.path must be a string".to_owned())
        })?
        .to_owned();
    let patches = match config.get("patches") {
        None | Some(ProfileNode::Null) => Vec::new(),
        Some(ProfileNode::Sequence(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let ProfileNode::Mapping(fields) = value else {
                    return Err(LoaderError::InvalidDocument(format!(
                        "cordis:include patch {} must be a mapping",
                        index + 1
                    )));
                };
                Ok(profile_patch::ProfilePatch::from_fields(fields.clone()))
            })
            .collect::<Result<Vec<_>, LoaderError>>()?,
        Some(_) => {
            return Err(LoaderError::InvalidDocument(
                "cordis:include config.patches must be an array".to_owned(),
            ));
        }
    };
    let initial = config
        .get("initial")
        .map(profile_node_profile_entries)
        .transpose()?;
    Ok(IncludeOptions {
        path,
        patches,
        initial,
        base_url: None,
        resolved_path: None,
    })
}

fn profile_node_profile_entries(
    node: &profile_patch::ProfileNode,
) -> Result<Vec<profile_patch::ProfileEntry>, LoaderError> {
    let profile_patch::ProfileNode::Sequence(entries) = node else {
        return Err(LoaderError::InvalidDocument(
            "loader entry list must be an array".to_owned(),
        ));
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let profile_patch::ProfileNode::Mapping(fields) = node else {
                return Err(LoaderError::InvalidDocument(format!(
                    "loader entry {} must be a mapping",
                    index + 1
                )));
            };
            Ok(profile_patch::ProfileEntry::from_fields(fields.clone()))
        })
        .collect()
}

fn profile_node_entries(node: &profile_patch::ProfileNode) -> Result<Vec<Entry>, LoaderError> {
    let profile_patch::ProfileNode::Sequence(entries) = node else {
        return Err(LoaderError::InvalidDocument(
            "group config must be an array of loader entries".to_owned(),
        ));
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let profile_patch::ProfileNode::Mapping(fields) = node else {
                return Err(LoaderError::InvalidDocument(format!(
                    "loader entry {} must be a mapping",
                    index + 1
                )));
            };
            profile_entry_to_runtime(&profile_patch::ProfileEntry::from_fields(fields.clone()))
        })
        .collect()
}

fn node_keys(node: Option<&profile_patch::ProfileNode>) -> Result<Vec<String>, LoaderError> {
    use profile_patch::ProfileNode;
    match node {
        None | Some(ProfileNode::Null) => Ok(Vec::new()),
        Some(ProfileNode::Sequence(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    LoaderError::InvalidDocument(format!(
                        "inject entry {} must be a service name",
                        index + 1
                    ))
                })
            })
            .collect(),
        Some(ProfileNode::Mapping(values)) => Ok(values.keys().cloned().collect()),
        Some(_) => Err(LoaderError::InvalidDocument(
            "inject must be an array or mapping".to_owned(),
        )),
    }
}

fn node_isolation(
    node: Option<&profile_patch::ProfileNode>,
) -> Result<IndexMap<String, Option<String>>, LoaderError> {
    use profile_patch::ProfileNode;
    match node {
        None | Some(ProfileNode::Null) => Ok(IndexMap::new()),
        Some(ProfileNode::Mapping(values)) => values
            .iter()
            .filter_map(|(name, value)| match value {
                ProfileNode::Bool(true) => Some(Ok((name.clone(), None))),
                ProfileNode::String(label) if !label.is_empty() => {
                    Some(Ok((name.clone(), Some(label.clone()))))
                }
                ProfileNode::Null | ProfileNode::Bool(false) | ProfileNode::String(_) => None,
                ProfileNode::Number(_)
                | ProfileNode::Sequence(_)
                | ProfileNode::Mapping(_)
                | ProfileNode::JavaScript(_) => Some(Err(LoaderError::InvalidDocument(format!(
                    "isolate realm for {name:?} must be true or a non-empty label"
                )))),
            })
            .collect(),
        Some(_) => Err(LoaderError::InvalidDocument(
            "isolate must be a mapping".to_owned(),
        )),
    }
}

fn node_json_map(
    node: Option<&profile_patch::ProfileNode>,
) -> Result<IndexMap<String, Value>, LoaderError> {
    use profile_patch::ProfileNode;
    match node {
        None | Some(ProfileNode::Null) => Ok(IndexMap::new()),
        Some(ProfileNode::Mapping(values)) => values
            .iter()
            .map(|(name, value)| Ok((name.clone(), expression::profile_node_to_raw_json(value)?)))
            .collect(),
        Some(_) => Err(LoaderError::InvalidDocument(
            "intercept must be a mapping".to_owned(),
        )),
    }
}

#[derive(Debug)]
struct MountedEntry {
    options: Entry,
    effective_disabled: bool,
    parent_context: Context,
    entry_context: Context,
    fiber: Option<Arc<PluginFiber>>,
    plugin: Option<Plugin>,
    module_path: Option<PathBuf>,
    children: Vec<Self>,
}

impl MountedEntry {
    fn same_runtime_shape(&self, candidate: &Entry) -> bool {
        let include_shape_matches = match (&self.options.include, &candidate.include) {
            (Some(previous), Some(candidate)) => previous.path == candidate.path,
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        self.options.plugin == candidate.plugin
            && self.options.inject == candidate.inject
            && self.options.group == candidate.group
            && self.options.isolate == candidate.isolate
            && self.options.intercept == candidate.intercept
            && include_shape_matches
    }

    fn update<'a>(
        &'a mut self,
        catalog: &'a PluginCatalog,
        candidate: &'a Entry,
    ) -> Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + 'a>> {
        Box::pin(async move {
            let candidate_disabled = effective_disabled(catalog, &self.parent_context, candidate)?;
            if !self.same_runtime_shape(candidate) {
                return self.replace_runtime(catalog, candidate).await;
            }

            if candidate.group || candidate.include.is_some() {
                match (self.effective_disabled, candidate_disabled) {
                    (true, true) => {}
                    (true, false) => {
                        let mut children = Vec::new();
                        mount_entries(
                            catalog,
                            &self.entry_context,
                            &candidate.children,
                            &mut children,
                        )
                        .await?;
                        self.children = children;
                    }
                    (false, true) => {
                        let previous = self.options.clone();
                        if let Err(error) = dispose_entries(&mut self.children).await {
                            let rollback = mount_entries(
                                catalog,
                                &self.entry_context,
                                &previous.children,
                                &mut self.children,
                            )
                            .await;
                            return Err(with_rollback(error, rollback));
                        }
                    }
                    (false, false) => {
                        reconcile_entries(
                            catalog,
                            &self.entry_context,
                            &mut self.children,
                            &candidate.children,
                        )
                        .await?;
                    }
                }
                self.options = candidate.clone();
                self.effective_disabled = candidate_disabled;
                return Ok(());
            }

            match (self.fiber.clone(), candidate_disabled) {
                (None, true) => {
                    self.options = candidate.clone();
                    self.effective_disabled = true;
                    Ok(())
                }
                (None, false) | (Some(_), true) => self.replace_runtime(catalog, candidate).await,
                (Some(fiber), false) => {
                    let previous = self.options.clone();
                    let changed_config = previous.config != candidate.config;
                    if changed_config
                        && let Err(error) =
                            fiber.update_transactional(candidate.config.clone()).await
                    {
                        return Err(entry_failure(candidate, error));
                    }
                    if let Err(error) = reconcile_entries(
                        catalog,
                        fiber.context(),
                        &mut self.children,
                        &candidate.children,
                    )
                    .await
                    {
                        if changed_config {
                            let rollback = fiber
                                .update_transactional(previous.config.clone())
                                .await
                                .map_err(|error| entry_failure(&previous, error));
                            return Err(with_rollback(error, rollback));
                        }
                        return Err(error);
                    }
                    self.options = candidate.clone();
                    self.effective_disabled = false;
                    Ok(())
                }
            }
        })
    }

    async fn replace_runtime(
        &mut self,
        catalog: &PluginCatalog,
        candidate: &Entry,
    ) -> Result<(), LoaderError> {
        let previous = self.options.clone();
        let previous_plugin = self.plugin.clone().map(|plugin| ResolvedPlugin {
            plugin,
            module_path: self.module_path.clone(),
        });
        let parent = self.parent_context.clone();
        if let Err(error) = self.dispose_runtime().await {
            let rollback =
                mount_entry_with_plugin(catalog, &parent, &previous, previous_plugin.clone()).await;
            return match rollback {
                Ok(restored) => {
                    *self = restored;
                    Err(error)
                }
                Err(rollback) => Err(LoaderError::Disposal(format!(
                    "{error}; loader entry rollback failed: {rollback}"
                ))),
            };
        }
        match mount_entry(catalog, &parent, candidate).await {
            Ok(next) => {
                *self = next;
                Ok(())
            }
            Err(error) => {
                match mount_entry_with_plugin(catalog, &parent, &previous, previous_plugin).await {
                    Ok(restored) => {
                        *self = restored;
                        Err(error)
                    }
                    Err(rollback) => Err(LoaderError::Disposal(format!(
                        "{error}; loader entry rollback failed: {rollback}"
                    ))),
                }
            }
        }
    }

    async fn replace_runtime_for_hmr(
        &mut self,
        catalog: &PluginCatalog,
    ) -> Result<(), LoaderError> {
        let previous = self.options.clone();
        let previous_plugin = self.plugin.clone().map(|plugin| ResolvedPlugin {
            plugin,
            module_path: self.module_path.clone(),
        });
        let parent = self.parent_context.clone();
        if let Err(error) = self.dispose_runtime().await {
            tracing::warn!(entry = %previous.id, %error, "Host HMR plugin disposal failed");
        }
        match mount_entry(catalog, &parent, &previous).await {
            Ok(next) => {
                *self = next;
                Ok(())
            }
            Err(error) => {
                match mount_entry_with_plugin(catalog, &parent, &previous, previous_plugin).await {
                    Ok(restored) => {
                        *self = restored;
                        Err(error)
                    }
                    Err(rollback) => Err(LoaderError::Disposal(format!(
                        "{error}; loader entry rollback failed: {rollback}"
                    ))),
                }
            }
        }
    }

    async fn dispose_runtime(&mut self) -> Result<(), LoaderError> {
        let mut errors = Vec::new();
        if let Err(error) = dispose_entries(&mut self.children).await {
            errors.push(error.to_string());
        }
        if let Some(fiber) = self.fiber.take()
            && let Err(error) = fiber.dispose().await
        {
            errors.push(format!("{error:#}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(LoaderError::Disposal(errors.join("; ")))
        }
    }
}

fn with_rollback(primary: LoaderError, rollback: Result<(), LoaderError>) -> LoaderError {
    match rollback {
        Ok(()) => primary,
        Err(rollback) => LoaderError::Disposal(format!(
            "{primary}; loader entry rollback failed: {rollback}"
        )),
    }
}

fn reconcile_entries<'a>(
    catalog: &'a PluginCatalog,
    context: &'a Context,
    mounted: &'a mut Vec<MountedEntry>,
    candidate: &'a [Entry],
) -> Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + 'a>> {
    Box::pin(async move {
        validate_unique_siblings(candidate)?;
        let previous = mounted
            .iter()
            .map(|entry| entry.options.clone())
            .collect::<Vec<_>>();
        let candidate = match apply_entries(catalog, context, mounted, candidate).await {
            Ok(()) => await_entries_settled(mounted).await,
            Err(error) => Err(error),
        };
        if let Err(error) = candidate {
            let rollback = match apply_entries(catalog, context, mounted, &previous).await {
                Ok(()) => await_entries_settled(mounted).await,
                Err(error) => Err(error),
            };
            return Err(with_rollback(error, rollback));
        }
        Ok(())
    })
}

fn apply_entries<'a>(
    catalog: &'a PluginCatalog,
    context: &'a Context,
    mounted: &'a mut Vec<MountedEntry>,
    candidate: &'a [Entry],
) -> Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + 'a>> {
    Box::pin(async move {
        validate_unique_siblings(candidate)?;
        let mut remaining = std::mem::take(mounted);
        let mut next = Vec::with_capacity(candidate.len());
        for options in candidate {
            if let Some(index) = remaining
                .iter()
                .position(|entry| entry.options.id == options.id)
            {
                let mut entry = remaining.remove(index);
                if let Err(error) = entry.update(catalog, options).await {
                    next.push(entry);
                    next.append(&mut remaining);
                    *mounted = next;
                    return Err(error);
                }
                next.push(entry);
            } else {
                match mount_entry(catalog, context, options).await {
                    Ok(entry) => next.push(entry),
                    Err(error) => {
                        next.append(&mut remaining);
                        *mounted = next;
                        return Err(error);
                    }
                }
            }
        }
        while let Some(mut removed) = remaining.pop() {
            if let Err(error) = removed.dispose_runtime().await {
                next.push(removed);
                next.append(&mut remaining);
                *mounted = next;
                return Err(error);
            }
        }
        *mounted = next;
        Ok(())
    })
}

fn validate_unique_siblings(entries: &[Entry]) -> Result<(), LoaderError> {
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        if !seen.insert(entry.id.clone()) {
            return Err(LoaderError::InvalidDocument(format!(
                "duplicate loader entry id: {}",
                entry.id
            )));
        }
    }
    Ok(())
}

fn collect_fibers(entries: &[MountedEntry]) -> Vec<Arc<PluginFiber>> {
    fn visit(entry: &MountedEntry, fibers: &mut Vec<Arc<PluginFiber>>) {
        if let Some(fiber) = &entry.fiber {
            fibers.push(fiber.clone());
        }
        for child in &entry.children {
            visit(child, fibers);
        }
    }
    let mut fibers = Vec::new();
    for entry in entries {
        visit(entry, &mut fibers);
    }
    fibers
}

async fn await_entries_settled(entries: &[MountedEntry]) -> Result<(), LoaderError> {
    let fibers = collect_fibers(entries);
    PluginFiber::await_all_quiescent(&fibers).await;
    for fiber in fibers {
        if let Err(error) = fiber.await_settled().await {
            let (entry, plugin) = locate_fiber(entries, &fiber).map_or_else(
                || {
                    (
                        fiber.plugin_name().to_owned(),
                        fiber.plugin_name().to_owned(),
                    )
                },
                |entry| {
                    (
                        entry.options.id.to_string(),
                        entry.options.plugin.to_string(),
                    )
                },
            );
            return Err(LoaderError::PluginStartup {
                entry,
                plugin,
                message: format!("{error:#}"),
            });
        }
    }
    Ok(())
}

fn module_paths_in_order(entries: &[MountedEntry]) -> Vec<PathBuf> {
    fn visit(entry: &MountedEntry, seen: &mut BTreeSet<PathBuf>, paths: &mut Vec<PathBuf>) {
        if let Some(path) = &entry.module_path
            && seen.insert(path.clone())
        {
            paths.push(path.clone());
        }
        for child in &entry.children {
            visit(child, seen, paths);
        }
    }
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for entry in entries {
        visit(entry, &mut seen, &mut paths);
    }
    paths
}

fn reload_mounted_entries<'a>(
    catalog: &'a PluginCatalog,
    entries: &'a mut [MountedEntry],
    affected: &'a BTreeSet<PathBuf>,
) -> Pin<Box<dyn Future<Output = Result<Vec<EntryId>, LoaderError>> + Send + 'a>> {
    Box::pin(async move {
        let mut reloaded = Vec::new();
        for entry in entries {
            if entry
                .module_path
                .as_ref()
                .is_some_and(|path| affected.contains(path))
                && entry.fiber.is_some()
            {
                entry.replace_runtime_for_hmr(catalog).await?;
                reloaded.push(entry.options.id.clone());
            }
            reloaded.extend(reload_mounted_entries(catalog, &mut entry.children, affected).await?);
        }
        Ok(reloaded)
    })
}

fn locate_fiber<'a>(
    entries: &'a [MountedEntry],
    target: &Arc<PluginFiber>,
) -> Option<&'a MountedEntry> {
    for entry in entries {
        if entry
            .fiber
            .as_ref()
            .is_some_and(|fiber| Arc::ptr_eq(fiber, target))
        {
            return Some(entry);
        }
        if let Some(found) = locate_fiber(&entry.children, target) {
            return Some(found);
        }
    }
    None
}

fn dispose_entries(
    entries: &mut Vec<MountedEntry>,
) -> Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + '_>> {
    Box::pin(async move {
        let mut errors = Vec::new();
        while let Some(mut entry) = entries.pop() {
            if let Err(error) = entry.dispose_runtime().await {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(LoaderError::Disposal(errors.join("; ")))
        }
    })
}

struct CompositionRuntime {
    context: Context,
    catalog: PluginCatalog,
    settlement: Arc<LoaderSettlement>,
    operation: tokio::sync::Mutex<()>,
    entries: parking_lot::Mutex<Option<Vec<MountedEntry>>>,
    fibers: RwLock<Vec<Arc<PluginFiber>>>,
    entry_snapshot: RwLock<Vec<LoaderEntrySnapshot>>,
    programmatic: parking_lot::Mutex<Vec<MountedEntry>>,
    initializing: AtomicBool,
    disposed: AtomicBool,
}

impl std::fmt::Debug for CompositionRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompositionRuntime")
            .field("fiber_count", &self.fibers.read().len())
            .field("entry_count", &self.entry_snapshot.read().len())
            .finish_non_exhaustive()
    }
}

impl CompositionRuntime {
    fn new(
        context: Context,
        catalog: PluginCatalog,
        settlement: Arc<LoaderSettlement>,
        entries: Vec<MountedEntry>,
    ) -> Self {
        let fibers = collect_fibers(&entries);
        let entry_snapshot = collect_entry_snapshot(&entries);
        Self {
            context,
            catalog,
            settlement,
            operation: tokio::sync::Mutex::new(()),
            entries: parking_lot::Mutex::new(Some(entries)),
            fibers: RwLock::new(fibers),
            entry_snapshot: RwLock::new(entry_snapshot),
            programmatic: parking_lot::Mutex::new(Vec::new()),
            initializing: AtomicBool::new(true),
            disposed: AtomicBool::new(false),
        }
    }

    fn fibers(&self) -> Vec<Arc<PluginFiber>> {
        let mut fibers = self.fibers.read().clone();
        fibers.extend(collect_fibers(&self.programmatic.lock()));
        fibers
    }

    fn all_entry_snapshots(&self) -> Vec<LoaderEntrySnapshot> {
        let mut entries = self.entry_snapshot.read().clone();
        entries.extend(collect_entry_snapshot(&self.programmatic.lock()));
        entries
    }

    async fn take_entries(
        &self,
    ) -> Result<(tokio::sync::MutexGuard<'_, ()>, Vec<MountedEntry>), LoaderError> {
        if self.entries.lock().is_none() {
            return Err(LoaderError::UpdateInProgress);
        }
        let operation = self.operation.lock().await;
        let entries = self.entries.lock().take().ok_or(LoaderError::Unavailable)?;
        Ok((operation, entries))
    }

    fn restore_entries(&self, entries: Vec<MountedEntry>) {
        *self.fibers.write() = collect_fibers(&entries);
        *self.entry_snapshot.write() = collect_entry_snapshot(&entries);
        *self.entries.lock() = Some(entries);
    }

    fn finish_initial_mount(&self, entries: Vec<MountedEntry>) {
        self.restore_entries(entries);
        self.initializing.store(false, Ordering::Release);
    }

    async fn reconcile_specs(&self, candidate: &[Entry]) -> Result<(), LoaderError> {
        preflight_active_entries(&self.catalog, &self.context, candidate)?;
        let completion = self.settlement.begin();
        let (_operation, mut entries) = self.take_entries().await?;
        let result = reconcile_entries(&self.catalog, &self.context, &mut entries, candidate).await;
        self.restore_entries(entries);
        completion.finish(
            result
                .as_ref()
                .map_err(|error| Arc::from(error.to_string()))
                .copied(),
        );
        result
    }

    async fn update_yaml(&self, source: &str) -> Result<(), LoaderError> {
        let mut entries = parse_entries(source)?;
        materialize_includes(&self.context, &mut entries)?;
        self.reconcile_specs(&entries).await
    }

    async fn append_yaml(&self, source: &str) -> Result<(), LoaderError> {
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        let mut appended = parse_entries(source)?;
        materialize_includes(&self.context, &mut appended)?;
        candidate.extend(appended);
        self.reconcile_specs(&candidate).await
    }

    async fn create_entry(
        &self,
        entry: Entry,
        parent: EntryParent,
        position: Option<usize>,
    ) -> Result<(), LoaderError> {
        if self.disposed.load(Ordering::Acquire) {
            return Err(LoaderError::Unavailable);
        }
        if parent == EntryParent::Root
            && (self.initializing.load(Ordering::Acquire) || self.entries.lock().is_none())
        {
            return self.create_programmatic_entry(entry).await;
        }
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        if find_entry(&candidate, &entry.id).is_some() {
            return Err(LoaderError::InvalidDocument(format!(
                "duplicate loader entry id: {}",
                entry.id
            )));
        }
        insert_entry(&mut candidate, entry, &parent, position)?;
        materialize_includes(&self.context, &mut candidate)?;
        self.reconcile_specs(&candidate).await
    }

    async fn create_programmatic_entry(&self, mut entry: Entry) -> Result<(), LoaderError> {
        if self.disposed.load(Ordering::Acquire) {
            return Err(LoaderError::Unavailable);
        }
        ensure_unique_runtime_id(self, &entry.id)?;
        materialize_includes(&self.context, std::slice::from_mut(&mut entry))?;
        let mut mounted = Some(mount_entry(&self.catalog, &self.context, &entry).await?);
        if let Err(error) = await_entries_settled(std::slice::from_ref(
            mounted.as_ref().expect("mounted entry is available"),
        ))
        .await
        {
            let cleanup = mounted
                .as_mut()
                .expect("mounted entry is available")
                .dispose_runtime()
                .await;
            return Err(with_rollback(error, cleanup));
        }
        let inserted = {
            let declarative_duplicate = self
                .entry_snapshot
                .read()
                .iter()
                .any(|candidate| candidate.id == entry.id);
            let mut programmatic = self.programmatic.lock();
            let programmatic_duplicate = programmatic
                .iter()
                .any(|candidate| candidate.options.id == entry.id);
            if self.disposed.load(Ordering::Acquire)
                || declarative_duplicate
                || programmatic_duplicate
            {
                false
            } else {
                programmatic.push(mounted.take().expect("mounted entry is available"));
                true
            }
        };
        if !inserted {
            let error = if self.disposed.load(Ordering::Acquire) {
                LoaderError::Unavailable
            } else {
                LoaderError::InvalidDocument(format!("duplicate loader entry id: {}", entry.id))
            };
            let cleanup = mounted
                .take()
                .expect("colliding mounted entry is available")
                .dispose_runtime()
                .await;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => LoaderError::Disposal(format!("{error}; {cleanup}")),
            });
        }
        Ok(())
    }

    async fn remove_programmatic_entry_if_present(
        &self,
        id: &EntryId,
    ) -> Result<bool, LoaderError> {
        let mut entry = {
            let mut entries = self.programmatic.lock();
            let Some(index) = entries.iter().position(|entry| entry.options.id == *id) else {
                return Ok(false);
            };
            entries.remove(index)
        };
        entry.dispose_runtime().await?;
        Ok(true)
    }

    async fn update_entry(
        &self,
        id: &EntryId,
        update: EntryUpdate,
        parent: EntryParent,
        position: Option<usize>,
    ) -> Result<(), LoaderError> {
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        let (mut entry, previous_parent, previous_position) = take_entry(&mut candidate, id)
            .ok_or_else(|| LoaderError::InvalidDocument(format!("cannot resolve entry {id}")))?;
        update.apply(&mut entry);
        match parent {
            EntryParent::Keep => insert_entry_at_path(
                &mut candidate,
                entry,
                &previous_parent,
                position.unwrap_or(previous_position),
            )?,
            parent => insert_entry(&mut candidate, entry, &parent, position)?,
        }
        materialize_includes(&self.context, &mut candidate)?;
        self.reconcile_specs(&candidate).await
    }

    async fn remove_entry(&self, id: &EntryId) -> Result<(), LoaderError> {
        if self.remove_programmatic_entry_if_present(id).await? {
            return Ok(());
        }
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        take_entry(&mut candidate, id)
            .ok_or_else(|| LoaderError::InvalidDocument(format!("cannot resolve entry {id}")))?;
        self.reconcile_specs(&candidate).await
    }

    async fn refresh_includes(&self) -> Result<(), LoaderError> {
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        materialize_includes(&self.context, &mut candidate)?;
        self.reconcile_specs(&candidate).await
    }

    async fn refresh_include_path(&self, path: &Path) -> Result<bool, LoaderError> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        if !has_include_path(&candidate, &path) {
            return Ok(false);
        }
        materialize_includes(&self.context, &mut candidate)?;
        self.reconcile_specs(&candidate).await?;
        Ok(true)
    }

    async fn update_include_patches(
        &self,
        id: &EntryId,
        patches: Vec<profile_patch::ProfilePatch>,
    ) -> Result<(), LoaderError> {
        let mut candidate = {
            let current = self.entries.lock();
            entry_specs(current.as_ref().ok_or(LoaderError::Unavailable)?)
        };
        let entry = find_entry_mut(&mut candidate, id)
            .ok_or_else(|| LoaderError::InvalidDocument(format!("cannot resolve entry {id}")))?;
        replace_include_patches(entry, patches)?;
        materialize_includes(&self.context, &mut candidate)?;
        self.reconcile_specs(&candidate).await
    }

    async fn reload_module(&self, path: &Path) -> Result<HostHmrOutcome, LoaderError> {
        let _hmr = self.catalog.hmr_transaction.lock().await;
        let changed = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if self.catalog.hmr_externals.read().contains(&changed) {
            return Ok(HostHmrOutcome::FullRestart);
        }

        let affected_set = self
            .catalog
            .compatibility_dependencies
            .read()
            .iter()
            .filter(|(_, dependencies)| dependencies.contains(&changed))
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        let affected = {
            let entries = self.entries.lock();
            let entries = entries.as_ref().ok_or(LoaderError::Unavailable)?;
            module_paths_in_order(entries)
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .filter(|key| affected_set.contains(key))
                .collect::<Vec<_>>()
        };
        if affected.is_empty() {
            let _ = self.context.events().emit(
                &self.context,
                "hmr/change",
                &seekdeep_cordis::EventArgs::one(changed),
            );
            return Ok(HostHmrOutcome::Untracked);
        }

        let candidates = self.catalog.prepare_hmr_candidates(&affected)?;

        let completion = self.settlement.begin();
        let (_operation, mut entries) = self.take_entries().await?;
        self.catalog.install_hmr_candidates(&candidates, false);
        let affected_paths = affected.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
        let result = reload_mounted_entries(&self.catalog, &mut entries, &affected_paths).await;
        match result {
            Ok(reloaded) => {
                self.restore_entries(entries);
                completion.finish(Ok(()));
                let _ = self.context.events().emit(
                    &self.context,
                    "hmr/reload",
                    &seekdeep_cordis::EventArgs::one(HostHmrReload {
                        changed,
                        entries: reloaded.clone(),
                    }),
                );
                Ok(HostHmrOutcome::Reloaded(reloaded))
            }
            Err(error) => {
                self.catalog.install_hmr_candidates(&candidates, true);
                let rollback =
                    reload_mounted_entries(&self.catalog, &mut entries, &affected_paths).await;
                self.restore_entries(entries);
                let error = match rollback {
                    Ok(_) => error,
                    Err(rollback) => LoaderError::Disposal(format!(
                        "{error}; Host HMR rollback failed: {rollback}"
                    )),
                };
                completion.finish(Err(Arc::from(error.to_string())));
                Err(error)
            }
        }
    }

    async fn dispose(&self) -> Result<(), LoaderError> {
        self.disposed.store(true, Ordering::Release);
        let programmatic_result = self.dispose_programmatic().await;
        let (_operation, mut entries) = self.take_entries().await?;
        let declarative_result = dispose_entries(&mut entries).await;
        self.fibers.write().clear();
        self.entry_snapshot.write().clear();
        match (programmatic_result, declarative_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(programmatic), Err(declarative)) => Err(LoaderError::Disposal(format!(
                "{programmatic}; {declarative}"
            ))),
        }
    }

    async fn dispose_programmatic(&self) -> Result<(), LoaderError> {
        let mut programmatic = std::mem::take(&mut *self.programmatic.lock());
        dispose_entries(&mut programmatic).await
    }
}

fn ensure_unique_runtime_id(runtime: &CompositionRuntime, id: &EntryId) -> Result<(), LoaderError> {
    let declarative_duplicate = runtime
        .entry_snapshot
        .read()
        .iter()
        .any(|entry| entry.id == *id);
    let programmatic_duplicate = runtime
        .programmatic
        .lock()
        .iter()
        .any(|entry| entry.options.id == *id);
    if declarative_duplicate || programmatic_duplicate {
        Err(LoaderError::InvalidDocument(format!(
            "duplicate loader entry id: {id}"
        )))
    } else {
        Ok(())
    }
}

fn collect_entry_snapshot(entries: &[MountedEntry]) -> Vec<LoaderEntrySnapshot> {
    fn snapshot_options(entry: &Entry, disabled: bool, output: &mut Vec<LoaderEntrySnapshot>) {
        output.push(LoaderEntrySnapshot {
            id: entry.id.clone(),
            plugin: entry.plugin.clone(),
            config: entry.config.clone(),
            group: entry.group,
            disabled,
            state: None,
        });
        for child in &entry.children {
            snapshot_options(child, disabled, output);
        }
    }

    fn visit(
        entry: &MountedEntry,
        inherited_disabled: bool,
        output: &mut Vec<LoaderEntrySnapshot>,
    ) {
        let disabled = inherited_disabled || entry.effective_disabled;
        output.push(LoaderEntrySnapshot {
            id: entry.options.id.clone(),
            plugin: entry.options.plugin.clone(),
            config: entry.options.config.clone(),
            group: entry.options.group,
            disabled,
            state: entry
                .fiber
                .as_ref()
                .map(|fiber| fiber.fiber().state())
                .or_else(|| {
                    entry
                        .options
                        .include
                        .as_ref()
                        .map(|_| seekdeep_cordis::FiberState::Active)
                }),
        });
        if disabled && entry.children.is_empty() {
            for child in &entry.options.children {
                snapshot_options(child, true, output);
            }
        } else {
            for child in &entry.children {
                visit(child, disabled, output);
            }
        }
    }

    let mut output = Vec::new();
    for entry in entries {
        visit(entry, false, &mut output);
    }
    output
}

/// Active mutable plugin tree mounted from one composition.
#[derive(Debug)]
pub struct LoadedComposition {
    runtime: Arc<CompositionRuntime>,
    settlement_effect: Option<EffectHandle>,
}

impl LoadedComposition {
    /// Snapshots every configured row in declaration preorder.
    #[must_use]
    pub fn entries(&self) -> Vec<LoaderEntrySnapshot> {
        self.runtime.all_entry_snapshots()
    }

    /// Active mounts in declaration/preorder.
    #[must_use]
    pub fn fibers(&self) -> Vec<Arc<PluginFiber>> {
        self.runtime.fibers()
    }

    /// Transactionally reconciles this tree with a source-compatible YAML
    /// generation. Unaffected entries retain their exact fibers.
    ///
    /// # Errors
    ///
    /// Returns parse, import, apply, disposal, or rollback failures. A
    /// successful rollback leaves this value on its previous generation.
    pub async fn update_yaml(&self, source: &str) -> Result<(), LoaderError> {
        self.runtime.update_yaml(source).await
    }

    /// Transactionally appends a YAML entry list to the current root tree.
    /// Host-preparation entries therefore remain active when app boot adds the
    /// file-backed application composition.
    ///
    /// # Errors
    ///
    /// Returns parse, duplicate-id, import, apply, disposal, or rollback failures.
    pub async fn append_yaml(&self, source: &str) -> Result<(), LoaderError> {
        self.runtime.append_yaml(source).await
    }

    /// Classifies and applies one Host module change.
    ///
    /// # Errors
    ///
    /// Returns candidate import, disposal, application, or rollback failures.
    pub async fn reload_module(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<HostHmrOutcome, LoaderError> {
        self.runtime.reload_module(path.as_ref()).await
    }

    /// Creates one programmatic entry in the root or a group.
    ///
    /// # Errors
    ///
    /// Returns duplicate id, invalid parent, import, apply, or rollback failures.
    pub async fn create_entry(
        &self,
        entry: Entry,
        parent: EntryParent,
        position: Option<usize>,
    ) -> Result<(), LoaderError> {
        self.runtime.create_entry(entry, parent, position).await
    }

    /// Updates one entry and optionally moves it transactionally.
    ///
    /// # Errors
    ///
    /// Returns missing entry/parent, import, apply, disposal, or rollback failures.
    pub async fn update_entry(
        &self,
        id: &EntryId,
        update: EntryUpdate,
        parent: EntryParent,
        position: Option<usize>,
    ) -> Result<(), LoaderError> {
        self.runtime
            .update_entry(id, update, parent, position)
            .await
    }

    /// Removes one programmatic entry and its descendants transactionally.
    ///
    /// # Errors
    ///
    /// Returns missing entry, disposal, or rollback failures.
    pub async fn remove_entry(&self, id: &EntryId) -> Result<(), LoaderError> {
        self.runtime.remove_entry(id).await
    }

    /// Disposes all mounts in reverse declaration order.
    ///
    /// # Errors
    ///
    /// Returns all cleanup failures as one causal diagnostic.
    pub async fn dispose(mut self) -> Result<(), LoaderError> {
        let mut errors = Vec::new();
        if let Err(error) = self.runtime.dispose().await {
            errors.push(error.to_string());
        }
        if let Some(effect) = self.settlement_effect.take()
            && let Err(error) = effect.dispose().await
        {
            errors.push(format!("loader settlement cleanup failed: {error:#}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(LoaderError::Disposal(errors.join("; ")))
        }
    }
}

fn entry_specs(entries: &[MountedEntry]) -> Vec<Entry> {
    entries
        .iter()
        .map(|entry| {
            let mut options = entry.options.clone();
            if !entry.effective_disabled {
                options.children = entry_specs(&entry.children);
            }
            options
        })
        .collect()
}

fn find_entry<'a>(entries: &'a [Entry], id: &EntryId) -> Option<&'a Entry> {
    for entry in entries {
        if &entry.id == id {
            return Some(entry);
        }
        if let Some(entry) = find_entry(&entry.children, id) {
            return Some(entry);
        }
    }
    None
}

fn has_include_path(entries: &[Entry], path: &Path) -> bool {
    entries.iter().any(|entry| {
        entry
            .include
            .as_ref()
            .and_then(|include| include.resolved_path.as_deref())
            == Some(path)
            || has_include_path(&entry.children, path)
    })
}

fn take_entry(entries: &mut Vec<Entry>, id: &EntryId) -> Option<(Entry, Vec<EntryId>, usize)> {
    fn visit(
        entries: &mut Vec<Entry>,
        id: &EntryId,
        path: &mut Vec<EntryId>,
    ) -> Option<(Entry, Vec<EntryId>, usize)> {
        if let Some(index) = entries.iter().position(|entry| &entry.id == id) {
            return Some((entries.remove(index), path.clone(), index));
        }
        for entry in entries {
            path.push(entry.id.clone());
            if let Some(found) = visit(&mut entry.children, id, path) {
                return Some(found);
            }
            path.pop();
        }
        None
    }
    visit(entries, id, &mut Vec::new())
}

fn insert_entry(
    entries: &mut Vec<Entry>,
    entry: Entry,
    parent: &EntryParent,
    position: Option<usize>,
) -> Result<(), LoaderError> {
    match parent {
        EntryParent::Keep | EntryParent::Root => {
            let position = position.unwrap_or(entries.len()).min(entries.len());
            entries.insert(position, entry);
            Ok(())
        }
        EntryParent::Group(parent) => {
            let group = find_entry_mut(entries, parent).ok_or_else(|| {
                LoaderError::InvalidDocument(format!("cannot resolve entry {parent}"))
            })?;
            if !group.group {
                return Err(LoaderError::InvalidDocument(format!(
                    "entry {parent} is not a group"
                )));
            }
            let position = position
                .unwrap_or(group.children.len())
                .min(group.children.len());
            group.children.insert(position, entry);
            Ok(())
        }
    }
}

fn insert_entry_at_path(
    entries: &mut Vec<Entry>,
    entry: Entry,
    path: &[EntryId],
    position: usize,
) -> Result<(), LoaderError> {
    let mut target = entries;
    for id in path {
        let group = target
            .iter_mut()
            .find(|entry| &entry.id == id)
            .ok_or_else(|| LoaderError::InvalidDocument(format!("cannot resolve entry {id}")))?;
        target = &mut group.children;
    }
    target.insert(position.min(target.len()), entry);
    Ok(())
}

fn find_entry_mut<'a>(entries: &'a mut [Entry], id: &EntryId) -> Option<&'a mut Entry> {
    for entry in entries {
        if &entry.id == id {
            return Some(entry);
        }
        if let Some(entry) = find_entry_mut(&mut entry.children, id) {
            return Some(entry);
        }
    }
    None
}
