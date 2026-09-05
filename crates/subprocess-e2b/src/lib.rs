//! E2B provider for the managed subprocess and terminal capability.

pub mod environment;
pub mod output;
pub mod process;
pub mod remote;
pub mod terminal;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_e2b::{E2B, E2bService, e2b_control_envs, quote_e2b_shell_arg};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessHandle as _, SubprocessHandleRef, SubprocessLookupEnvironment, SubprocessRuntime,
    SubprocessService, SubprocessSpawnSpec, SubprocessTerminalHandle as _,
    SubprocessTerminalHandleRef, SubprocessTerminalSpawnSpec,
};
use seekdeep_util::timeout::MAX_TIMER_DELAY_MS;

use crate::{process::E2bSubprocessHandle, terminal::spawn_e2b_terminal};

/// Cordis plugin name.
pub const NAME: &str = "subprocess-e2b";
/// Required shared execution-world owner.
pub const INJECT: &[&str] = &["e2b"];

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);
const MAX_TIMER_DELAY_MILLIS: u64 = 2_147_483_647;

/// E2B subprocess provider configuration.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct E2bSubprocessConfig {
    /// Remote status and liveness poll cadence in milliseconds.
    pub poll_ms: u64,
}

impl Default for E2bSubprocessConfig {
    fn default() -> Self {
        Self { poll_ms: 20 }
    }
}

/// Aggregate teardown failure after every owned target settles.
#[derive(Debug)]
pub struct E2bSubprocessTeardownError {
    failures: Vec<anyhow::Error>,
}

impl E2bSubprocessTeardownError {
    /// Individual cleanup failures in target order.
    #[must_use]
    pub fn failures(&self) -> &[anyhow::Error] {
        &self.failures
    }
}

impl std::fmt::Display for E2bSubprocessTeardownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("subprocess-e2b: teardown failed")?;
        for failure in &self.failures {
            write!(formatter, "\n- {failure:#}")?;
        }
        Ok(())
    }
}

impl std::error::Error for E2bSubprocessTeardownError {}

/// E2B command and PTY provider with deterministic ownership of live remote groups.
pub struct E2bSubprocessRuntime {
    e2b: Arc<E2bService>,
    poll_ms: u64,
    next_id: AtomicU64,
    live: Arc<Mutex<BTreeMap<u64, Arc<E2bSubprocessHandle>>>>,
    terminals: Arc<Mutex<BTreeMap<u64, SubprocessTerminalHandleRef>>>,
    setups: Arc<Mutex<HashMap<u64, AbortSignal>>>,
    setups_changed: Arc<tokio::sync::Notify>,
    disposing: Arc<AtomicBool>,
}

