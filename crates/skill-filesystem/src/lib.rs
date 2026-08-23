//! Local directory-bundle and flat-Markdown Skill discovery with native watching.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use notify::{RecursiveMode, Watcher as _};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_fs::{FS, FsKind};
use seekdeep_skill::{
    BUNDLED_SKILL_RANK, SKILLS, SkillCandidate, SkillDefinition, SkillInvocationPolicy,
    SkillLookupOptions, SkillProvider, SkillProviderObservation, SkillRegistry, SkillResourceBase,
    SkillSource, SkillSummary, is_skill_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const PROJECT_SEEKDEEP_RANK: f64 = 100.0;
const PROJECT_AGENTS_RANK: f64 = 200.0;
const CUSTOM_RANK: f64 = 300.0;
const USER_SEEKDEEP_RANK: f64 = 400.0;
const USER_AGENTS_RANK: f64 = 500.0;
const DEFAULT_WATCH_MAX_PROJECTS: usize = 128;
const DEFAULT_WATCH_STABILITY_THRESHOLD_MS: u64 = 200;
const DEFAULT_WATCH_POLL_INTERVAL_MS: u64 = 100;
/// Loader plugin name.
pub const NAME: &str = "skill-filesystem";
/// Required registry service.
pub const INJECT: &[&str] = &["skills"];

/// Native provider configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Preserves the source configuration's independent boolean switches.
pub struct Config {
    /// Unique provider name.
    pub provider_name: String,
    /// Include project, user, and optional bundled roots.
    pub include_default_roots: bool,
    /// `SeekDeep` home override.
    pub seekdeep_home: Option<PathBuf>,
    /// Shared Agent home override.
    pub agents_home: Option<PathBuf>,
    /// Explicit roots between project and user roots.
    pub custom_skill_dirs: Vec<PathBuf>,
    /// Observe host filesystem changes.
    pub watch: bool,
    /// Use polling rather than the platform-native watcher.
    pub watch_use_polling: bool,
    /// Stable-write debounce before invalidation.
    pub watch_stability_threshold_ms: u64,
    /// Polling interval when polling is selected.
    pub watch_poll_interval_ms: u64,
    /// Maximum project roots retained by the watcher.
    pub watch_max_projects: usize,
    /// Follow symbolic links during discovery and watching.
    pub watch_follow_symlinks: bool,
    /// Optional bundled root.
    pub bundled_skill_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider_name: "filesystem".to_owned(),
            include_default_roots: true,
            seekdeep_home: None,
            agents_home: None,
            custom_skill_dirs: Vec::new(),
            watch: true,
            watch_use_polling: false,
            watch_stability_threshold_ms: DEFAULT_WATCH_STABILITY_THRESHOLD_MS,
            watch_poll_interval_ms: DEFAULT_WATCH_POLL_INTERVAL_MS,
            watch_max_projects: DEFAULT_WATCH_MAX_PROJECTS,
            watch_follow_symlinks: true,
            bundled_skill_dir: None,
        }
    }
}

#[derive(Clone, Debug)]
struct SkillRoot {
    path: PathBuf,
    source: SkillSource,
    rank: f64,
    skip_system: bool,
    project_root: Option<PathBuf>,
    trusted_host: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LocalLocator {
    path: PathBuf,
    directory: PathBuf,
}

struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    invocation: SkillInvocationPolicy,
    metadata: Option<Value>,
    content: String,
}

#[derive(Default)]
struct WatchState {
    watchers: HashMap<PathBuf, Box<dyn notify::Watcher + Send>>,
    project_order: Vec<PathBuf>,
    project_paths: HashMap<PathBuf, HashSet<PathBuf>>,
    closing: bool,
}

/// Filesystem-backed Skill provider.
pub struct FileSystemSkillProvider {
    self_weak: Weak<FileSystemSkillProvider>,
    context: Context,
    config: Config,
    seekdeep_home: PathBuf,
    agents_home: PathBuf,
    bundled_skill_dir: Option<PathBuf>,
    registry: Weak<SkillRegistry>,
    watch: Mutex<WatchState>,
    invalidation_epoch: Arc<AtomicU64>,
}

impl std::fmt::Debug for FileSystemSkillProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSystemSkillProvider")
            .field("name", &self.config.provider_name)
            .field("watch", &self.config.watch)
            .finish_non_exhaustive()
    }
}

