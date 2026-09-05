//! Configured generic stdio providers, workspace pooling, and Cordis ownership.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{
    FutureExt as _, StreamExt as _,
    future::{BoxFuture, join_all},
    stream::FuturesUnordered,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_fs::{FS, FileSystem, FsTargetKey};
use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{
    LSP, LSP_DISPOSED, Lsp, LspError, LspProvider, LspProviderId, LspProviderQuery, LspQueryResult,
};
use seekdeep_schemastery::Schema;
use seekdeep_subprocess::{SUBPROCESS, SubprocessService};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    HostWorkspace, InstanceSpec, LspInstance, abort_error, canonicalize_workspace, read_host_source,
};

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "lsp-stdio";
/// Required filesystem, LSP registry, and subprocess services.
pub const INJECT: &[&str] = &["fs", "lsp", "subprocess"];

const DEFAULT_MAX_MESSAGE_BYTES: f64 = 16_000_000.0;
const DEFAULT_MAX_STDERR_BYTES: f64 = 1_000_000.0;
const DEFAULT_MAX_DOCUMENT_BYTES: f64 = 4_000_000.0;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: f64 = 5_000.0;
const DEFAULT_KILL_GRACE_MS: f64 = 2_000.0;

/// One configured language server and its host bounds before defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LspLocalServerConfig {
    /// Executable to resolve at load.
    pub command: String,
    /// Lowercase leading-dot extension to language-id mappings.
    pub extension_to_language: IndexMap<String, String>,
    /// Arguments passed without a shell.
    #[serde(default)]
    pub args: Vec<String>,
    /// Explicit environment merged after ambient scrubbing.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Static initialize options; missing defaults to null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<Value>,
    /// Static workspace/configuration value; missing defaults to null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<Value>,
    /// Largest accepted framed message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_message_bytes: Option<f64>,
    /// Largest retained stderr tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_stderr_bytes: Option<f64>,
    /// Largest source opened by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_document_bytes: Option<f64>,
    /// Graceful shutdown budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_timeout_ms: Option<f64>,
    /// Request cancellation and TERM-to-KILL grace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kill_grace_ms: Option<f64>,
}

/// Provider-id to independent server configurations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Non-empty table of stable provider ids.
    pub servers: IndexMap<String, LspLocalServerConfig>,
}

#[derive(Clone, Debug)]
struct ResolvedServerConfig {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    extension_to_language: IndexMap<String, String>,
    initialization_options: Value,
    configuration: Value,
    max_message_bytes: usize,
    max_stderr_bytes: usize,
    max_document_bytes: usize,
    shutdown_timeout_ms: f64,
    kill_grace_ms: f64,
}

/// Source-compatible plugin configuration schema.
#[must_use]
pub fn config_schema() -> Schema {
    let server = Schema::object([
        ("command", Schema::string().required()),
        ("args", Schema::array(Schema::string())),
        ("env", Schema::dict(Schema::string())),
        (
            "extensionToLanguage",
            Schema::dict(Schema::string()).required(),
        ),
        (
            "initializationOptions",
            Schema::any().with_default(Value::Null),
        ),
        ("configuration", Schema::any().with_default(Value::Null)),
        (
            "maxMessageBytes",
            Schema::number().with_default(DEFAULT_MAX_MESSAGE_BYTES),
        ),
        (
            "maxStderrBytes",
            Schema::number().with_default(DEFAULT_MAX_STDERR_BYTES),
        ),
        (
            "maxDocumentBytes",
            Schema::number().with_default(DEFAULT_MAX_DOCUMENT_BYTES),
        ),
        (
            "shutdownTimeoutMs",
            Schema::number()
                .max(MAX_TIMER_DELAY_MS)
                .with_default(DEFAULT_SHUTDOWN_TIMEOUT_MS),
        ),
        (
            "killGraceMs",
            Schema::number()
                .max(MAX_TIMER_DELAY_MS)
                .with_default(DEFAULT_KILL_GRACE_MS),
        ),
    ]);
    Schema::object([("servers", Schema::dict(server).required())])
}

/// Loader-facing Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            apply(&context, config).await
        })
    })
    .with_config_validator(|value| {
        let config: Config = serde_json::from_value(value.clone())?;
        validate_config(&config)?;
        Ok(value.clone())
    })
}

