//! Local managed subprocess provider.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use path_clean::PathClean as _;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use seekdeep_process_exit_hook::{ProcessExitTarget, register as register_process_exit};
use seekdeep_subprocess::{
    SubprocessEnvironment, SubprocessHandle as _, SubprocessHandleRef, SubprocessLookupEnvironment,
    SubprocessRuntime, SubprocessService, SubprocessSpawnSpec, SubprocessTerminalHandle as _,
    SubprocessTerminalHandleRef, SubprocessTerminalSpawnSpec,
};
use uuid::Uuid;

/// Explained-empty invariant companion.
pub mod invariant;
/// Bounded output collector used by local process streams.
pub mod output;
/// Platform process-table inspection used by terminal ownership.
pub mod process_inspector;
mod spawn;
mod terminal;

pub use spawn::{
    LinuxGroupProbeFn, LocalSubprocessHandle, MAX_TIMER_DELAY_MS, SpawnInternals, SpawnPlatform,
    TaskkillFn, child_env, kill_group, spawn_subprocess, spawn_subprocess_with,
    taskkill_process_tree,
};
pub use terminal::LocalTerminalHandle;

/// Aggregated normal-disposal failures when more than one managed target cannot quiesce.
#[derive(Debug)]
pub struct LocalSubprocessTeardownError {
    failures: Vec<anyhow::Error>,
}

impl LocalSubprocessTeardownError {
    /// Individual target failures in completion order.
    #[must_use]
    pub fn failures(&self) -> &[anyhow::Error] {
        &self.failures
    }
}

impl std::fmt::Display for LocalSubprocessTeardownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local subprocess teardown failed")
    }
}

impl std::error::Error for LocalSubprocessTeardownError {}

/// Concrete local provider with lifecycle ownership over every spawned tree.
#[derive(Debug, Default)]
pub struct LocalSubprocessRuntime {
    live: Arc<Mutex<BTreeMap<Uuid, Arc<LocalSubprocessHandle>>>>,
    terminals: Arc<Mutex<BTreeMap<Uuid, Arc<LocalTerminalHandle>>>>,
    spawn_internals: SpawnInternals,
    terminal_inspector: Option<Arc<dyn process_inspector::ProcessInspector>>,
}

impl LocalSubprocessRuntime {
    /// Creates a provider using the owner-private default spill directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a provider with a deterministic spill directory for tests or embedding.
    #[must_use]
    pub fn with_spill_dir(path: impl Into<PathBuf>) -> Self {
        Self {
            live: Arc::default(),
            terminals: Arc::default(),
            spawn_internals: SpawnInternals {
                spill_dir: Some(path.into()),
                ..SpawnInternals::default()
            },
            terminal_inspector: None,
        }
    }

    /// Creates a provider with an injected spawn host boundary.
    #[must_use]
    pub fn with_spawn_internals(spawn_internals: SpawnInternals) -> Self {
        Self {
            live: Arc::default(),
            terminals: Arc::default(),
            spawn_internals,
            terminal_inspector: None,
        }
    }

    /// Creates a provider with an injected terminal process inspector.
    #[must_use]
    pub fn with_terminal_inspector(
        terminal_inspector: Arc<dyn process_inspector::ProcessInspector>,
    ) -> Self {
        Self {
            live: Arc::default(),
            terminals: Arc::default(),
            spawn_internals: SpawnInternals::default(),
            terminal_inspector: Some(terminal_inspector),
        }
    }