impl FileSystemSkillProvider {
    /// Constructs a provider over one registry generation.
    ///
    /// # Errors
    ///
    /// Returns invalid provider/watcher configuration or home resolution failures.
    pub fn new(
        context: &Context,
        registry: &Arc<SkillRegistry>,
        config: Config,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            !config.provider_name.is_empty(),
            "skill-filesystem: providerName must be non-empty"
        );
        anyhow::ensure!(
            config.watch_max_projects >= 1,
            "skill-filesystem: watchMaxProjects must be a positive integer"
        );
        anyhow::ensure!(
            config.watch_stability_threshold_ms >= 1,
            "skill-filesystem: watchStabilityThresholdMs must be a positive integer"
        );
        anyhow::ensure!(
            config.watch_poll_interval_ms >= 1,
            "skill-filesystem: watchPollIntervalMs must be a positive integer"
        );
        let seekdeep_home = match &config.seekdeep_home {
            Some(path) => absolute(path)?,
            None => seekdeep_util::home_paths::resolve_process_seekdeep_home(None)?,
        };
        let agents_home = match &config.agents_home {
            Some(path) => absolute(path)?,
            None => std::env::var_os("SEEKDEEP_AGENTS_HOME").map_or_else(
                || {
                    Ok(seekdeep_util::home_paths::default_seekdeep_home()?
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(".agents"))
                },
                |path| absolute(Path::new(&path)),
            )?,
        };
        let bundled_skill_dir = config
            .bundled_skill_dir
            .as_ref()
            .map(|path| absolute(path))
            .transpose()?
            .or_else(|| {
                config
                    .include_default_roots
                    .then(|| std::env::var_os("SEEKDEEP_BUNDLED_SKILL_DIR"))
                    .flatten()
                    .and_then(|path| absolute(Path::new(&path)).ok())
            });
        Ok(Arc::new_cyclic(|weak| Self {
            self_weak: weak.clone(),
            context: context.clone(),
            config,
            seekdeep_home,
            agents_home,
            bundled_skill_dir,
            registry: Arc::downgrade(registry),
            watch: Mutex::new(WatchState::default()),
            invalidation_epoch: Arc::new(AtomicU64::new(0)),
        }))
    }

    /// Invalidates after a first-party host mutation under an observed Skill path.
    pub fn observe_host_mutation(&self, path: &Path) {
        let watch = self.watch.lock();
        if watch.closing {
            return;
        }
        let normalized = canonical_watch_path(path).unwrap_or_else(|_| path.to_path_buf());
        if watch.watchers.keys().any(|root| {
            let root = canonical_watch_path(root).unwrap_or_else(|_| root.clone());
            potential_skill_path(&root, &normalized)
        }) {
            drop(watch);
            self.invalidate();
        }
    }

    /// Closes every native watcher. Repeated calls are harmless.
    pub fn dispose(&self) {
        let mut state = self.watch.lock();
        state.closing = true;
        state.watchers.clear();
        state.project_order.clear();
        state.project_paths.clear();
    }

    async fn roots(&self, cwd: Option<&str>) -> anyhow::Result<Vec<SkillRoot>> {
        let mut roots = Vec::new();
        if self.config.include_default_roots
            && let Some(cwd) = cwd
        {
            let cwd = absolute(Path::new(cwd))?;
            let project = find_project_root(&self.context, &cwd).await;
            roots.push(SkillRoot {
                path: project.join(".seekdeep/skills"),
                source: SkillSource("project-seekdeep".to_owned()),
                rank: PROJECT_SEEKDEEP_RANK,
                skip_system: false,
                project_root: Some(project.clone()),
                trusted_host: false,
            });
            roots.push(SkillRoot {
                path: project.join(".agents/skills"),
                source: SkillSource("project-agents".to_owned()),
                rank: PROJECT_AGENTS_RANK,
                skip_system: false,
                project_root: Some(project),
                trusted_host: false,
            });
        }
        for path in &self.config.custom_skill_dirs {
            roots.push(SkillRoot {
                path: absolute(path)?,
                source: SkillSource("custom".to_owned()),
                rank: CUSTOM_RANK,
                skip_system: false,
                project_root: None,
                trusted_host: false,
            });
        }
        if self.config.include_default_roots {
            roots.push(SkillRoot {
                path: self.seekdeep_home.join("skills"),
                source: SkillSource("user-seekdeep".to_owned()),
                rank: USER_SEEKDEEP_RANK,
                skip_system: true,
                project_root: None,
                trusted_host: false,
            });
            roots.push(SkillRoot {
                path: self.agents_home.join("skills"),
                source: SkillSource("user-agents".to_owned()),
                rank: USER_AGENTS_RANK,
                skip_system: false,
                project_root: None,
                trusted_host: false,
            });
        }
        if let Some(path) = &self.bundled_skill_dir {
            roots.push(SkillRoot {
                path: path.clone(),
                source: SkillSource("bundled".to_owned()),
                rank: BUNDLED_SKILL_RANK,
                skip_system: false,
                project_root: None,
                trusted_host: true,
            });
        }
        Ok(roots)
    }

    fn observe_roots(self: &Arc<Self>, roots: &[SkillRoot]) -> bool {
        if !self.config.watch {
            return true;
        }
        let mut complete = true;
        for root in roots {
            if let Some(project) = &root.project_root {
                self.retain_project(project, &root.path);
            }
            if self.watch.lock().watchers.contains_key(&root.path) {
                continue;
            }
            if let Err(error) = self.open_watcher(&root.path) {
                tracing::warn!(path = %root.path.display(), %error, "skill-filesystem watcher start failed");
                complete = false;
            }
        }
        self.evict_projects();
        complete
    }

    fn open_watcher(self: &Arc<Self>, root: &Path) -> anyhow::Result<()> {
        let anchor = watch_anchor(root, self.config.watch_follow_symlinks)?;
        let watch_root = canonical_watch_path(root)?;
        let weak = Arc::downgrade(self);
        let mut watcher: Box<dyn notify::Watcher + Send> = if self.config.watch_use_polling {
            Box::new(notify::PollWatcher::new(
                watch_handler(
                    weak,
                    watch_root.clone(),
                    self.config.watch_stability_threshold_ms,
                ),
                notify::Config::default()
                    .with_poll_interval(Duration::from_millis(self.config.watch_poll_interval_ms)),
            )?)
        } else {
            Box::new(notify::RecommendedWatcher::new(
                watch_handler(weak, watch_root, self.config.watch_stability_threshold_ms),
                notify::Config::default(),
            )?)
        };
        watcher.watch(&anchor, RecursiveMode::Recursive)?;
        self.watch
            .lock()
            .watchers
            .insert(root.to_path_buf(), watcher);
        Ok(())
    }

    fn retain_project(&self, project: &Path, root: &Path) {
        let mut state = self.watch.lock();
        let project = project.to_path_buf();
        if !state.project_paths.contains_key(&project) {
            state.project_order.push(project.clone());
        }
        state
            .project_paths
            .entry(project)
            .or_default()
            .insert(root.to_path_buf());
    }

    fn evict_projects(&self) {
        let mut state = self.watch.lock();
        while state.project_order.len() > self.config.watch_max_projects {
            let project = state.project_order.remove(0);
            if let Some(paths) = state.project_paths.remove(&project) {
                for path in paths {
                    state.watchers.remove(&path);
                }
            }
            drop(state);
            self.invalidate();
            state = self.watch.lock();
        }
    }

    fn invalidate(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.invalidate();
        }
    }
}

