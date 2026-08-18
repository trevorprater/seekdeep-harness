//! File-backed settings provider.
//!
//! One YAML or JSON document stores every namespace. Writes reconcile under a
//! cross-process writer lock and replace atomically; YAML updates are applied
//! as map-leaf edits so untouched formatting and comments survive.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use notify::{RecursiveMode, Watcher};
use parking_lot::Mutex;
use path_clean::PathClean;
use rowan::ast::AstNode as _;
use seekdeep_cordis::{
    Context, Plugin,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_invariants::InvariantError;
use seekdeep_settings::{
    SettingsDocument, SettingsNamespace, SettingsPublisher, SettingsService, SettingsStorage,
};
use seekdeep_util::{
    atomic_write::{WriteFileAtomicOptions, with_file_lock, write_file_atomic},
    home_paths::{canonicalize_watch_path, resolve_process_seekdeep_home},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::{sync::watch, task::JoinHandle};
use yaml_edit::{AsYaml, Mapping, MappingEntry, SyntaxKind, YamlFile, YamlKind, YamlNode};

/// Package-owned invariant companion.
pub mod invariant;

pub use invariant::{INVARIANT_NAME, register_invariant};

/// Cordis plugin name.
pub const NAME: &str = "settings-file";
/// This provider has no required startup service.
pub const INJECT: &[&str] = &[];
/// Default document basename under the harness home.
pub const SETTINGS_FILENAME: &str = "settings.yaml";

/// File location and hot-reload behavior.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FileSettingsConfig {
    /// Explicit settings document path.
    pub path: Option<PathBuf>,
    /// Explicit `SeekDeep` home used when `path` is absent.
    pub seekdeep_home: Option<PathBuf>,
    /// Whether external edits are watched.
    pub watch: bool,
    /// Stable-write debounce interval in milliseconds.
    pub debounce_ms: f64,
}

impl Default for FileSettingsConfig {
    fn default() -> Self {
        Self {
            path: None,
            seekdeep_home: None,
            watch: true,
            debounce_ms: 100.0,
        }
    }
}

/// Document syntax derived from the configured extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsFormat {
    /// YAML document.
    Yaml,
    /// JSON document.
    Json,
}

/// Fully resolved provider parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSpec {
    /// Absolute document filename.
    pub filename: PathBuf,
    /// Syntax selected by extension.
    pub format: SettingsFormat,
    /// Whether external edits are watched.
    pub watch: bool,
    /// Stable-write debounce interval in milliseconds.
    pub debounce_ms: f64,
}

/// Resolves defaults, absolute path, and document format.
///
/// # Errors
///
/// Returns home/current-directory resolution or unsupported-extension failures.
pub fn resolve_spec(config: &FileSettingsConfig) -> anyhow::Result<ResolvedSpec> {
    let filename = if let Some(path) = &config.path {
        absolute_clean(path)?
    } else {
        resolve_process_seekdeep_home(config.seekdeep_home.as_deref().map(Path::as_os_str))?
            .join(SETTINGS_FILENAME)
            .clean()
    };
    let extension = filename.extension().and_then(OsStr::to_str).unwrap_or("");
    let format = match extension {
        "yaml" | "yml" => SettingsFormat::Yaml,
        "json" => SettingsFormat::Json,
        _ => anyhow::bail!(
            "settings-file: extension \"{}\" is not supported (use .yaml, .yml, or .json)",
            filename
                .extension()
                .map_or_else(String::new, |value| format!(".{}", value.to_string_lossy()))
        ),
    };
    Ok(ResolvedSpec {
        filename,
        format,
        watch: config.watch,
        debounce_ms: config.debounce_ms,
    })
}

fn absolute_clean(path: &Path) -> std::io::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean())
}

#[derive(Default)]
struct ProviderState {
    text: Option<String>,
    publisher: Option<SettingsPublisher>,
}