/// Resolves every executable, atomically registers every provider, and owns teardown.
///
/// # Errors
///
/// Returns missing services, invalid configuration, lookup, registration,
/// cancellation, ownership, or rollback failures.
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    validate_config(&config)?;
    let filesystem = context
        .get(FS)
        .ok_or_else(|| anyhow::anyhow!("lsp-stdio requires fs"))?
        .filesystem();
    let lsp = context
        .get(LSP)
        .ok_or_else(|| anyhow::anyhow!("lsp-stdio requires lsp"))?;
    let subprocess = context
        .get(SUBPROCESS)
        .ok_or_else(|| anyhow::anyhow!("lsp-stdio requires subprocess"))?;
    let resolved = config
        .servers
        .into_iter()
        .map(|(id, config)| resolve_config(&id, config).map(|config| (id, config)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let providers = resolve_providers(context, filesystem, subprocess, resolved).await?;
    register_provider_table(context, &lsp, providers).await
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        !config.servers.is_empty(),
        "lsp-stdio: servers must contain at least one server"
    );
    for (provider_id, config) in &config.servers {
        let _ = resolve_config(provider_id, config.clone())?;
    }
    Ok(())
}

fn resolve_config(
    provider_id: &str,
    config: LspLocalServerConfig,
) -> anyhow::Result<ResolvedServerConfig> {
    anyhow::ensure!(
        !provider_id.trim().is_empty(),
        "lsp-stdio: server ids must be non-empty strings"
    );
    let shutdown_timeout_ms = config
        .shutdown_timeout_ms
        .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS);
    let kill_grace_ms = config.kill_grace_ms.unwrap_or(DEFAULT_KILL_GRACE_MS);
    assert_timer(provider_id, "shutdownTimeoutMs", shutdown_timeout_ms)?;
    assert_timer(provider_id, "killGraceMs", kill_grace_ms)?;
    let max_message_bytes = config
        .max_message_bytes
        .unwrap_or(DEFAULT_MAX_MESSAGE_BYTES);
    let max_stderr_bytes = config.max_stderr_bytes.unwrap_or(DEFAULT_MAX_STDERR_BYTES);
    let max_document_bytes = config
        .max_document_bytes
        .unwrap_or(DEFAULT_MAX_DOCUMENT_BYTES);
    assert_positive_integer(provider_id, "maxStderrBytes", max_stderr_bytes)?;
    assert_positive_integer(provider_id, "maxMessageBytes", max_message_bytes)?;
    assert_positive_integer(provider_id, "maxDocumentBytes", max_document_bytes)?;
    Ok(ResolvedServerConfig {
        command: config.command,
        args: config.args,
        env: config.env,
        extension_to_language: config.extension_to_language,
        initialization_options: config.initialization_options.unwrap_or(Value::Null),
        configuration: config.configuration.unwrap_or(Value::Null),
        max_message_bytes: cap_to_usize(max_message_bytes),
        max_stderr_bytes: cap_to_usize(max_stderr_bytes),
        max_document_bytes: cap_to_usize(max_document_bytes),
        shutdown_timeout_ms,
        kill_grace_ms,
    })
}

fn assert_timer(provider_id: &str, name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && (1.0..=MAX_TIMER_DELAY_MS).contains(&value),
        "lsp-stdio: servers.{provider_id}.{name} must be a positive integer no greater than {MAX_TIMER_DELAY_MS}"
    );
    Ok(())
}

fn assert_positive_integer(provider_id: &str, name: &str, value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && value >= 1.0,
        "lsp-stdio: servers.{provider_id}.{name} must be a positive integer"
    );
    Ok(())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn cap_to_usize(value: f64) -> usize {
    value as usize
}