#[async_trait::async_trait]
impl SkillProvider for FileSystemSkillProvider {
    fn name(&self) -> &str {
        &self.config.provider_name
    }

    async fn list(&self, options: &SkillLookupOptions) -> anyhow::Result<SkillProviderObservation> {
        let roots = self.roots(options.cwd.as_deref()).await?;
        let complete = self
            .self_weak
            .upgrade()
            .is_some_and(|provider| provider.observe_roots(&roots));
        let mut candidates = Vec::new();
        for root in &roots {
            candidates.extend(discover_root(&self.context, root, self.name(), options).await?);
        }
        // Watch creation needs an Arc owner. Directly constructed providers
        // without one remain readable and simply produce incomplete snapshots.
        Ok(SkillProviderObservation {
            candidates,
            complete,
        })
    }

    async fn get(
        &self,
        candidate: &SkillCandidate,
        options: &SkillLookupOptions,
    ) -> anyhow::Result<Option<SkillDefinition>> {
        let locator: LocalLocator = serde_json::from_value(candidate.locator.clone())?;
        let Some(parsed) = parse_skill_file(
            &self.context,
            &locator.path,
            options.signal.as_ref(),
            candidate.source.0 == "bundled",
        )
        .await?
        else {
            return Ok(None);
        };
        Ok(Some(SkillDefinition {
            summary: SkillSummary {
                name: parsed.name,
                description: parsed.description,
                when_to_use: parsed.when_to_use,
                invocation: parsed.invocation,
                source: candidate.source.clone(),
                provider: self.name().to_owned(),
                resource_base: Some(SkillResourceBase::Directory {
                    path: display_path(&locator.directory),
                }),
            },
            content: parsed.content,
            path: Some(display_path(&locator.path)),
            metadata: parsed.metadata,
        }))
    }
}