impl std::fmt::Debug for E2bSubprocessRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bSubprocessRuntime")
            .field("poll_ms", &self.poll_ms)
            .field("live", &self.live.lock().len())
            .field("terminals", &self.terminals.lock().len())
            .field("setups", &self.setups.lock().len())
            .field("disposing", &self.disposing.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl E2bSubprocessRuntime {
    /// Creates a runtime over the shared E2B owner.
    ///
    /// # Errors
    ///
    /// Rejects a zero poll cadence.
    pub fn new(e2b: Arc<E2bService>, config: E2bSubprocessConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.poll_ms > 0 && config.poll_ms <= MAX_TIMER_DELAY_MILLIS,
            "subprocess-e2b: pollMs must be a positive safe integer"
        );
        Ok(Arc::new(Self {
            e2b,
            poll_ms: config.poll_ms,
            next_id: AtomicU64::new(NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed)),
            live: Arc::default(),
            terminals: Arc::default(),
            setups: Arc::default(),
            setups_changed: Arc::new(tokio::sync::Notify::new()),
            disposing: Arc::new(AtomicBool::new(false)),
        }))
    }

    /// Installs the service and its complete remote-group teardown owner.
    ///
    /// # Errors
    ///
    /// Returns missing E2B, configuration, duplicate-service, or ownership failures.
    pub fn install(context: &Context, config: E2bSubprocessConfig) -> anyhow::Result<Arc<Self>> {
        let e2b = context
            .get(E2B)
            .ok_or_else(|| anyhow::anyhow!("subprocess-e2b requires e2b"))?;
        let runtime = Self::new(e2b, config)?;
        let provider: Arc<dyn SubprocessRuntime> = runtime.clone();
        SubprocessService::new(provider).provide(context)?;
        let cleanup = runtime.clone();
        context.own(EffectHandle::new("e2b subprocess teardown", move || {
            Box::pin(async move { cleanup.dispose().await })
        }))?;
        Ok(runtime)
    }

    /// Number of ordinary process groups still retained for cleanup.
    #[must_use]
    pub fn live_process_count(&self) -> usize {
        self.live.lock().len()
    }

    /// Number of terminal sessions still retained for cleanup.
    #[must_use]
    pub fn live_terminal_count(&self) -> usize {
        self.terminals.lock().len()
    }

    /// Number of terminal allocations currently owned before publication.
    #[must_use]
    pub fn terminal_setup_count(&self) -> usize {
        self.setups.lock().len()
    }

    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn dispose(&self) -> anyhow::Result<()> {
        self.disposing.store(true, Ordering::Release);
        for signal in self.setups.lock().values() {
            signal.abort();
        }
        loop {
            let changed = self.setups_changed.notified();
            if self.setups.lock().is_empty() {
                break;
            }
            changed.await;
        }
        let processes = self.live.lock().values().cloned().collect::<Vec<_>>();
        let terminals = self.terminals.lock().values().cloned().collect::<Vec<_>>();
        for process in &processes {
            process.terminate();
        }
        let mut pending = Vec::new();
        for process in processes {
            pending.push(
                async move {
                    anyhow::ensure!(
                        process.wait_for_exit(None).await?,
                        "E2B process group {} did not exit",
                        process.pid().as_i64()
                    );
                    let _ = process.done().await;
                    Ok(())
                }
                .boxed(),
            );
        }
        for terminal in terminals {
            pending.push(async move { terminal.terminate().await }.boxed());
        }
        let failures = futures::future::join_all(pending)
            .await
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        if failures.is_empty() {
            self.live.lock().clear();
            self.terminals.lock().clear();
            Ok(())
        } else if failures.len() == 1 {
            Err(failures.into_iter().next().expect("one failure"))
        } else {
            Err(E2bSubprocessTeardownError { failures }.into())
        }
    }
}

