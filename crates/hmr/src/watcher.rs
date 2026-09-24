//! File observation and exact config-registration lifetimes.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, fiber::EffectHandle};
use seekdeep_loader::{
    HostFileWatcher, HostHmrChange, HostHmrOutcome, HostWatchEvent, LoaderError, LoaderSettlement,
};
use serde_json::{Map, Value, json};

use crate::{Config, RestartHook, ignore::IgnoreMatcher};

/// The refresh callback retained by an exact config-file registration.
pub type ConfigRefresh = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

struct Background {
    cancel: tokio::sync::watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    joining: tokio::sync::Mutex<()>,
}

impl Background {
    fn spawn(
        task: impl FnOnce(tokio::sync::watch::Receiver<bool>) -> BoxFuture<'static, ()>,
    ) -> Self {
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        Self {
            cancel,
            task: Mutex::new(Some(tokio::spawn(task(receiver)))),
            joining: tokio::sync::Mutex::new(()),
        }
    }

    fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    async fn join(&self) -> anyhow::Result<()> {
        self.cancel();
        let _joining = self.joining.lock().await;
        let task = self.task.lock().take();
        if let Some(task) = task {
            task.await?;
        }
        Ok(())
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

struct SerialRefresh {
    sender: tokio::sync::mpsc::UnboundedSender<()>,
    background: Background,
}

impl SerialRefresh {
    fn new(context: Context, filename: PathBuf, callback: ConfigRefresh) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let background = Background::spawn(move |mut cancelled| {
            Box::pin(async move {
                loop {
                    let has_refresh = tokio::select! {
                        biased;
                        message = receiver.recv() => message.is_some(),
                        _ = cancelled.changed() => receiver.try_recv().is_ok(),
                    };
                    if !has_refresh {
                        break;
                    }
                    loop {
                        let failure = match std::panic::AssertUnwindSafe(async { callback().await })
                            .catch_unwind()
                            .await
                        {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(error.to_string()),
                            Err(_) => Some("config refresh callback panicked".to_owned()),
                        };
                        if let Some(error) = failure {
                            report_config_failure(&context, &filename, error).await;
                        }
                        let mut dirty = false;
                        while receiver.try_recv().is_ok() {
                            dirty = true;
                        }
                        if !dirty {
                            break;
                        }
                    }
                    if *cancelled.borrow() {
                        break;
                    }
                }
            })
        });
        Self { sender, background }
    }

    fn trigger(&self) {
        if !*self.background.cancel.borrow() {
            let _ = self.sender.send(());
        }
    }
}

type Registrations = Arc<Mutex<BTreeMap<PathBuf, Arc<ConfigRegistration>>>>;
type IncludeRefreshes = Arc<Mutex<BTreeMap<PathBuf, Arc<SerialRefresh>>>>;

struct ConfigRegistration {
    watcher: Arc<HostFileWatcher>,
    events: Background,
    refresh: Arc<SerialRefresh>,
}

impl ConfigRegistration {
    fn stop(&self) {
        self.events.cancel();
        self.refresh.background.cancel();
    }