/// Installs and registers the provider in the calling scope.
///
/// # Errors
///
/// Returns missing Skill service, invalid config, or registration failures.
pub fn install(
    context: &Context,
    config: Config,
) -> anyhow::Result<(
    Arc<FileSystemSkillProvider>,
    seekdeep_cordis::fiber::EffectHandle,
)> {
    let registry = context
        .get(SKILLS)
        .ok_or_else(|| anyhow::anyhow!("skill-filesystem requires skills"))?;
    let provider = FileSystemSkillProvider::new(context, &registry, config)?;
    let effect = registry.register_provider(context, provider.clone())?;
    let weak = Arc::downgrade(&provider);
    context.own(EffectHandle::synchronous(
        "skill-filesystem watcher",
        move || {
            if let Some(provider) = weak.upgrade() {
                provider.dispose();
            }
            Ok(())
        },
    ))?;
    let observed = Arc::downgrade(&provider);
    context.events().on_sync(
        context,
        "fs/observed",
        move |_, args| {
            let target = args
                .get::<seekdeep_fs::FsTarget>(0)
                .ok_or_else(|| anyhow::anyhow!("fs/observed lacks its target"))?;
            let execution = args
                .get::<seekdeep_tools::ToolExecution>(2)
                .ok_or_else(|| anyhow::anyhow!("fs/observed lacks its tool execution"))?;
            if matches!(execution.name.as_str(), "edit" | "write")
                && let Some(provider) = observed.upgrade()
            {
                provider.observe_host_mutation(Path::new(&target.display_path));
            }
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;
    Ok((provider, effect))
}

/// Builds the Loader-compatible provider plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value::<Config>(config)?;
            install(&context, config)?;
            Ok(())
        })
    })
}

#[allow(clippy::case_sensitive_file_extension_comparisons)] // Source accepts only the literal lowercase `.md` suffix.
async fn discover_root(
    context: &Context,
    root: &SkillRoot,
    provider: &str,
    options: &SkillLookupOptions,
) -> anyhow::Result<Vec<SkillCandidate>> {
    let entries = list_entries(context, root, options.signal.as_ref()).await?;
    let mut candidates = Vec::new();
    for (name, kind, path) in entries {
        if root.skip_system && name == ".system" {
            continue;
        }
        let locator = match kind {
            FsKind::Directory => LocalLocator {
                path: path.join("SKILL.md"),
                directory: path,
            },
            FsKind::File if name.ends_with(".md") => LocalLocator {
                path,
                directory: root.path.clone(),
            },
            FsKind::File | FsKind::Other => continue,
        };
        let Some(parsed) = parse_skill_file(
            context,
            &locator.path,
            options.signal.as_ref(),
            root.trusted_host,
        )
        .await?
        else {
            continue;
        };
        candidates.push(SkillCandidate {
            name: parsed.name,
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            invocation: parsed.invocation,
            source: root.source.clone(),
            provider: provider.to_owned(),
            resource_base: Some(SkillResourceBase::Directory {
                path: display_path(&locator.directory),
            }),
            rank: root.rank,
            locator: serde_json::to_value(&locator)?,
            path: Some(display_path(&locator.path)),
            metadata: parsed.metadata,
        });
    }
    candidates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(candidates)
}

