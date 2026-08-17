//! Declarative plugin-tree parsing, patch layering, and executable loading.

use std::{collections::HashMap, fmt, future::Future, path::Path, pin::Pin, sync::Arc};

use indexmap::IndexMap;
use parking_lot::RwLock;
use seekdeep_cordis::{Context, Plugin, PluginFiber};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

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
    /// Nested rows mounted in the plugin's lifecycle scope.
    #[serde(default)]
    pub children: Vec<Entry>,
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
    /// A catalog key was registered twice.
    #[error("loader: plugin {0:?} is already registered")]
    DuplicatePlugin(String),
    /// An entry named a plugin absent from the catalog.
    #[error("loader: plugin {0:?} is not registered")]
    UnknownPlugin(String),
    /// A document could not be parsed.
    #[error("loader: invalid composition document: {0}")]
    InvalidDocument(String),
    /// A mounted plugin failed to settle.
    #[error("loader: entry {entry:?} ({plugin:?}) failed: {message}")]
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
#[derive(Clone, Default)]
pub struct PluginCatalog {
    plugins: Arc<RwLock<HashMap<PluginSpecifier, Plugin>>>,
}

impl fmt::Debug for PluginCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginCatalog")
            .field("len", &self.plugins.read().len())
            .finish()
    }
}

impl PluginCatalog {
    /// Creates an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        let entries = parse_entries(source)?;
        self.mount(context, &entries).await
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
        Ok(self.load_yaml(context, &source).await?)
    }

    async fn mount(
        &self,
        context: &Context,
        entries: &[Entry],
    ) -> Result<LoadedComposition, LoaderError> {
        let mut fibers = Vec::new();
        if let Err(error) = mount_entries(self, context, entries, &mut fibers).await {
            dispose_fibers(&mut fibers).await?;
            return Err(error);
        }
        Ok(LoadedComposition { fibers })
    }
}

type MountFuture<'a> = Pin<Box<dyn Future<Output = Result<(), LoaderError>> + Send + 'a>>;

fn mount_entries<'a>(
    catalog: &'a PluginCatalog,
    context: &'a Context,
    entries: &'a [Entry],
    fibers: &'a mut Vec<Arc<PluginFiber>>,
) -> MountFuture<'a> {
    Box::pin(async move {
        for entry in entries.iter().filter(|entry| !entry.disabled) {
            let plugin = catalog.resolve(&entry.plugin)?;
            let fiber = context
                .plugin(plugin, entry.config.clone())
                .map_err(|error| LoaderError::PluginStartup {
                    entry: entry.id.to_string(),
                    plugin: entry.plugin.to_string(),
                    message: error.to_string(),
                })?;
            if let Err(error) = fiber.await_settled().await {
                fibers.push(fiber);
                return Err(LoaderError::PluginStartup {
                    entry: entry.id.to_string(),
                    plugin: entry.plugin.to_string(),
                    message: format!("{error:#}"),
                });
            }
            let child = fiber.context().clone();
            fibers.push(fiber);
            mount_entries(catalog, &child, &entry.children, fibers).await?;
        }
        Ok(())
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CompositionDocument {
    Entries(Vec<Entry>),
    Tree(ConfigTree),
}

fn parse_entries(source: &str) -> Result<Vec<Entry>, LoaderError> {
    match serde_yml::from_str::<CompositionDocument>(source)
        .map_err(|error| LoaderError::InvalidDocument(error.to_string()))?
    {
        CompositionDocument::Entries(entries) => Ok(entries),
        CompositionDocument::Tree(tree) => Ok(tree.entries),
    }
}

/// Active set of plugin fibers mounted from one composition snapshot.
#[derive(Debug)]
pub struct LoadedComposition {
    fibers: Vec<Arc<PluginFiber>>,
}

impl LoadedComposition {
    /// Active mounts in declaration/preorder.
    #[must_use]
    pub fn fibers(&self) -> &[Arc<PluginFiber>] {
        &self.fibers
    }

    /// Disposes all mounts in reverse declaration order.
    ///
    /// # Errors
    ///
    /// Returns all cleanup failures as one causal diagnostic.
    pub async fn dispose(mut self) -> Result<(), LoaderError> {
        dispose_fibers(&mut self.fibers).await
    }
}

async fn dispose_fibers(fibers: &mut Vec<Arc<PluginFiber>>) -> Result<(), LoaderError> {
    let mut errors = Vec::new();
    while let Some(fiber) = fibers.pop() {
        if let Err(error) = fiber.dispose().await {
            errors.push(format!("{error:#}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(LoaderError::Disposal(errors.join("; ")))
    }
}