    async fn dispose(&self) -> anyhow::Result<()> {
        self.stop();
        let closed = self.watcher.close().await;
        let events = self.events.join().await;
        let refresh = self.refresh.background.join().await;
        closed?;
        events?;
        refresh
    }
}

impl Drop for ConfigRegistration {
    fn drop(&mut self) {
        self.stop();
    }
}

// Cancellation during readiness must release the canonical path and watcher.
struct RegistrationAdmission {
    registrations: Weak<Mutex<BTreeMap<PathBuf, Arc<ConfigRegistration>>>>,
    filename: PathBuf,
    record: Arc<ConfigRegistration>,
    pending: bool,
}

impl Drop for RegistrationAdmission {
    fn drop(&mut self) {
        if self.pending {
            remove_registration(&self.registrations, &self.filename, &self.record);
            self.record.stop();
        }
    }
}

/// Active Host watcher and all lifecycle-owned exact config registrations.
pub struct HostHmrService {
    context: Context,
    loader: Arc<LoaderSettlement>,
    config: Config,
    base: PathBuf,
    watcher: Arc<HostFileWatcher>,
    active: AtomicBool,
    background: Background,
    registrations: Registrations,
    includes: IncludeRefreshes,
    disposing: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for HostHmrService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostHmrService")
            .field("base", &self.base)
            .field("active", &self.active.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl HostHmrService {
    /// Opens module roots after the Loader has established its dependency graph.
    ///
    /// # Errors
    ///
    /// Returns base-path, ignore-pattern, or watcher startup failures.
    pub fn start(
        context: Context,
        loader: Arc<LoaderSettlement>,
        config: Config,
        restart: RestartHook,
    ) -> anyhow::Result<Arc<Self>> {
        let configured_base = resolve_base(&context, config.base.as_deref())?;
        if config.base.is_some() {
            context.logger(None).info([
                json!("watching %o in %s"),
                json!(config.root),
                json!(configured_base),
            ]);
        } else {
            context
                .logger(None)
                .info([json!("watching %o"), json!(config.root)]);
        }
        IgnoreMatcher::new(&config.ignored)?;
        let base = configured_base.canonicalize()?;
        let mut options: Map<String, Value> =
            serde_json::from_value(serde_json::to_value(&config)?)?;
        options.insert("cwd".into(), json!(base));
        options.insert("ignoreInitial".into(), Value::Bool(true));
        let watcher = loader.watch_files(
            &config.root,
            &Value::Object(options),
            Some(&(base.clone(), config.ignored.clone())),
        )?;
        watcher.ready_blocking()?;
        let registrations = Arc::new(Mutex::new(BTreeMap::new()));
        let includes = Arc::new(Mutex::new(BTreeMap::new()));
        let runtime = MainWatcher {
            context: context.clone(),
            loader: loader.clone(),
            base,
            configured_base: configured_base.clone(),
            debounce: config.debounce,
            restart,
            includes: includes.clone(),
            watcher: watcher.clone(),
        };
        let background = Background::spawn(move |cancelled| Box::pin(runtime.run(cancelled)));
        Ok(Arc::new(Self {
            context,
            loader,
            config,
            base: configured_base,
            watcher,
            active: AtomicBool::new(true),
            background,
            registrations,
            includes,
            disposing: tokio::sync::Mutex::new(()),
        }))
    }

    /// Directory against which HMR resolves configured paths.
    #[must_use]
    pub fn base_dir(&self) -> &Path {
        &self.base
    }

    /// Watches one exact config path independently of module roots and ignores.
    /// Waits for the initial scan; admitted refreshes may still be running.
    ///
    /// # Errors
    ///
    /// Rejects inactive HMR, duplicate canonical paths, invalid watch parents,
    /// watcher startup failures, and registration against a disposed context.
    pub async fn register_config(
        self: &Arc<Self>,
        filename: impl AsRef<Path>,
        refresh: ConfigRefresh,
    ) -> anyhow::Result<EffectHandle> {
        anyhow::ensure!(self.active.load(Ordering::Acquire), "HMR is not active");
        let filename = resolve_path(&self.base, filename.as_ref());
        let target = find_watch_root(&filename)?;
        let (record, ready) = {
            let mut registrations = self.registrations.lock();
            anyhow::ensure!(self.active.load(Ordering::Acquire), "HMR is not active");
            anyhow::ensure!(
                !registrations.contains_key(&target.filename),
                "config path already registered: {}",
                filename.display()
            );
            let mut options: Map<String, Value> =
                serde_json::from_value(serde_json::to_value(&self.config)?)?;
            options.remove("cwd");
            options.remove("ignored");
            options.insert("depth".into(), json!(target.depth));
            options.insert("ignoreInitial".into(), Value::Bool(false));
            let watcher = self.loader.watch_files(
                std::slice::from_ref(&target.root),
                &Value::Object(options),
                None,
            )?;
            let serial = Arc::new(SerialRefresh::new(
                self.context.clone(),
                filename.clone(),
                refresh,
            ));
            let (ready_sender, ready) = tokio::sync::oneshot::channel();
            let configured = filename.clone();
            let canonical = target.filename.clone();
            let context = self.context.clone();
            let watching = watcher.clone();
            let refreshing = serial.clone();
            let events = Background::spawn(move |cancelled| {
                Box::pin(run_config_watch(
                    context,
                    watching,
                    configured,
                    canonical,
                    refreshing,
                    ready_sender,
                    cancelled,
                ))
            });
            let record = Arc::new(ConfigRegistration {
                watcher,
                events,
                refresh: serial,
            });
            registrations.insert(target.filename.clone(), record.clone());
            (record, ready)
        };
        let mut admission = RegistrationAdmission {
            registrations: Arc::downgrade(&self.registrations),
            filename: target.filename.clone(),
            record: record.clone(),
            pending: true,
        };
        let readiness = ready.await.unwrap_or(Err(LoaderError::Unavailable));
        if let Err(error) = readiness {
            remove_registration(&admission.registrations, &target.filename, &record);
            record.dispose().await?;
            return Err(error.into());
        }
        if !self.active.load(Ordering::Acquire) {
            remove_registration(&admission.registrations, &target.filename, &record);
            record.dispose().await?;
            anyhow::bail!("HMR is not active");
        }
        let registrations = Arc::downgrade(&self.registrations);
        let cleanup = record.clone();
        let key = target.filename;
        let effect = EffectHandle::new("hmr.registerConfig()", move || {
            let cleanup = cleanup.clone();
            let registrations = registrations.clone();
            let key = key.clone();
            Box::pin(async move {
                remove_registration(&registrations, &key, &cleanup);
                cleanup.dispose().await
            })
        });
        if let Err(error) = self.context.own(effect.clone()) {
            effect.dispose().await?;
            return Err(error.into());
        }
        admission.pending = false;
        Ok(effect)
    }

    /// Closes every watcher and joins all admitted refresh and reload work.
    ///
    /// # Errors
    ///
    /// Returns an unexpected worker join failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.active.store(false, Ordering::Release);
        self.background.cancel();
        let registrations: Vec<_> = self.registrations.lock().values().cloned().collect();
        for registration in &registrations {
            registration.stop();
        }
        let _disposing = self.disposing.lock().await;
        let watcher_result = self.watcher.close().await;
        let main_result = self.background.join().await;
        let includes: Vec<_> = self.includes.lock().values().cloned().collect();
        for refresh in &includes {
            refresh.background.cancel();
        }
        let mut failures = Vec::new();
        if let Err(error) = watcher_result {
            failures.push(error.to_string());
        }
        if let Err(error) = main_result {
            failures.push(error.to_string());
        }
        for result in futures::future::join_all(
            registrations
                .iter()
                .map(|registration| registration.dispose()),
        )
        .await
        {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        for result in
            futures::future::join_all(includes.iter().map(|refresh| refresh.background.join()))
                .await
        {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        self.registrations.lock().clear();
        self.includes.lock().clear();
        anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
        Ok(())
    }
}

impl Drop for HostHmrService {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.background.cancel();
        for registration in self.registrations.lock().values() {
            registration.stop();
        }
        for refresh in self.includes.lock().values() {
            refresh.background.cancel();
        }
    }
}

fn remove_registration(
    registrations: &Weak<Mutex<BTreeMap<PathBuf, Arc<ConfigRegistration>>>>,
    key: &Path,
    registration: &Arc<ConfigRegistration>,
) {
    if let Some(registrations) = registrations.upgrade() {
        let mut registrations = registrations.lock();
        if registrations
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, registration))
        {
            registrations.remove(key);
        }
    }
}

async fn run_config_watch(
    context: Context,
    watcher: Arc<HostFileWatcher>,
    configured: PathBuf,
    canonical: PathBuf,
    refresh: Arc<SerialRefresh>,
    ready: tokio::sync::oneshot::Sender<Result<(), LoaderError>>,
    mut cancelled: tokio::sync::watch::Receiver<bool>,
) {
    let mut ready = Some(ready);
    loop {
        tokio::select! {
            biased;
            _ = cancelled.changed() => break,
            event = watcher.next_event() => {
                let Some(event) = event else { break; };
                match event {
                    HostWatchEvent::Ready => {
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(watcher.ready().await);
                        }
                    }
                    HostWatchEvent::Error(error) => {
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(watcher.ready().await);
                        } else {
                            context.logger(None).warn([error]);
                        }
                    }
                    HostWatchEvent::Add(path)
                    | HostWatchEvent::Change(path)
                    | HostWatchEvent::Unlink(path) => {
                        if path == configured || path == canonical {
                            refresh.trigger();
                        }
                    }
                }
            }
        }
    }
    if let Some(ready) = ready {
        let _ = ready.send(Err(LoaderError::Unavailable));
    }
}