async fn list_entries(
    context: &Context,
    root: &SkillRoot,
    signal: Option<&seekdeep_llm::AbortSignal>,
) -> anyhow::Result<Vec<(String, FsKind, PathBuf)>> {
    if let Some(fs) = context.get(FS).filter(|_| !root.trusted_host) {
        let filesystem = fs.filesystem();
        let target = match filesystem
            .resolve(&display_path(&root.path), None, signal)
            .await
        {
            Ok(target) => target,
            Err(error) if absent_error(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let entries = match filesystem.list_dir(&target, signal).await {
            Ok(entries) => entries,
            Err(error) if absent_error(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        return Ok(entries
            .into_iter()
            .map(|entry| {
                (
                    entry.name,
                    entry.kind,
                    PathBuf::from(entry.target.display_path),
                )
            })
            .collect());
    }
    let mut directory = match tokio::fs::read_dir(&root.path).await {
        Ok(directory) => directory,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut entries = Vec::new();
    while let Some(entry) = directory.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = if self_follow_symlink(&entry.path()).await {
            tokio::fs::metadata(entry.path()).await.ok()
        } else {
            entry.metadata().await.ok()
        };
        let kind = metadata.as_ref().map_or(FsKind::Other, |metadata| {
            if metadata.is_dir() {
                FsKind::Directory
            } else if metadata.is_file() {
                FsKind::File
            } else {
                FsKind::Other
            }
        });
        entries.push((name, kind, entry.path()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

async fn self_follow_symlink(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
}

async fn parse_skill_file(
    context: &Context,
    path: &Path,
    signal: Option<&seekdeep_llm::AbortSignal>,
    trusted_host: bool,
) -> anyhow::Result<Option<ParsedSkill>> {
    if signal.is_some_and(seekdeep_llm::AbortSignal::is_aborted) {
        anyhow::bail!("skill read was aborted");
    }
    let raw = match read_text(context, path, signal, trusted_host).await {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some((frontmatter, body)) = split_frontmatter(&raw) else {
        tracing::warn!(path = %path.display(), "skill file ignored: missing YAML frontmatter");
        return Ok(None);
    };
    let data: Value = match serde_yml::from_str::<serde_json::Value>(frontmatter) {
        Ok(Value::Object(data)) => Value::Object(data),
        Ok(_) => return Ok(None),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "skill file ignored: invalid YAML frontmatter");
            return Ok(None);
        }
    };
    let Value::Object(data) = data else {
        unreachable!();
    };
    let Some(name) = nonempty_string(&data, "name") else {
        return Ok(None);
    };
    let Some(description) = nonempty_string(&data, "description") else {
        return Ok(None);
    };
    if !is_skill_name(&name) {
        return Ok(None);
    }
    let invocation = match invocation_policy(&data) {
        Ok(invocation) => invocation,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "skill file ignored: invalid invocation frontmatter");
            return Ok(None);
        }
    };
    Ok(Some(ParsedSkill {
        name,
        description,
        when_to_use: nonempty_string(&data, "whenToUse"),
        invocation,
        metadata: data
            .get("metadata")
            .filter(|value| value.is_object())
            .cloned(),
        content: body.trim().to_owned(),
    }))
}

async fn read_text(
    context: &Context,
    path: &Path,
    signal: Option<&seekdeep_llm::AbortSignal>,
    trusted_host: bool,
) -> anyhow::Result<Option<String>> {
    if let Some(fs) = context.get(FS).filter(|_| !trusted_host) {
        let filesystem = fs.filesystem();
        let target = match filesystem.resolve(&display_path(path), None, signal).await {
            Ok(target) => target,
            Err(error) if absent_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(info) = filesystem.stat(&target, signal).await? else {
            return Ok(None);
        };
        if info.kind != FsKind::File {
            return Ok(None);
        }
        return filesystem.read_text(&target, signal).await.map(Some);
    }
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let first = raw.find('\n')?;
    if raw[..first].trim_end_matches('\r') != "---" {
        return None;
    }
    let mut cursor = first + 1;
    loop {
        let next = raw[cursor..].find('\n').map(|offset| cursor + offset);
        let end = next.unwrap_or(raw.len());
        if raw[cursor..end].trim_end_matches('\r') == "---" {
            return Some((
                &raw[first + 1..cursor],
                &raw[next.map_or(raw.len(), |value| value + 1)..],
            ));
        }
        cursor = next? + 1;
    }
}

fn invocation_policy(data: &Map<String, Value>) -> anyhow::Result<SkillInvocationPolicy> {
    for (legacy, canonical) in [
        ("disableModelInvocation", "disable-model-invocation"),
        ("modelInvocable", "disable-model-invocation"),
        ("userInvocable", "user-invocable"),
    ] {
        anyhow::ensure!(
            !data.contains_key(legacy),
            "frontmatter field \"{legacy}\" is unsupported; use \"{canonical}\""
        );
    }
    Ok(SkillInvocationPolicy {
        model_invocable: !boolean_field(data.get("disable-model-invocation"))?.unwrap_or(false),
        user_invocable: boolean_field(data.get("user-invocable"))?.unwrap_or(true),
    })
}

fn boolean_field(value: Option<&Value>) -> anyhow::Result<Option<bool>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) if value.as_i64() == Some(1) => Some(true),
        Value::Number(value) if value.as_i64() == Some(0) => Some(false),
        Value::String(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    };
    parsed
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("frontmatter field must be a boolean"))
}

fn nonempty_string(data: &Map<String, Value>, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn find_project_root(context: &Context, cwd: &Path) -> PathBuf {
    let mut current = cwd.to_path_buf();
    loop {
        if path_exists(context, &current.join(".git")).await {
            return current;
        }
        let Some(parent) = current.parent() else {
            return cwd.to_path_buf();
        };
        current = parent.to_path_buf();
    }
}

async fn path_exists(context: &Context, path: &Path) -> bool {
    if let Some(fs) = context.get(FS) {
        let filesystem = fs.filesystem();
        let Ok(target) = filesystem.resolve(&display_path(path), None, None).await else {
            return false;
        };
        return filesystem
            .stat(&target, None)
            .await
            .is_ok_and(|info| info.is_some());
    }
    tokio::fs::metadata(path).await.is_ok()
}

fn absent_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<seekdeep_fs::FsError>()
        .is_some_and(|error| {
            matches!(
                error.code,
                seekdeep_fs::FsErrorCode::FsNotFound | seekdeep_fs::FsErrorCode::FsNotDirectory
            )
        })
}

fn existing_ancestor(path: &Path) -> anyhow::Result<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.is_dir() {
            let current = std::fs::canonicalize(&current).unwrap_or(current);
            if path.is_dir()
                && let Some(parent) = current.parent()
            {
                return Ok(std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf()));
            }
            return Ok(current);
        }
        anyhow::ensure!(current.pop(), "watch path has no existing ancestor");
    }
}