#[async_trait::async_trait]
impl SubprocessRuntime for E2bSubprocessRuntime {
    async fn resolve_executable(
        &self,
        command: &str,
        env: Option<&SubprocessLookupEnvironment>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !command.is_empty(),
            "subprocess-e2b: executable name must be non-empty"
        );
        ensure_not_aborted(signal.as_ref())?;
        let sandbox = self.e2b.get_sandbox().await?;
        if command.starts_with('/') {
            sandbox
                .commands()
                .run(
                    &format!(
                        "test -f {} -a -x {}",
                        quote_e2b_shell_arg(command),
                        quote_e2b_shell_arg(command)
                    ),
                    e2b_control_envs(&BTreeMap::new()),
                    signal.as_ref(),
                )
                .await?;
            ensure_not_aborted(signal.as_ref())?;
            return Ok(command.to_owned());
        }
        anyhow::ensure!(
            !command.contains('/'),
            "subprocess-e2b: command {command:?} is a relative path; use an absolute path or a bare PATH name"
        );
        let prefix = env
            .and_then(|values| values.get("PATH"))
            .map_or_else(String::new, |path| {
                format!("PATH={} ", quote_e2b_shell_arg(path))
            });
        let result = sandbox
            .commands()
            .run_in(
                &format!("{prefix}command -v -- {}", quote_e2b_shell_arg(command)),
                self.e2b.cwd(),
                e2b_control_envs(&BTreeMap::new()),
                signal.as_ref(),
            )
            .await?;
        ensure_not_aborted(signal.as_ref())?;
        let executable = result.stdout.trim();
        anyhow::ensure!(
            !executable.contains('\n') && (executable.starts_with('/') || executable.contains('/')),
            "subprocess-e2b: executable {command:?} did not resolve to one absolute path"
        );
        Ok(resolve_posix(self.e2b.cwd(), executable))
    }

    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        anyhow::ensure!(
            !self.disposing.load(Ordering::Acquire),
            "subprocess-e2b: service is disposing"
        );
        let program = spec.argv.first().filter(|program| !program.is_empty());
        anyhow::ensure!(
            program.is_some(),
            "invalid argv: expected a non-empty program name at argv[0]"
        );
        validate_grace(spec.grace_ms)?;
        if let Some(signal) = spec.signal.as_ref()
            && signal.is_aborted()
        {
            anyhow::bail!("aborted before spawn: {}", abort_reason(signal));
        }
        let id = self.allocate_id();
        let state_dir = format!("{}/processes/{id}", self.e2b.runtime_root());
        let handle = E2bSubprocessHandle::spawn(self.e2b.clone(), spec, state_dir, self.poll_ms)?;
        self.live.lock().insert(id, handle.clone());
        let live = self.live.clone();
        let release = handle.clone();
        tokio::spawn(async move {
            let _ = release.done().await;
            if matches!(release.wait_for_exit(None).await, Ok(true)) {
                live.lock().remove(&id);
            }
        });
        Ok(handle)
    }

    async fn spawn_terminal(
        &self,
        mut spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef> {
        anyhow::ensure!(
            !self.disposing.load(Ordering::Acquire),
            "subprocess-e2b: service is disposing"
        );
        anyhow::ensure!(
            spec.argv.first().is_some_and(|program| !program.is_empty()),
            "subprocess-e2b: terminal argv must contain a program"
        );
        validate_grace(spec.grace_ms)?;
        ensure_not_aborted(spec.signal.as_ref())?;
        let id = self.allocate_id();
        let setup_abort = AbortSignal::default();
        spec.signal = Some(spec.signal.as_ref().map_or_else(
            || setup_abort.clone(),
            |signal| AbortSignal::fuse(signal, &setup_abort),
        ));
        self.setups.lock().insert(id, setup_abort);
        let state_dir = format!("{}/terminals/{id}", self.e2b.runtime_root());
        let result = spawn_e2b_terminal(self.e2b.clone(), spec, state_dir, self.poll_ms).await;
        self.setups.lock().remove(&id);
        self.setups_changed.notify_waiters();
        let terminal = result?;
        if self.disposing.load(Ordering::Acquire) {
            terminal.terminate().await?;
            anyhow::bail!("subprocess-e2b: service disposed during terminal setup");
        }
        let terminal: SubprocessTerminalHandleRef = terminal;
        self.terminals.lock().insert(id, terminal.clone());
        let terminals = self.terminals.clone();
        let release = terminal.clone();
        tokio::spawn(async move {
            let _ = release.done().await;
            if release.terminate().await.is_ok() {
                terminals.lock().remove(&id);
            }
        });
        Ok(terminal)
    }
}

fn validate_grace(value: f64) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && (1.0..=MAX_TIMER_DELAY_MS).contains(&value),
        "subprocess graceMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MILLIS}"
    );
    Ok(())
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if let Some(signal) = signal
        && signal.is_aborted()
    {
        anyhow::bail!("aborted: {}", abort_reason(signal));
    }
    Ok(())
}

fn abort_reason(signal: &AbortSignal) -> String {
    signal
        .reason()
        .map_or_else(|| "null".to_owned(), |reason| reason.to_string())
}

fn resolve_posix(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        return normalize_posix(path);
    }
    normalize_posix(&format!("{}/{path}", cwd.trim_end_matches('/')))
}

fn normalize_posix(path: &str) -> String {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            component => components.push(component),
        }
    }
    format!("/{}", components.join("/"))
}

/// Builds the loader-compatible E2B subprocess plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: E2bSubprocessConfig = serde_json::from_value(config)?;
            E2bSubprocessRuntime::install(&context, config)?;
            Ok(())
        })
    })
}