struct MainWatcher {
    context: Context,
    loader: Arc<LoaderSettlement>,
    base: PathBuf,
    configured_base: PathBuf,
    debounce: u64,
    restart: RestartHook,
    includes: IncludeRefreshes,
    watcher: Arc<HostFileWatcher>,
}

impl MainWatcher {
    async fn run(self, mut cancelled: tokio::sync::watch::Receiver<bool>) {
        let mut pending = Vec::new();
        let mut deadline = None;
        loop {
            tokio::select! {
                biased;
                _ = cancelled.changed() => break,
                event = self.watcher.next_event() => {
                    let Some(event) = event else { break; };
                    let changed = self.observe(event, &mut pending).await;
                    if changed { deadline = Some(tokio::time::Instant::now() + Duration::from_millis(self.debounce)); }
                }
                () = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => {
                    deadline = None;
                    match self.loader.reload_modules(&pending).await {
                        Ok(HostHmrOutcome::FullRestart) => self.restart().await,
                        Ok(HostHmrOutcome::Reloaded(_) | HostHmrOutcome::Untracked) => pending.clear(),
                        Err(error) => report_loader_failure(&self.context, &error),
                    }
                }
            }
        }
    }

    async fn restart(&self) {
        if let Err(error) = (self.restart)().await {
            self.context.logger(None).warn([json!(error.to_string())]);
        }
    }