fn watch_anchor(path: &Path, follow_symlinks: bool) -> anyhow::Result<PathBuf> {
    if !follow_symlinks
        && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        let absolute = absolute(path)?;
        return Ok(absolute
            .parent()
            .map_or_else(|| absolute.clone(), Path::to_path_buf));
    }
    existing_ancestor(path)
}

fn watch_handler(
    weak: Weak<FileSystemSkillProvider>,
    root: PathBuf,
    stability_threshold_ms: u64,
) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
    move |event| {
        let Some(provider) = weak.upgrade() else {
            return;
        };
        if provider.watch.lock().closing {
            return;
        }
        match event {
            Ok(event)
                if !event.paths.is_empty()
                    && !event.paths.iter().any(|path| {
                        path == &root || potential_skill_path(&root, path) || root.starts_with(path)
                    }) =>
            {
                return;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "skill-filesystem watcher failed"),
        }
        let epoch = provider.invalidation_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let weak = Arc::downgrade(&provider);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(stability_threshold_ms));
            let Some(provider) = weak.upgrade() else {
                return;
            };
            if !provider.watch.lock().closing
                && provider.invalidation_epoch.load(Ordering::Acquire) == epoch
            {
                provider.invalidate();
            }
        });
    }
}

fn canonical_watch_path(path: &Path) -> anyhow::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("watch path has no existing ancestor"))?
            .to_os_string();
        suffix.push(name);
        anyhow::ensure!(existing.pop(), "watch path has no existing ancestor");
    }
    let mut canonical = std::fs::canonicalize(existing)?;
    for segment in suffix.into_iter().rev() {
        canonical.push(segment);
    }
    Ok(canonical)
}

fn potential_skill_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let segments = relative.components().collect::<Vec<_>>();
    match segments.as_slice() {
        [name] => name.as_os_str().to_string_lossy().ends_with(".md"),
        [_, file] => file.as_os_str() == "SKILL.md",
        _ => false,
    }
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