async fn resolve_providers(
    context: &Context,
    filesystem: Arc<dyn FileSystem>,
    subprocess: Arc<SubprocessService>,
    entries: Vec<(String, ResolvedServerConfig)>,
) -> anyhow::Result<Vec<Arc<LocalLspProvider>>> {
    let setup_signal = AbortSignal::default();
    let bridge_signal = setup_signal.clone();
    let fiber = context.fiber().clone();
    let (stop_sender, stop_receiver) = tokio::sync::oneshot::channel();
    let bridge = tokio::spawn(async move {
        tokio::select! {
            () = fiber.when_disposing() => {
                bridge_signal.abort_with_reason(Value::String("lsp-stdio setup disposed".to_owned()));
            }
            _ = stop_receiver => {}
        }
    });
    let mut lookups = entries
        .into_iter()
        .enumerate()
        .map(|(index, (id, config))| {
            let subprocess = subprocess.clone();
            let signal = setup_signal.clone();
            async move {
                let executable = subprocess
                    .resolve_executable(&config.command, Some(&config.env), Some(signal))
                    .await?;
                Ok::<_, anyhow::Error>((index, id, config, executable))
            }
        })
        .collect::<FuturesUnordered<_>>();
    let mut first_error = None;
    let mut resolved = Vec::new();
    while let Some(outcome) = lookups.next().await {
        match outcome {
            Ok(provider) => resolved.push(provider),
            Err(error) => {
                if first_error.is_none() {
                    setup_signal.abort_with_reason(Value::String(error.to_string()));
                    first_error = Some(error);
                }
            }
        }
    }
    let _ = stop_sender.send(());
    let _ = bridge.await;
    if context.fiber().is_disposal_requested() {
        anyhow::bail!("lsp-stdio setup disposed");
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    resolved.sort_by_key(|(index, _, _, _)| *index);
    Ok(resolved
        .into_iter()
        .map(|(_, id, config, executable)| {
            Arc::new(LocalLspProvider::new(
                id,
                filesystem.clone(),
                config,
                executable,
                subprocess.clone(),
            ))
        })
        .collect())
}

async fn register_provider_table(
    context: &Context,
    lsp: &Lsp,
    providers: Vec<Arc<LocalLspProvider>>,
) -> anyhow::Result<()> {
    let mut registrations = Vec::new();
    for provider in &providers {
        let provider: Arc<dyn LspProvider> = provider.clone();
        match lsp.register_provider_unowned(provider) {
            Ok(registration) => registrations.push(registration),
            Err(error) => {
                for registration in registrations.into_iter().rev() {
                    let _ = registration.dispose().await;
                }
                return Err(error);
            }
        }
    }
    let cleanup_providers = providers;
    let cleanup = EffectHandle::new("lsp-stdio.registerProviders", move || {
        Box::pin(async move {
            let mut failures = Vec::new();
            for registration in registrations.into_iter().rev() {
                if let Err(error) = registration.dispose().await {
                    failures.push(error);
                }
            }
            let disposals = cleanup_providers
                .into_iter()
                .map(|provider| async move { provider.dispose_all().await }.boxed())
                .collect::<Vec<_>>();
            failures.extend(collect_teardown_failures(disposals).await);
            teardown_result(failures, "lsp-stdio provider teardown failed")
        })
    });
    if let Err(error) = context.own(cleanup.clone()) {
        let _ = cleanup.dispose().await;
        return Err(error.into());
    }
    Ok(())
}

#[derive(Debug)]
struct ActivityTracker {
    active: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl ActivityTracker {
    fn enter(self: &Arc<Self>) -> ActivityGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        ActivityGuard(self.clone())
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.notify.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct ActivityGuard(Arc<ActivityTracker>);

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        if self.0.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.notify.notify_waiters();
        }
    }
}

#[derive(Default)]
struct ProviderState {
    disposed: bool,
    instances: IndexMap<FsTargetKey, LspInstance>,
}

#[derive(Default)]
struct ProviderDisposal {
    started: bool,
    outcome: Option<Result<(), Vec<String>>>,
}

struct ProviderInner {
    id: LspProviderId,
    filesystem: Arc<dyn FileSystem>,
    config: ResolvedServerConfig,
    executable: String,
    subprocess: Arc<SubprocessService>,
    state: Mutex<ProviderState>,
    queues: Mutex<HashMap<FsTargetKey, Arc<tokio::sync::Mutex<()>>>>,
    activity: Arc<ActivityTracker>,
    lifetime: AbortSignal,
    disposal: Mutex<ProviderDisposal>,
    disposal_notify: tokio::sync::Notify,
}

/// One lazily pooled generic provider for a configured server entry.
struct LocalLspProvider {
    inner: Arc<ProviderInner>,
}

impl std::fmt::Debug for LocalLspProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalLspProvider")
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

