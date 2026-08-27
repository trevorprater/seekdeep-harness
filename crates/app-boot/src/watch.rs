//! Exact-path config watching with serialized refresh and disposal drain.

use std::{
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::future::BoxFuture;
use notify::{RecursiveMode, Watcher as _};
use parking_lot::Mutex;
use path_clean::PathClean as _;

/// Asynchronous refresh invoked after one exact target-state change.
pub type ConfigRefresh = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// Contained refresh/watch failure observer.
pub type ConfigRefreshFailure = Arc<dyn Fn(&Path, anyhow::Error) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileStamp {
    Absent,
    Content(Vec<u8>),
    Unreadable(String),
}

fn file_stamp(path: &Path) -> FileStamp {
    match std::fs::read(path) {
        Ok(content) => FileStamp::Content(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileStamp::Absent,
        Err(error) => FileStamp::Unreadable(error.to_string()),
    }
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean())
}

/// Collapses existing filesystem aliases while retaining a missing suffix.
///
/// # Errors
///
/// Returns current-directory or canonicalization failures.
pub fn canonical_watch_key(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = absolute(path)?;
    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("config path has no existing ancestor"))?;
        suffix.push(name.to_owned());
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no existing ancestor"))?;
    }
    let mut canonical = std::fs::canonicalize(existing)?;
    for component in suffix.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical.clean())
}

fn watch_root(target: &Path) -> anyhow::Result<PathBuf> {
    let mut current = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent"))?;
    while !current.exists() {
        current = current
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no existing parent"))?;
    }
    Ok(std::fs::canonicalize(current)?)
}

fn report_failure(observer: &ConfigRefreshFailure, path: &Path, error: anyhow::Error) {
    let _ = catch_unwind(AssertUnwindSafe(|| observer(path, error)));
}

async fn refresh_if_changed(
    target: &Path,
    stamp: &mut FileStamp,
    refresh: &ConfigRefresh,
    failure: &ConfigRefreshFailure,
) {
    let next = file_stamp(target);
    if next == *stamp {
        return;
    }
    *stamp = next;
    if let Err(error) = refresh().await {
        report_failure(failure, target, error);
    }
}

struct WatchState {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// Exact-path watcher whose refreshes never overlap.
pub struct ExactConfigWatcher {
    target: PathBuf,
    key: PathBuf,
    state: Mutex<WatchState>,
}

impl std::fmt::Debug for ExactConfigWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExactConfigWatcher")
            .field("target", &self.target)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl ExactConfigWatcher {
    /// Registers a recursive ancestor watch filtered by exact target state.
    ///
    /// An existing target triggers one initial refresh, matching the source
    /// watcher's initial `add` event.
    ///
    /// # Errors
    ///
    /// Returns path resolution, backend construction, or registration failures.
    pub fn open(
        target: impl AsRef<Path>,
        refresh: ConfigRefresh,
        failure: ConfigRefreshFailure,
    ) -> anyhow::Result<Self> {
        let target = absolute(target.as_ref())?;
        let key = canonical_watch_key(&target)?;
        let root = watch_root(&key)?;
        let (events_sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = events_sender.send(event);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        let (cancel, mut cancellation) = tokio::sync::oneshot::channel();
        let task_target = target.clone();
        let task = tokio::spawn(async move {
            let mut stamp = FileStamp::Absent;
            if task_target.exists() {
                refresh_if_changed(&task_target, &mut stamp, &refresh, &failure).await;
            }
            loop {
                tokio::select! {
                    biased;
                    event = events.recv() => {
                        match event {
                            Some(Ok(_)) => {
                                refresh_if_changed(&task_target, &mut stamp, &refresh, &failure).await;
                            }
                            Some(Err(error)) => {
                                report_failure(&failure, &task_target, error.into());
                            }
                            None => break,
                        }
                    }
                    _ = &mut cancellation => {
                        drop(watcher);
                        while let Ok(event) = events.try_recv() {
                            match event {
                                Ok(_) => {
                                    refresh_if_changed(&task_target, &mut stamp, &refresh, &failure).await;
                                }
                                Err(error) => report_failure(&failure, &task_target, error.into()),
                            }
                        }
                        break;
                    }
                }
            }
        });
        Ok(Self {
            target,
            key,
            state: Mutex::new(WatchState {
                cancel: Some(cancel),
                task: Some(task),
            }),
        })
    }

    /// Canonical identity used for duplicate registration checks.
    #[must_use]
    pub fn key(&self) -> &Path {
        &self.key
    }

    /// Stops new events and waits for active and queued refreshes.
    ///
    /// # Errors
    ///
    /// Returns a worker panic or cancellation failure.
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

impl Drop for ExactConfigWatcher {
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

/// Registry enforcing one watcher per canonical config identity.
#[derive(Debug, Default)]
pub struct ConfigWatchRegistry {
    keys: Arc<Mutex<HashSet<PathBuf>>>,
    refresh_transaction: Arc<tokio::sync::Mutex<()>>,
}

impl ConfigWatchRegistry {
    /// Creates an empty exact-path registry.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Opens and reserves one canonical exact-path watcher.
    ///
    /// # Errors
    ///
    /// Returns duplicate-path or watcher construction failures.
    pub fn register(
        self: &Arc<Self>,
        target: impl AsRef<Path>,
        refresh: ConfigRefresh,
        failure: ConfigRefreshFailure,
    ) -> anyhow::Result<RegisteredConfigWatcher> {
        let key = canonical_watch_key(target.as_ref())?;
        anyhow::ensure!(
            self.keys.lock().insert(key.clone()),
            "config path already registered: {}",
            key.display()
        );
        let transaction = self.refresh_transaction.clone();
        let serialized: ConfigRefresh = Arc::new(move || {
            let transaction = transaction.clone();
            let refresh = refresh.clone();
            Box::pin(async move {
                let _guard = transaction.lock().await;
                refresh().await
            })
        });
        match ExactConfigWatcher::open(target, serialized, failure) {
            Ok(watcher) => Ok(RegisteredConfigWatcher {
                watcher: Some(watcher),
                registry: self.clone(),
                key,
            }),
            Err(error) => {
                self.keys.lock().remove(&key);
                Err(error)
            }
        }
    }
}

/// One canonical path reservation plus its live watcher.
pub struct RegisteredConfigWatcher {
    watcher: Option<ExactConfigWatcher>,
    registry: Arc<ConfigWatchRegistry>,
    key: PathBuf,
}

impl std::fmt::Debug for RegisteredConfigWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredConfigWatcher")
            .field("key", &self.key)
            .field("active", &self.watcher.is_some())
            .finish_non_exhaustive()
    }
}

impl RegisteredConfigWatcher {
    /// Canonical registered identity.
    #[must_use]
    pub fn key(&self) -> &Path {
        &self.key
    }

    /// Drains refreshes, stops the watcher, and releases the path reservation.
    ///
    /// # Errors
    ///
    /// Returns watcher worker failures.
    pub async fn dispose(mut self) -> anyhow::Result<()> {
        let result = match self.watcher.take() {
            Some(watcher) => watcher.dispose().await,
            None => Ok(()),
        };
        self.registry.keys.lock().remove(&self.key);
        result
    }
}

impl Drop for RegisteredConfigWatcher {
    fn drop(&mut self) {
        self.registry.keys.lock().remove(&self.key);
    }
}