    /// Installs the service and its process-quiescence cleanup in one Cordis owner.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn install(context: &Context) -> anyhow::Result<Arc<Self>> {
        Self::install_runtime(context, Arc::new(Self::new()))
    }

    /// Installs a caller-configured runtime.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn install_runtime(context: &Context, runtime: Arc<Self>) -> anyhow::Result<Arc<Self>> {
        let mut exit_registration = register_process_exit(&runtime)?;
        let provider: Arc<dyn SubprocessRuntime> = runtime.clone();
        SubprocessService::new(provider).provide(context)?;
        let cleanup = runtime.clone();
        context.own(EffectHandle::new("local subprocess teardown", move || {
            Box::pin(async move {
                let outcome = cleanup.dispose_managed_processes().await;
                exit_registration.unregister();
                outcome
            })
        }))?;
        Ok(runtime)
    }

    /// Number of ordinary process trees still owned by this runtime.
    #[must_use]
    pub fn live_process_count(&self) -> usize {
        self.live.lock().len()
    }

    /// Number of terminal sessions still owned by this runtime.
    #[must_use]
    pub fn live_terminal_count(&self) -> usize {
        self.terminals.lock().len()
    }

    async fn dispose_managed_processes(&self) -> anyhow::Result<()> {
        let handles = self.live.lock().values().cloned().collect::<Vec<_>>();
        let terminals = self.terminals.lock().values().cloned().collect::<Vec<_>>();
        for handle in &handles {
            handle.terminate();
        }
        let mut pending: Vec<Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>> = Vec::new();
        for handle in handles {
            pending.push(Box::pin(async move {
                let _ = handle.done().await;
                anyhow::ensure!(
                    handle.wait_for_exit(None).await?,
                    "process tree {} did not exit",
                    handle.pid().as_i64()
                );
                Ok(())
            }));
        }
        for terminal in terminals {
            pending.push(Box::pin(async move { terminal.terminate().await }));
        }
        let failures = futures::future::join_all(pending)
            .await
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            self.terminate_for_host_exit();
        }
        self.live.lock().clear();
        self.terminals.lock().clear();
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.into_iter().next().expect("one failure")),
            _ => Err(LocalSubprocessTeardownError { failures }.into()),
        }
    }

    fn terminate_for_host_exit(&self) {
        for handle in self.live.lock().values() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handle.terminate_for_host_exit();
            }));
        }
        for terminal in self.terminals.lock().values() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                terminal.terminate_for_host_exit();
            }));
        }
    }
}

impl Drop for LocalSubprocessRuntime {
    fn drop(&mut self) {
        self.terminate_for_host_exit();
    }
}

impl ProcessExitTarget for LocalSubprocessRuntime {
    fn terminate_for_process_exit(&self) {
        self.terminate_for_host_exit();
    }
}