impl LocalLspProvider {
    fn new(
        id: String,
        filesystem: Arc<dyn FileSystem>,
        config: ResolvedServerConfig,
        executable: String,
        subprocess: Arc<SubprocessService>,
    ) -> Self {
        Self {
            inner: Arc::new(ProviderInner {
                id: LspProviderId::new(id),
                filesystem,
                config,
                executable,
                subprocess,
                state: Mutex::new(ProviderState::default()),
                queues: Mutex::new(HashMap::new()),
                activity: Arc::new(ActivityTracker::default()),
                lifetime: AbortSignal::default(),
                disposal: Mutex::new(ProviderDisposal::default()),
                disposal_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    async fn dispose_all(&self) -> anyhow::Result<()> {
        self.inner.clone().dispose_all().await
    }
}

#[async_trait]
impl LspProvider for LocalLspProvider {
    fn id(&self) -> &LspProviderId {
        &self.inner.id
    }

    fn extension_to_language(&self) -> &IndexMap<String, String> {
        &self.inner.config.extension_to_language
    }

    async fn query(
        &self,
        request: LspProviderQuery,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        self.inner.clone().query(request, signal).await
    }
}

impl ProviderInner {
    async fn query(
        self: Arc<Self>,
        request: LspProviderQuery,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        let _activity = self.activity.enter();
        self.assert_active(signal.as_ref())?;
        let signal = signal.map_or_else(
            || self.lifetime.clone(),
            |signal| AbortSignal::fuse(&signal, &self.lifetime),
        );
        let workspace = canonicalize_workspace(
            self.filesystem.as_ref(),
            &request.workspace_root,
            Some(&signal),
        )
        .await?;
        self.assert_active(Some(&signal))?;
        let workspace_key = workspace.target.target_key.clone();
        let queue = self
            .queues
            .lock()
            .entry(workspace_key.clone())
            .or_default()
            .clone();
        let _queue = lock_abortably(&queue, &signal).await?;
        self.assert_active(Some(&signal))?;
        let source = read_host_source(
            self.filesystem.as_ref(),
            &request.file_path,
            &workspace,
            self.config.max_document_bytes,
            Some(&signal),
        )
        .await?;
        self.assert_active(Some(&signal))?;
        let mut instance = self.instance_for(&workspace_key, &workspace)?;
        let first = instance
            .query(request.clone(), source.clone(), Some(&signal))
            .await;
        let outcome = match first {
            Err(error) if instance.is_transport_failure(&error) => {
                instance.dispose().await?;
                self.evict_if_current(&workspace_key, &instance);
                self.assert_active(Some(&signal))?;
                instance = self.instance_for(&workspace_key, &workspace)?;
                instance.query(request, source, Some(&signal)).await
            }
            outcome => outcome,
        };
        if instance.dead() {
            instance.dispose().await?;
            self.evict_if_current(&workspace_key, &instance);
        }
        outcome
    }

    fn assert_active(&self, signal: Option<&AbortSignal>) -> anyhow::Result<()> {
        if self.state.lock().disposed {
            return Err(LspError::new("lsp-stdio provider is disposed", LSP_DISPOSED).into());
        }
        if let Some(signal) = signal.filter(|signal| signal.is_aborted()) {
            return Err(abort_error(signal));
        }
        Ok(())
    }

    fn instance_for(
        &self,
        workspace_key: &FsTargetKey,
        workspace: &HostWorkspace,
    ) -> anyhow::Result<LspInstance> {
        let mut state = self.state.lock();
        if state.disposed {
            return Err(LspError::new("lsp-stdio provider is disposed", LSP_DISPOSED).into());
        }
        if let Some(instance) = state.instances.get(workspace_key) {
            return Ok(instance.clone());
        }
        let instance = LspInstance::new(
            InstanceSpec {
                command: self.executable.clone(),
                args: self.config.args.clone(),
                cwd: self.filesystem.process_path(&workspace.target).into(),
                env: self.config.env.clone(),
                max_message_bytes: self.config.max_message_bytes,
                max_stderr_bytes: self.config.max_stderr_bytes,
                kill_grace_ms: self.config.kill_grace_ms,
                configuration: Some(self.config.configuration.clone()),
                workspace_uri: workspace.file_url.clone(),
                initialization_options: Some(self.config.initialization_options.clone()),
                shutdown_timeout_ms: self.config.shutdown_timeout_ms,
            },
            &**self.subprocess,
            None,
        )?;
        state
            .instances
            .insert(workspace_key.clone(), instance.clone());
        Ok(instance)
    }

    fn evict_if_current(&self, workspace_key: &FsTargetKey, instance: &LspInstance) {
        let mut state = self.state.lock();
        if state
            .instances
            .get(workspace_key)
            .is_some_and(|current| current.same_instance(instance))
        {
            state.instances.shift_remove(workspace_key);
        }
    }

    async fn dispose_all(self: Arc<Self>) -> anyhow::Result<()> {
        let start = {
            let mut disposal = self.disposal.lock();
            if disposal.started {
                false
            } else {
                disposal.started = true;
                true
            }
        };
        if start {
            let instances = {
                let mut state = self.state.lock();
                state.disposed = true;
                std::mem::take(&mut state.instances)
                    .into_values()
                    .collect::<Vec<_>>()
            };
            self.lifetime.abort_with_error(
                Arc::new(LspError::new(
                    "lsp-stdio provider is disposed",
                    LSP_DISPOSED,
                )),
                json!({
                    "name": "LspError",
                    "message": "lsp-stdio provider is disposed",
                    "code": LSP_DISPOSED,
                }),
            );
            let inner = self.clone();
            tokio::spawn(async move {
                let mut failures = Vec::new();
                let disposals = instances
                    .into_iter()
                    .map(|instance| async move { instance.dispose().await }.boxed())
                    .collect::<Vec<_>>();
                failures.extend(
                    collect_teardown_failures(disposals)
                        .await
                        .into_iter()
                        .map(|error| error.to_string()),
                );
                inner.activity.wait_idle().await;
                inner.queues.lock().clear();
                inner.disposal.lock().outcome = Some(if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures)
                });
                inner.disposal_notify.notify_waiters();
            });
        }
        self.wait_disposal().await
    }

    async fn wait_disposal(&self) -> anyhow::Result<()> {
        loop {
            let notified = self.disposal_notify.notified();
            if let Some(outcome) = self.disposal.lock().outcome.clone() {
                return match outcome {
                    Ok(()) => Ok(()),
                    Err(failures) if failures.len() == 1 => {
                        Err(anyhow::anyhow!(failures[0].clone()))
                    }
                    Err(failures) => Err(TeardownFailures {
                        message: "lsp-stdio instance teardown failed",
                        failures,
                    }
                    .into()),
                };
            }
            notified.await;
        }
    }
}

async fn lock_abortably<'a>(
    queue: &'a tokio::sync::Mutex<()>,
    signal: &AbortSignal,
) -> anyhow::Result<tokio::sync::MutexGuard<'a, ()>> {
    if signal.is_aborted() {
        return Err(abort_error(signal));
    }
    tokio::select! {
        biased;
        guard = queue.lock() => Ok(guard),
        () = signal.cancelled() => Err(abort_error(signal)),
    }
}