    async fn observe(&self, event: HostWatchEvent, pending: &mut Vec<PathBuf>) -> bool {
        let (kind, relative) = match event {
            HostWatchEvent::Ready => return false,
            HostWatchEvent::Add(path) => (ChangeKind::Add, path),
            HostWatchEvent::Change(path) => (ChangeKind::Change, path),
            HostWatchEvent::Unlink(path) => (ChangeKind::Unlink, path),
            HostWatchEvent::Error(error) => {
                self.context.logger(None).warn([error]);
                return false;
            }
        };
        self.context.logger(None).debug([
            json!("%s detected at %C"),
            json!(kind.name()),
            json!(relative),
        ]);
        let path = resolve_path(&self.base, &relative);
        let configured = resolve_path(&self.configured_base, &relative);
        let include = if self.loader.is_include_path(&path) {
            Some(path.clone())
        } else if self.loader.is_include_path(&configured) {
            Some(configured)
        } else {
            None
        };
        if let Some(filename) = include {
            let mut includes = self.includes.lock();
            let refresh = includes.entry(filename.clone()).or_insert_with(|| {
                let loader = self.loader.clone();
                let target = filename.clone();
                Arc::new(SerialRefresh::new(
                    self.context.clone(),
                    filename.clone(),
                    Arc::new(move || {
                        let loader = loader.clone();
                        let target = target.clone();
                        Box::pin(async move {
                            loader.refresh_include_path(&target).await?;
                            Ok(())
                        })
                    }),
                ))
            });
            refresh.trigger();
            return false;
        }
        if kind != ChangeKind::Change {
            return false;
        }
        match self.loader.classify_hmr_path(&path) {
            Ok(HostHmrChange::External) => self.restart().await,
            Ok(HostHmrChange::Loaded) => {
                if !pending.contains(&path) {
                    pending.push(path);
                }
                return true;
            }
            Ok(HostHmrChange::Untracked) => {
                let _ =
                    self.context
                        .events()
                        .emit(&self.context, "hmr/change", &EventArgs::one(path));
            }
            Err(error) => report_loader_failure(&self.context, &error),
        }
        false
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Add,
    Change,
    Unlink,
}

impl ChangeKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Change => "change",
            Self::Unlink => "unlink",
        }
    }
}

fn report_loader_failure(context: &Context, error: &LoaderError) {
    if let Err(error) = crate::error::handle_loader_error(context, error) {
        context.logger(None).warn([json!(error.to_string())]);
    }
}

async fn report_config_failure(context: &Context, filename: &Path, error: String) {
    context
        .logger(None)
        .warn([json!("config reload at %C failed"), json!(filename)]);
    context.logger(None).warn([json!(error)]);
    if let Err(rejection) = context
        .events()
        .parallel(
            context,
            "hmr/config-update-failed",
            &EventArgs::from_values(vec![Arc::new(filename.to_owned()), Arc::new(error)]),
        )
        .await
    {
        context.logger(None).warn([json!(rejection.to_string())]);
    }
}

fn resolve_base(context: &Context, configured: Option<&Path>) -> anyhow::Result<PathBuf> {
    let base = context
        .meta("loader.base_url")
        .and_then(|value| value.as_str().and_then(|value| url::Url::parse(value).ok()))
        .unwrap_or(
            url::Url::from_directory_path(std::env::current_dir()?)
                .map_err(|()| anyhow::anyhow!("cannot resolve HMR base directory"))?,
        );
    let configured = configured.unwrap_or_else(|| Path::new("."));
    let url = base.join(&configured.to_string_lossy())?;
    url.to_file_path()
        .map_err(|()| anyhow::anyhow!("HMR base must resolve to a file URL"))
}

fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn relative_path(base: &Path, path: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = base
        .iter()
        .zip(&path)
        .take_while(|(left, right)| left == right)
        .count();
    let mut output = PathBuf::new();
    for _ in common..base.len() {
        output.push("..");
    }
    for component in &path[common..] {
        output.push(component.as_os_str());
    }
    output
}

struct WatchTarget {
    filename: PathBuf,
    root: PathBuf,
    depth: usize,
}

fn find_watch_root(filename: &Path) -> anyhow::Result<WatchTarget> {
    let mut root = filename.parent().unwrap_or(filename).to_owned();
    let mut depth = 0;
    loop {
        match std::fs::metadata(&root) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.is_dir(),
                    "config watch parent is not a directory: {}",
                    root.display()
                );
                let canonical = root.canonicalize()?;
                return Ok(WatchTarget {
                    filename: resolve_path(&canonical, &relative_path(&root, filename)),
                    root: canonical,
                    depth,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = root.parent().ok_or(error)?.to_owned();
                root = parent;
                depth += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