#[async_trait]
impl SubprocessRuntime for LocalSubprocessRuntime {
    async fn resolve_executable(
        &self,
        command: &str,
        env: Option<&SubprocessLookupEnvironment>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !command.is_empty(),
            "subprocess-local: executable must be non-empty"
        );
        check_abort(signal.as_ref())?;
        let explicit = env.map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect::<SubprocessEnvironment>()
        });
        let environment = child_env(explicit.as_ref());
        let path = Path::new(command);
        let absolute = path.is_absolute();
        if !absolute && has_path_separator(command) {
            anyhow::bail!(
                "subprocess-local: command {command:?} is a relative path; use an absolute path or a bare PATH name"
            );
        }
        let candidates = if absolute {
            vec![path.to_path_buf()]
        } else {
            executable_candidates(command, &environment)
        };
        for candidate in candidates {
            check_abort(signal.as_ref())?;
            if is_executable_file(&candidate) {
                check_abort(signal.as_ref())?;
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
        check_abort(signal.as_ref())?;
        if absolute {
            anyhow::bail!("subprocess-local: command {command:?} is not an executable file");
        }
        anyhow::bail!("subprocess-local: command {command:?} was not found on PATH")
    }

    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        let handle = spawn_subprocess_with(spec, self.spawn_internals.clone())?;
        let id = Uuid::now_v7();
        self.live.lock().insert(id, handle.clone());
        let live = self.live.clone();
        let release = handle.clone();
        tokio::spawn(async move {
            let _ = release.done().await;
            let _ = release.wait_for_exit(None).await;
            live.lock().remove(&id);
        });
        Ok(handle)
    }

    async fn spawn_terminal(
        &self,
        spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef> {
        let program = spec.argv.first().filter(|program| !program.is_empty());
        anyhow::ensure!(
            program.is_some(),
            "subprocess-local: terminal argv must contain a program"
        );
        check_abort(spec.signal.as_ref())?;
        let grace = terminal_grace(spec.grace_ms);
        let inspector = self
            .terminal_inspector
            .clone()
            .map_or_else(process_inspector::create_process_inspector, Ok)?;
        let terminal = LocalTerminalHandle::spawn(&spec, inspector, grace)?;
        let id = Uuid::now_v7();
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

fn terminal_grace(value: f64) -> std::time::Duration {
    if !value.is_finite() || !(1.0..=MAX_TIMER_DELAY_MS).contains(&value) {
        return std::time::Duration::from_millis(1);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    std::time::Duration::from_millis(value as u64)
}

fn executable_candidates(
    command: &str,
    environment: &BTreeMap<OsString, OsString>,
) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    executable_candidates_for(command, environment, cfg!(windows), &cwd)
}

fn executable_candidates_for(
    command: &str,
    environment: &BTreeMap<OsString, OsString>,
    windows: bool,
    cwd: &Path,
) -> Vec<PathBuf> {
    let path = environment_value(environment, "PATH", windows).unwrap_or_default();
    let directories = if windows {
        path.to_string_lossy()
            .split(';')
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    } else {
        std::env::split_paths(&path).collect()
    };
    let extensions = if windows && Path::new(command).extension().is_none() {
        environment_value(environment, "PATHEXT", true)
            .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"))
            .to_string_lossy()
            .split(';')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        vec![String::new()]
    };
    directories
        .into_iter()
        .flat_map(|directory| {
            let directory = if directory.is_absolute() {
                directory
            } else {
                cwd.join(directory)
            };
            extensions
                .iter()
                .map(move |extension| directory.join(format!("{command}{extension}")).clean())
        })
        .collect()
}

fn environment_value(
    environment: &BTreeMap<OsString, OsString>,
    name: &str,
    case_insensitive: bool,
) -> Option<OsString> {
    environment.get(&OsString::from(name)).cloned().or_else(|| {
        case_insensitive.then(|| {
            environment
                .iter()
                .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        })?
    })
}

fn has_path_separator(command: &str) -> bool {
    command.contains('/') || (cfg!(windows) && command.contains('\\'))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn check_abort(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    let Some(signal) = signal.filter(|signal| signal.is_aborted()) else {
        return Ok(());
    };
    match signal.reason() {
        Some(serde_json::Value::String(reason)) => anyhow::bail!(reason),
        Some(reason) => anyhow::bail!(reason.to_string()),
        None => anyhow::bail!("aborted"),
    }
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::Context;
    use seekdeep_subprocess::{SUBPROCESS, SubprocessRuntime as _};

    use super::*;

    #[tokio::test]
    async fn resolves_absolute_path_names_and_rejects_relative_paths() {
        let runtime = LocalSubprocessRuntime::new();
        let shell = runtime
            .resolve_executable("sh", None, None)
            .await
            .expect("PATH shell");
        assert!(Path::new(&shell).is_absolute());
        assert_eq!(
            runtime
                .resolve_executable("./sh", None, None)
                .await
                .unwrap_err()
                .to_string(),
            "subprocess-local: command \"./sh\" is a relative path; use an absolute path or a bare PATH name"
        );
        assert!(runtime.resolve_executable("", None, None).await.is_err());
    }

    #[tokio::test]
    async fn install_provides_and_disposal_joins_owned_processes() {
        let context = Context::new();
        let runtime = LocalSubprocessRuntime::install(&context).unwrap();
        assert!(context.get(SUBPROCESS).is_some());
        assert_eq!(runtime.live_process_count(), 0);
        context.fiber().dispose().await.unwrap();
        assert!(context.get(SUBPROCESS).is_none());
    }

    #[test]
    fn windows_candidates_honor_case_insensitive_path_and_pathext() {
        let environment = BTreeMap::from([
            (OsString::from("Path"), OsString::from("/bin")),
            (OsString::from("PathExt"), OsString::from(".EXE;.CMD")),
        ]);
        assert_eq!(
            executable_candidates_for("tool", &environment, true, Path::new("/cwd")),
            vec![
                PathBuf::from("/bin/tool.EXE"),
                PathBuf::from("/bin/tool.CMD")
            ]
        );
        let exact_wins = BTreeMap::from([
            (OsString::from("Path"), OsString::from("/ambient")),
            (OsString::from("PATH"), OsString::from("/explicit")),
            (OsString::from("PATHEXT"), OsString::from(".EXE")),
        ]);
        assert_eq!(
            executable_candidates_for("tool", &exact_wins, true, Path::new("/cwd")),
            vec![PathBuf::from("/explicit/tool.EXE")]
        );
    }

    #[tokio::test]
    async fn explained_empty_invariant_requires_only_the_registry() {
        let context = Context::new();
        let registry = seekdeep_invariants::InvariantRegistry::install(
            &context,
            &seekdeep_invariants::InvariantConfig::default(),
        )
        .unwrap();
        let registration = invariant::register_invariant(&registry).unwrap();
        registration.await_ready().await.unwrap();
        assert!(registry.is_registered("seekdeep-subprocess-local"));
        registration.dispose().await.unwrap();
    }
}