/// YAML/JSON-backed settings storage implementation.
pub struct FileSettingsStorage {
    spec: ResolvedSpec,
    state: Mutex<ProviderState>,
    operation: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl std::fmt::Debug for FileSettingsStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSettingsStorage")
            .field("spec", &self.spec)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl FileSettingsStorage {
    /// Constructs unopened storage from a resolved spec.
    #[must_use]
    pub fn new(spec: ResolvedSpec) -> Arc<Self> {
        Arc::new(Self {
            spec,
            state: Mutex::new(ProviderState::default()),
            operation: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        })
    }

    /// Resolved runtime spec.
    #[must_use]
    pub fn spec(&self) -> &ResolvedSpec {
        &self.spec
    }

    fn set_publisher(&self, publisher: SettingsPublisher) {
        self.state.lock().publisher = Some(publisher);
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn begin_shutdown(&self) {
        self.closed.store(true, Ordering::Release);
    }

    async fn drain(&self) {
        let _operation = self.operation.lock().await;
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        if self.is_closed() {
            return Ok(());
        }
        let _operation = self.operation.lock().await;
        match self.reconcile_from_disk().await {
            Ok(()) => Ok(()),
            Err(error) if error.downcast_ref::<InvariantError>().is_some() => Err(error),
            Err(error) => {
                tracing::warn!(path = %self.spec.filename.display(), "settings-file: reload failed; keeping the last good document");
                tracing::warn!(%error, "settings-file reload error");
                Ok(())
            }
        }
    }

    async fn reconcile_from_disk(&self) -> anyhow::Result<()> {
        let text = read_optional(&self.spec.filename).await?;
        {
            let state = self.state.lock();
            if text == state.text || self.is_closed() {
                return Ok(());
            }
        }
        let document = text
            .as_deref()
            .map_or_else(|| Ok(Map::new()), |text| parse_document(text, &self.spec))?;
        let publisher = {
            let mut state = self.state.lock();
            state.text = text;
            state.publisher.clone()
        };
        if let Some(publisher) = publisher {
            publisher.publish(document)?;
        }
        Ok(())
    }

    async fn persist_section(
        &self,
        namespace: &SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        create_private_parents(&self.spec.filename).await?;
        with_file_lock(&self.spec.filename, || async {
            self.reconcile_from_disk().await?;
            let text = self.state.lock().text.clone();
            let output = match self.spec.format {
                SettingsFormat::Yaml => {
                    render_yaml(text.as_deref(), namespace, section, &self.spec)?
                }
                SettingsFormat::Json => {
                    render_json(text.as_deref(), namespace, section, &self.spec)?
                }
            };
            write_file_atomic(
                &self.spec.filename,
                output.as_bytes(),
                WriteFileAtomicOptions {
                    mode: 0o600,
                    dir_mode: Some(0o700),
                },
            )
            .await?;
            self.state.lock().text = Some(output);
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
    }
}

#[async_trait]
impl SettingsStorage for FileSettingsStorage {
    fn writable(&self) -> bool {
        true
    }

    fn document_path(&self) -> Option<&Path> {
        Some(&self.spec.filename)
    }

    async fn prepare_document(&self) -> anyhow::Result<Option<PathBuf>> {
        let _operation = self.operation.lock().await;
        create_private_parents(&self.spec.filename).await?;
        with_file_lock(&self.spec.filename, || async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            set_open_mode(&mut options, 0o600);
            match options.open(&self.spec.filename).await {
                Ok(_) => {
                    let publisher = {
                        let mut state = self.state.lock();
                        state.text = Some(String::new());
                        (!self.is_closed())
                            .then(|| state.publisher.clone())
                            .flatten()
                    };
                    if let Some(publisher) = publisher {
                        publisher.publish(Map::new())?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(self.spec.filename.clone()))
    }

    async fn load(&self) -> anyhow::Result<SettingsDocument> {
        let text = read_optional(&self.spec.filename).await?;
        let document = text
            .as_deref()
            .map_or_else(|| Ok(Map::new()), |text| parse_document(text, &self.spec))?;
        self.state.lock().text = text;
        Ok(document)
    }

    async fn persist(
        &self,
        namespace: &SettingsNamespace,
        section: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        let _operation = self.operation.lock().await;
        self.persist_section(namespace, section).await
    }
}

fn parse_document(text: &str, spec: &ResolvedSpec) -> anyhow::Result<SettingsDocument> {
    let root = match spec.format {
        SettingsFormat::Yaml => serde_yml::from_str::<Value>(text).map_err(|error| {
            let at = error.location().map_or_else(String::new, |location| {
                format!(" at line {}, column {}", location.line(), location.column())
            });
            anyhow::anyhow!(
                "settings-file: invalid document at {}: YAML_PARSE{at}",
                spec.filename.display()
            )
        })?,
        SettingsFormat::Json if text.trim().is_empty() => Value::Object(Map::new()),
        SettingsFormat::Json => serde_json::from_str::<Value>(text).map_err(|error| {
            anyhow::anyhow!(
                "settings-file: invalid document at {}: JSON_PARSE at line {}, column {}",
                spec.filename.display(),
                error.line(),
                error.column()
            )
        })?,
    };
    match root {
        Value::Null => Ok(Map::new()),
        Value::Object(document) => Ok(document),
        _ => anyhow::bail!(
            "settings-file: {} must be a map of namespace sections",
            spec.filename.display()
        ),
    }
}

fn render_yaml(
    text: Option<&str>,
    namespace: &SettingsNamespace,
    section: &Map<String, Value>,
    spec: &ResolvedSpec,
) -> anyhow::Result<String> {
    let Some(text) = text else {
        let mut document = Map::new();
        document.insert(namespace.to_string(), Value::Object(section.clone()));
        return Ok(serde_yml::to_string(&document)?);
    };
    let current = parse_document(text, spec)?;
    let file = YamlFile::from_str(text)
        .map_err(|_| anyhow::anyhow!("settings-file: validated YAML could not be edited"))?;
    file.ensure_document();
    let document = file
        .document()
        .ok_or_else(|| anyhow::anyhow!("settings-file: YAML editor has no document"))?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("settings-file: YAML editor root is not a mapping"))?;
    match (
        current.get(namespace.as_str()).and_then(Value::as_object),
        mapping.get_mapping(namespace.as_str()),
    ) {
        (Some(previous), Some(target)) => patch_mapping(&target, previous, section)?,
        _ => set_yaml_value(
            &mapping,
            namespace.as_str(),
            &Value::Object(section.clone()),
        )?,
    }
    let mut rendered = file.to_string();
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn patch_mapping(
    mapping: &Mapping,
    current: &Map<String, Value>,
    next: &Map<String, Value>,
) -> anyhow::Result<()> {
    for key in current.keys() {
        if !next.contains_key(key) {
            remove_yaml_key(mapping, key);
        }
    }
    for (key, value) in next {
        let previous = current.get(key);
        if previous == Some(value) {
            continue;
        }
        if let (Some(previous), Some(next), Some(_sequence)) = (
            previous.and_then(Value::as_array),
            value.as_array(),
            mapping.get_sequence(key.as_str()),
        ) {
            debug_assert_ne!(previous, next);
            replace_sequence(mapping, key, next)?;
            continue;
        }
        if let (Some(previous), Some(next), Some(child)) = (
            previous.and_then(Value::as_object),
            value.as_object(),
            mapping.get_mapping(key.as_str()),
        ) {
            patch_mapping(&child, previous, next)?;
        } else {
            set_yaml_value(mapping, key, value)?;
        }
    }
    Ok(())
}

fn replace_sequence(mapping: &Mapping, key: &str, values: &[Value]) -> anyhow::Result<()> {
    let value = Value::Array(values.to_vec());
    let entry = mapping
        .find_entry_by_key(key)
        .ok_or_else(|| anyhow::anyhow!("settings-file: YAML sequence entry disappeared"))?;
    let key_node = entry
        .key_node()
        .ok_or_else(|| anyhow::anyhow!("settings-file: YAML sequence key disappeared"))?;
    let use_explicit_key = entry.syntax().children_with_tokens().any(|child| {
        child
            .as_token()
            .is_some_and(|token| token.kind() == SyntaxKind::QUESTION)
    });
    let replacement = MappingEntry::new(
        key_node,
        IndentedYaml {
            value: yaml_node(&value)?,
            indent: mapping.detect_indentation_level() + 2,
        },
        false,
        use_explicit_key,
    );
    let index = mapping
        .syntax()
        .children_with_tokens()
        .position(|child| child.as_node() == Some(entry.syntax()))
        .ok_or_else(|| anyhow::anyhow!("settings-file: YAML sequence pair disappeared"))?;
    mapping
        .syntax()
        .splice_children(index..index + 1, vec![replacement.syntax().clone().into()]);
    Ok(())
}

fn set_yaml_value(mapping: &Mapping, key: &str, value: &Value) -> anyhow::Result<()> {
    let value = yaml_node(value)?;
    if mapping
        .get(key)
        .is_some_and(|current| !current.is_inline() || !value.is_inline())
    {
        remove_yaml_key(mapping, key);
    }
    mapping.set(
        key,
        IndentedYaml {
            value,
            indent: mapping.detect_indentation_level() + 2,
        },
    );
    Ok(())
}

fn remove_yaml_key(mapping: &Mapping, key: &str) -> bool {
    let children: Vec<_> = mapping.syntax().children_with_tokens().collect();
    for (index, child) in children.iter().enumerate() {
        let Some(node) = child.as_node() else {
            continue;
        };
        let Some(entry) = MappingEntry::cast(node.clone()) else {
            continue;
        };
        if !entry.key_matches(key) {
            continue;
        }
        // `yaml-edit` models indentation before a nested pair as a sibling
        // token. Its public `remove` detaches only the pair, so that token can
        // incorrectly indent the following entry. Remove the pair-owned
        // indentation alongside it.
        let has_previous_entry = children[..index].iter().any(|child| {
            child
                .as_node()
                .is_some_and(|node| node.kind() == SyntaxKind::MAPPING_ENTRY)
        });
        let owned_indent = if has_previous_entry {
            index.checked_sub(1).and_then(|previous| {
                children[previous]
                    .as_token()
                    .filter(|token| token.kind() == SyntaxKind::INDENT)
                    .cloned()
            })
        } else {
            children.get(index + 1).and_then(|next| {
                next.as_token()
                    .filter(|token| token.kind() == SyntaxKind::INDENT)
                    .cloned()
            })
        };
        entry.remove();
        if let Some(indent) = owned_indent {
            indent.detach();
        }
        return true;
    }
    false
}

struct IndentedYaml {
    value: YamlNode,
    indent: usize,
}

impl AsYaml for IndentedYaml {
    fn as_node(&self) -> Option<&rowan::SyntaxNode<yaml_edit::Lang>> {
        None
    }

    fn kind(&self) -> YamlKind {
        self.value.kind()
    }

    fn build_content(
        &self,
        builder: &mut rowan::GreenNodeBuilder,
        _indent: usize,
        flow_context: bool,
    ) -> bool {
        if self.value.is_inline() {
            self.value.build_content(builder, 0, flow_context)
        } else {
            builder.token(SyntaxKind::INDENT.into(), &" ".repeat(self.indent));
            match &self.value {
                YamlNode::Mapping(value) => value.build_content(builder, self.indent, flow_context),
                YamlNode::Sequence(value) => {
                    value.build_content(builder, self.indent, flow_context)
                }
                value => value.build_content(builder, self.indent, flow_context),
            }
        }
    }

    fn is_inline(&self) -> bool {
        self.value.is_inline()
    }
}

fn yaml_node(value: &Value) -> anyhow::Result<YamlNode> {
    let mut wrapper = BTreeMap::new();
    wrapper.insert("value", value);
    let text = serde_yml::to_string(&wrapper)?;
    let file = YamlFile::from_str(&text)
        .map_err(|_| anyhow::anyhow!("settings-file: rendered YAML could not be edited"))?;
    file.document()
        .and_then(|document| document.get("value"))
        .ok_or_else(|| anyhow::anyhow!("settings-file: rendered YAML has no value node"))
}

fn render_json(
    text: Option<&str>,
    namespace: &SettingsNamespace,
    section: &Map<String, Value>,
    spec: &ResolvedSpec,
) -> anyhow::Result<String> {
    let mut document = text.map_or_else(|| Ok(Map::new()), |text| parse_document(text, spec))?;
    document.insert(namespace.to_string(), Value::Object(section.clone()));
    Ok(format!("{}\n", serde_json::to_string_pretty(&document)?))
}

async fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn create_private_parents(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    set_directory_mode(&mut builder, 0o700);
    builder.create(parent).await
}

#[cfg(unix)]
fn set_open_mode(options: &mut tokio::fs::OpenOptions, mode: u32) {
    options.mode(mode);
}

#[cfg(not(unix))]
fn set_open_mode(_options: &mut tokio::fs::OpenOptions, _mode: u32) {}

#[cfg(unix)]
fn set_directory_mode(builder: &mut tokio::fs::DirBuilder, mode: u32) {
    builder.mode(mode);
}

#[cfg(not(unix))]
fn set_directory_mode(_builder: &mut tokio::fs::DirBuilder, _mode: u32) {}

fn watch_root(target: &Path) -> anyhow::Result<PathBuf> {
    let mut current = target.to_path_buf();
    loop {
        match std::fs::metadata(&current) {
            Ok(metadata) if metadata.is_dir() => return Ok(current),
            Ok(_) => anyhow::ensure!(current.pop(), "watch path has no directory ancestor"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::ensure!(current.pop(), "watch path has no existing ancestor");
            }
            Err(error) => return Err(error.into()),
        }
    }
}

struct WatchLifecycle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl WatchLifecycle {
    async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

async fn start_watcher(
    storage: &Arc<FileSettingsStorage>,
) -> anyhow::Result<Option<WatchLifecycle>> {
    if !storage.spec.watch {
        return Ok(None);
    }
    let target = canonicalize_watch_path(&storage.spec.filename).await?;
    let root = watch_root(&target)?;
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = events_tx.send(event);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    if let Err(error) = storage.refresh().await {
        tracing::error!(path = %target.display(), %error, "settings-file: reload commit failed");
    }
    let (stop, stop_rx) = watch::channel(false);
    let weak = Arc::downgrade(storage);
    let debounce = Duration::from_secs_f64(storage.spec.debounce_ms / 1_000.0);
    let task = tokio::spawn(async move {
        let _watcher = watcher;
        watcher_loop(weak, events_rx, stop_rx, target, debounce).await;
    });
    Ok(Some(WatchLifecycle { stop, task }))
}

async fn watcher_loop(
    storage: Weak<FileSettingsStorage>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    mut stop: watch::Receiver<bool>,
    target: PathBuf,
    debounce: Duration,
) {
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    Err(error) => tracing::warn!(path = %target.display(), %error, "settings-file: watcher error"),
                    Ok(event) if relevant_event(&event, &target) => {
                        if wait_for_settle(&mut events, &mut stop, &target, debounce).await { break; }
                        if let Some(storage) = storage.upgrade()
                            && let Err(error) = storage.refresh().await
                        {
                            tracing::error!(path = %target.display(), %error, "settings-file: reload commit failed");
                        }
                    }
                    Ok(_) => {}
                }
            }
        }
    }
}

async fn wait_for_settle(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    stop: &mut watch::Receiver<bool>,
    target: &Path,
    debounce: Duration,
) -> bool {
    let timer = tokio::time::sleep(debounce);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            changed = stop.changed() => return changed.is_err() || *stop.borrow(),
            () = &mut timer => return false,
            event = events.recv() => match event {
                None => return false,
                Some(Err(error)) => tracing::warn!(path = %target.display(), %error, "settings-file: watcher error"),
                Some(Ok(event)) if relevant_event(&event, target) => {
                    timer.as_mut().reset(tokio::time::Instant::now() + debounce);
                }
                Some(Ok(_)) => {}
            }
        }
    }
}

fn relevant_event(event: &notify::Event, target: &Path) -> bool {
    event.paths.is_empty() || event.paths.iter().any(|path| path.clean() == target)
}

/// Builds the file-settings plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: FileSettingsConfig = serde_json::from_value(config)?;
            let spec = resolve_spec(&config)?;
            let storage = FileSettingsStorage::new(spec);
            let service = SettingsService::install(&context, storage.clone()).await?;
            storage.set_publisher(service.publisher());
            let watcher = start_watcher(&storage).await?;
            let cleanup = storage.clone();
            context.own(EffectHandle::new(
                "settings-file drain",
                move || -> DisposeFuture {
                    Box::pin(async move {
                        cleanup.begin_shutdown();
                        if let Some(watcher) = watcher {
                            watcher.stop().await;
                        }
                        cleanup.drain().await;
                        Ok(())
                    })
                },
            ))?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        let config: FileSettingsConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(
            config.debounce_ms.is_finite() && config.debounce_ms >= 0.0,
            "settings-file: debounceMs must be a finite number greater than or equal to 0"
        );
        resolve_spec(&config)?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Installs the provider as a lifecycle-owned plugin fiber.
///
/// # Errors
///
/// Returns configuration serialization or inactive-context failures.
pub fn install(
    context: &Context,
    config: FileSettingsConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use notify::event::{DataChange, ModifyKind};
    use seekdeep_cordis::{EventOptions, EventReply};

    use super::*;

    #[tokio::test]
    async fn watcher_queue_survives_backend_error_then_stops_before_later_events() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("settings.yaml");
        tokio::fs::write(&target, "ui:\n  theme: light\n")
            .await
            .unwrap();
        let spec = resolve_spec(&FileSettingsConfig {
            path: Some(target.clone()),
            watch: false,
            debounce_ms: 0.0,
            ..FileSettingsConfig::default()
        })
        .unwrap();
        let storage = FileSettingsStorage::new(spec);
        storage.load().await.unwrap();
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = watch::channel(false);
        let task_storage = Arc::downgrade(&storage);
        let task_target = target.clone();
        let task = tokio::spawn(async move {
            watcher_loop(
                task_storage,
                events_rx,
                stop_rx,
                task_target,
                Duration::ZERO,
            )
            .await;
        });
        events_tx
            .send(Err(notify::Error::generic("watch backend failure")))
            .unwrap();
        tokio::fs::write(&target, "ui:\n  theme: darker\n")
            .await
            .unwrap();
        events_tx
            .send(Ok(notify::Event::new(notify::EventKind::Modify(
                ModifyKind::Data(DataChange::Any),
            ))
            .add_path(target.clone())))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if storage.state.lock().text.as_deref() == Some("ui:\n  theme: darker\n") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        storage.begin_shutdown();
        stop_tx.send(true).unwrap();
        task.await.unwrap();
        tokio::fs::write(&target, "ui:\n  theme: ignored\n")
            .await
            .unwrap();
        assert_eq!(
            storage.state.lock().text.as_deref(),
            Some("ui:\n  theme: darker\n")
        );
    }

    #[tokio::test]
    async fn queued_document_create_after_shutdown_creates_file_without_publication() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("settings.yaml");
        let spec = resolve_spec(&FileSettingsConfig {
            path: Some(target.clone()),
            watch: false,
            ..FileSettingsConfig::default()
        })
        .unwrap();
        let storage = FileSettingsStorage::new(spec);
        let context = Context::new();
        let service = SettingsService::install(&context, storage.clone())
            .await
            .unwrap();
        storage.set_publisher(service.publisher());
        let publications = Arc::new(AtomicUsize::new(0));
        let observed = publications.clone();
        context
            .events()
            .on_sync(
                &context,
                "settings/document-updated",
                move |_, _| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .unwrap();
        let held = storage.operation.lock().await;
        let preparing_storage = storage.clone();
        let preparing = tokio::spawn(async move { preparing_storage.prepare_document().await });
        tokio::task::yield_now().await;
        storage.begin_shutdown();
        drop(held);
        assert_eq!(preparing.await.unwrap().unwrap(), Some(target.clone()));
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"");
        assert_eq!(publications.load(Ordering::SeqCst), 0);
    }
}
