//! Lifecycle-owned Host file watching over Loader HMR transactions.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::future::BoxFuture;
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_loader::{HostHmrOutcome, LOADER, LoaderSettlement};
use serde::{Deserialize, Serialize};

/// Cordis service name.
pub const NAME: &str = "hmr";
/// Services required by the Host watcher.
pub const INJECT: &[&str] = &["loader", "timer"];
/// Typed Host HMR service slot.
pub const HMR: ServiceKey<HostHmrService> = ServiceKey::new(NAME);

/// Full-process restart callback selected by the launcher.
pub type RestartHook = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Host watcher configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Base directory for relative roots.
    pub base: Option<PathBuf>,
    /// Recursive roots observed for changes.
    pub root: Vec<PathBuf>,
    /// Quiet period used to coalesce a burst.
    pub debounce: u64,
    /// Path fragments excluded from observation.
    pub ignored: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base: None,
            root: vec![PathBuf::from(".")],
            debounce: 100,
            ignored: vec![
                "**/node_modules".to_owned(),
                "**/.*".to_owned(),
                "cache".to_owned(),
                "data".to_owned(),
            ],
        }
    }
}

struct WorkerState {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// Active Host watcher. All file events and replacements are serialized by
/// its worker and the Loader transaction.
pub struct HostHmrService {
    state: Mutex<WorkerState>,
}

impl std::fmt::Debug for HostHmrService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostHmrService")
            .finish_non_exhaustive()
    }
}

impl HostHmrService {
    /// Opens every configured recursive root and starts the serialized worker.
    ///
    /// # Errors
    ///
    /// Returns base-path, watcher construction, or root registration failures.
    pub fn start(
        context: Context,
        loader: Arc<LoaderSettlement>,
        config: Config,
        restart: RestartHook,
    ) -> anyhow::Result<Arc<Self>> {
        let base = resolve_base(&context, config.base.as_deref())?;
        let (events_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = events_sender.send(event);
        })?;
        for root in &config.root {
            let root = if root.is_absolute() {
                root.clone()
            } else {
                base.join(root)
            };
            watcher.watch(&root, RecursiveMode::Recursive)?;
        }
        let (cancel, mut cancellation) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut pending = BTreeSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut cancellation => {
                        drop(watcher);
                        while let Ok(event) = events.try_recv() {
                            collect_event(event, &config, &mut pending);
                        }
                        process_paths(&context, &loader, &restart, &mut pending).await;
                        break;
                    }
                    event = events.recv() => {
                        let Some(event) = event else { break };
                        collect_event(event, &config, &mut pending);
                        if pending.is_empty() {
                            continue;
                        }
                        if config.debounce != 0 {
                            tokio::time::sleep(Duration::from_millis(config.debounce)).await;
                            while let Ok(event) = events.try_recv() {
                                collect_event(event, &config, &mut pending);
                            }
                        }
                        process_paths(&context, &loader, &restart, &mut pending).await;
                    }
                }
            }
        });
        Ok(Arc::new(Self {
            state: Mutex::new(WorkerState {
                cancel: Some(cancel),
                task: Some(task),
            }),
        }))
    }

    /// Stops observation and waits for every admitted file event.
    ///
    /// # Errors
    ///
    /// Returns worker cancellation or panic failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        let (cancel, task) = {
            let mut state = self.state.lock();
            (state.cancel.take(), state.task.take())
        };
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
        if let Some(task) = task {
            task.await?;
        }
        Ok(())
    }
}

impl Drop for HostHmrService {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        if let Some(cancel) = state.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(task) = state.task.take() {
            task.abort();
        }
    }
}

fn resolve_base(context: &Context, configured: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(configured) = configured {
        return Ok(if configured.is_absolute() {
            configured.to_path_buf()
        } else {
            std::env::current_dir()?.join(configured)
        });
    }
    if let Some(base) = context
        .meta("loader.base_url")
        .and_then(|value| value.as_str().and_then(|value| url::Url::parse(value).ok()))
        .and_then(|url| url.to_file_path().ok())
    {
        return Ok(base);
    }
    Ok(std::env::current_dir()?)
}

fn collect_event(event: notify::Result<Event>, config: &Config, pending: &mut BTreeSet<PathBuf>) {
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            tracing::warn!(%error, "Host HMR watcher failed");
            return;
        }
    };
    if !matches!(event.kind, EventKind::Modify(_)) {
        return;
    }
    pending.extend(
        event
            .paths
            .into_iter()
            .filter(|path| !is_ignored(path, config)),
    );
}

fn is_ignored(path: &Path, config: &Config) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
    {
        return true;
    }
    let rendered = path.to_string_lossy();
    config.ignored.iter().any(|ignored| {
        if ignored == "**/.*" {
            return false;
        }
        let fragment = ignored.trim_matches('*').trim_matches('/');
        !fragment.is_empty() && rendered.contains(fragment)
    })
}

async fn process_paths(
    context: &Context,
    loader: &LoaderSettlement,
    restart: &RestartHook,
    pending: &mut BTreeSet<PathBuf>,
) {
    for path in std::mem::take(pending) {
        match loader.refresh_include_path(&path).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "config reload failed");
                let _ = context.events().emit(
                    context,
                    "hmr/config-update-failed",
                    &EventArgs::from_values(vec![Arc::new(path), Arc::new(error.to_string())]),
                );
                continue;
            }
        }
        match loader.reload_module(&path).await {
            Ok(HostHmrOutcome::FullRestart) => {
                if let Err(error) = restart().await {
                    tracing::warn!(%error, "Host full restart hook failed");
                }
            }
            Ok(HostHmrOutcome::Reloaded(_) | HostHmrOutcome::Untracked) => {}
            Err(error) => tracing::warn!(path = %path.display(), %error, "Host HMR failed"),
        }
    }
}

/// Builds the Loader-compatible Host HMR plugin.
#[must_use]
pub fn plugin(restart: RestartHook) -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        let restart = restart.clone();
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            let loader = context
                .get(LOADER)
                .ok_or_else(|| anyhow::anyhow!("Host HMR requires loader"))?;
            let service = HostHmrService::start(context.clone(), loader, config, restart)?;
            let cleanup = service.clone();
            context.own(EffectHandle::new("Host HMR watcher", move || {
                let cleanup = cleanup.clone();
                Box::pin(async move { cleanup.dispose().await })
            }))?;
            context.provide(HMR, service)?;
            Ok(())
        })
    })
}