/// Aggregate emitted only after every sibling cleanup has settled.
#[derive(Debug)]
pub struct TeardownFailures {
    message: &'static str,
    failures: Vec<String>,
}

impl std::fmt::Display for TeardownFailures {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TeardownFailures {}

impl TeardownFailures {
    /// Individual cleanup failures in settlement order.
    #[must_use]
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
}

fn teardown_result(failures: Vec<anyhow::Error>, message: &'static str) -> anyhow::Result<()> {
    match failures.len() {
        0 => Ok(()),
        1 => Err(failures.into_iter().next().expect("one failure")),
        _ => Err(TeardownFailures {
            message,
            failures: failures
                .into_iter()
                .map(|error| error.to_string())
                .collect(),
        }
        .into()),
    }
}

async fn collect_teardown_failures(
    tasks: Vec<BoxFuture<'static, anyhow::Result<()>>>,
) -> Vec<anyhow::Error> {
    join_all(tasks)
        .await
        .into_iter()
        .filter_map(Result::err)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn teardown_aggregation_waits_for_every_sibling_and_preserves_input_order() {
        for message in [
            "lsp-stdio instance teardown failed",
            "lsp-stdio provider teardown failed",
        ] {
            let (release, wait) = tokio::sync::oneshot::channel();
            let tasks = vec![
                async { Err(anyhow::anyhow!("first cleanup failed")) }.boxed(),
                async move {
                    let _ = wait.await;
                    Err(anyhow::anyhow!("second cleanup failed"))
                }
                .boxed(),
            ];
            let settling = tokio::spawn(async move {
                teardown_result(collect_teardown_failures(tasks).await, message)
            });
            tokio::task::yield_now().await;
            assert!(!settling.is_finished());
            release.send(()).unwrap();
            let error = settling.await.unwrap().unwrap_err();
            let aggregate = error.downcast_ref::<TeardownFailures>().unwrap();
            assert_eq!(aggregate.to_string(), message);
            assert_eq!(
                aggregate.failures(),
                ["first cleanup failed", "second cleanup failed"]
            );
        }
    }

    #[tokio::test]
    async fn activity_tracker_waits_for_every_pre_disposal_operation() {
        let tracker = Arc::new(ActivityTracker::default());
        let first = tracker.enter();
        let second = tracker.enter();
        let waiting_tracker = tracker.clone();
        let waiting = tokio::spawn(async move { waiting_tracker.wait_idle().await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(first);
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(second);
        waiting.await.unwrap();
    }
}
