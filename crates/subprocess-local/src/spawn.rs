//! Ordinary-process spawn, output collection, and process-tree ownership.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::Path,
    pin::Pin,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    ProcessGroupId, ProcessId, ProcessSignal, SubprocessCollectedOutputs, SubprocessEnvironment,
    SubprocessHandle, SubprocessInput, SubprocessOutcome, SubprocessOutput, SubprocessOutputMode,
    SubprocessSpawnSpec, SubprocessStdinMode, scrubbed_parent_env,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    process::Command,
    task::JoinHandle,
};

use crate::output::{OutputCollector, default_spill_dir};

type ChildInput = Pin<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>;
type ChildOutput = Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>;
type NativeSpawn = (
    tokio::process::Child,
    Option<ChildInput>,
    Option<ChildOutput>,
    Option<ChildOutput>,
);

/// Largest millisecond delay representable by the source runtime timer.
pub const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
const MAX_TIMER_DELAY_MILLIS: u64 = 2_147_483_647;

/// Extensible host-platform selection used by the injectable spawn boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnPlatform {
    /// Linux process-group semantics, including post-settlement zombie inspection.
    Linux,
    /// Other POSIX process-group semantics.
    Posix,
    /// Windows direct-child liveness and `taskkill /T /F` signalling semantics.
    Windows,
    /// Unknown platform spelling retained while using the source's non-Windows path.
    Unknown(String),
}

impl SpawnPlatform {
    fn host() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(windows) {
            Self::Windows
        } else if cfg!(unix) {
            Self::Posix
        } else {
            Self::Unknown(std::env::consts::OS.to_owned())
        }
    }

    fn is_windows(&self) -> bool {
        matches!(self, Self::Windows)
    }
}

/// Injectable Windows tree-termination runner.
pub type TaskkillFn = Arc<dyn Fn(ProcessId) + Send + Sync>;
/// Injectable Linux live-member probe. `None` means inspection was inconclusive.
pub type LinuxGroupProbeFn = Arc<dyn Fn(ProcessGroupId) -> Option<bool> + Send + Sync>;

/// Injectable knobs matching the source spawn implementation's test boundary.
#[derive(Clone, Default)]
pub struct SpawnInternals {
    /// Directory for complete-stream spill files.
    pub spill_dir: Option<std::path::PathBuf>,
    /// Host platform override for detached spawn, signalling, and liveness.
    pub platform: Option<SpawnPlatform>,
    /// Windows process-tree termination override.
    pub taskkill: Option<TaskkillFn>,
    /// Linux process-group live-member probe override.
    pub linux_group_has_live_members: Option<LinuxGroupProbeFn>,
}

impl std::fmt::Debug for SpawnInternals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpawnInternals")
            .field("spill_dir", &self.spill_dir)
            .field("platform", &self.platform)
            .field("taskkill", &self.taskkill.as_ref().map(|_| "<injected>"))
            .field(
                "linux_group_has_live_members",
                &self
                    .linux_group_has_live_members
                    .as_ref()
                    .map(|_| "<injected>"),
            )
            .finish()
    }
}

#[derive(Clone)]
struct TreeOperations {
    platform: SpawnPlatform,
    taskkill: TaskkillFn,
    linux_group_has_live_members: LinuxGroupProbeFn,
}

impl TreeOperations {
    fn from_internals(internals: &SpawnInternals) -> Self {
        Self {
            platform: internals
                .platform
                .clone()
                .unwrap_or_else(SpawnPlatform::host),
            taskkill: internals
                .taskkill
                .clone()
                .unwrap_or_else(|| Arc::new(taskkill_process_tree)),
            linux_group_has_live_members: internals
                .linux_group_has_live_members
                .clone()
                .unwrap_or_else(|| Arc::new(default_linux_group_has_live_members)),
        }
    }

    fn signal(&self, pid: ProcessId, signal: TreeSignal) {
        if self.platform.is_windows() {
            (self.taskkill)(pid);
        } else {
            kill_group_with_fallback(pid, signal);
        }
    }

    fn tree_alive(&self, pid: ProcessId, direct_exited: bool, outcome_settled: bool) -> bool {
        if pid.as_i64() <= 0 {
            return false;
        }
        if self.platform.is_windows() {
            return !direct_exited;
        }
        posix_tree_alive(
            pid,
            direct_exited,
            outcome_settled,
            matches!(self.platform, SpawnPlatform::Linux),
            &self.linux_group_has_live_members,
        )
    }
}

#[derive(Clone, Debug)]
enum StoredDone {
    Success(SubprocessOutcome),
    Failure(Arc<str>),
}

#[derive(Debug, Default)]
struct DoneSlot {
    value: Mutex<Option<StoredDone>>,
    notify: tokio::sync::Notify,
}

impl DoneSlot {
    fn complete(&self, value: StoredDone) {
        let mut current = self.value.lock();
        if current.is_none() {
            *current = Some(value);
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> anyhow::Result<SubprocessOutcome> {
        loop {
            let notified = self.notify.notified();
            if let Some(value) = self.value.lock().clone() {
                return match value {
                    StoredDone::Success(outcome) => Ok(outcome),
                    StoredDone::Failure(message) => Err(anyhow::anyhow!(message.to_string())),
                };
            }
            notified.await;
        }
    }
}

/// Concrete local ordinary-process handle.
pub struct LocalSubprocessHandle {
    pid: ProcessId,
    stdin: Option<SubprocessInput>,
    stdout: Option<SubprocessOutput>,
    stderr: Option<SubprocessOutput>,
    collected: SubprocessCollectedOutputs,
    done: Arc<DoneSlot>,
    direct_exited: Arc<AtomicBool>,
    outcome_settled: Arc<AtomicBool>,
    tree_exit_observed: Arc<AtomicBool>,
    termination_started: AtomicBool,
    grace: Duration,
    operations: TreeOperations,
}

impl std::fmt::Debug for LocalSubprocessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalSubprocessHandle")
            .field("pid", &self.pid)
            .field("direct_exited", &self.direct_exited.load(Ordering::Acquire))
            .field(
                "tree_exit_observed",
                &self.tree_exit_observed.load(Ordering::Acquire),
            )
            .field(
                "termination_started",
                &self.termination_started.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl LocalSubprocessHandle {
    fn failed(
        message: String,
        grace: Duration,
        operations: TreeOperations,
        collected: SubprocessCollectedOutputs,
    ) -> Arc<Self> {
        let done = Arc::new(DoneSlot::default());
        done.complete(StoredDone::Failure(Arc::from(message)));
        Arc::new(Self {
            pid: ProcessId::new(-1),
            stdin: None,
            stdout: None,
            stderr: None,
            collected,
            done,
            direct_exited: Arc::new(AtomicBool::new(true)),
            outcome_settled: Arc::new(AtomicBool::new(true)),
            tree_exit_observed: Arc::new(AtomicBool::new(true)),
            termination_started: AtomicBool::new(false),
            grace,
            operations,
        })
    }

    /// Force-terminates the owned tree synchronously without starting timers or waits.
    pub fn terminate_for_host_exit(&self) {
        if self.tree_alive() {
            self.signal_tree(TreeSignal::Kill);
        }
    }

    fn signal_tree(&self, signal: TreeSignal) {
        self.operations.signal(self.pid, signal);
    }

    fn tree_alive(&self) -> bool {
        if self.pid.as_i64() <= 0 || self.tree_exit_observed.load(Ordering::Acquire) {
            return false;
        }
        let alive = self.operations.tree_alive(
            self.pid,
            self.direct_exited.load(Ordering::Acquire),
            self.outcome_settled.load(Ordering::Acquire),
        );
        if !alive {
            self.tree_exit_observed.store(true, Ordering::Release);
        }
        alive
    }
}

#[async_trait::async_trait]
impl SubprocessHandle for LocalSubprocessHandle {
    fn pid(&self) -> ProcessId {
        self.pid
    }

    fn stdin(&self) -> Option<SubprocessInput> {
        self.stdin.clone()
    }

    fn stdout(&self) -> Option<SubprocessOutput> {
        self.stdout.clone()
    }

    fn stderr(&self) -> Option<SubprocessOutput> {
        self.stderr.clone()
    }

    fn collected(&self) -> SubprocessCollectedOutputs {
        self.collected.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        self.done.wait().await
    }

    fn terminate(&self) {
        if !self.tree_alive() || self.termination_started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.signal_tree(TreeSignal::Term);
        let pid = self.pid;
        let grace = self.grace;
        let tree_exit_observed = self.tree_exit_observed.clone();
        let direct_exited = self.direct_exited.clone();
        let outcome_settled = self.outcome_settled.clone();
        let operations = self.operations.clone();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + grace;
            loop {
                if !operations.tree_alive(
                    pid,
                    direct_exited.load(Ordering::Acquire),
                    outcome_settled.load(Ordering::Acquire),
                ) {
                    tree_exit_observed.store(true, Ordering::Release);
                    return;
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            if !tree_exit_observed.load(Ordering::Acquire)
                && operations.tree_alive(
                    pid,
                    direct_exited.load(Ordering::Acquire),
                    outcome_settled.load(Ordering::Acquire),
                )
            {
                operations.signal(pid, TreeSignal::Kill);
            }
        });
    }

    async fn wait_for_exit(&self, signal: Option<AbortSignal>) -> anyhow::Result<bool> {
        loop {
            if !self.tree_alive() {
                return Ok(true);
            }
            if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                return Ok(false);
            }
            if let Some(signal) = signal.as_ref() {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(15)) => {}
                    () = signal.cancelled() => return Ok(false),
                }
            } else {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        }
    }
}

/// Builds the scrubbed child environment and applies explicit overrides last.
#[must_use]
pub fn child_env(extra: Option<&SubprocessEnvironment>) -> BTreeMap<OsString, OsString> {
    let mut environment = scrubbed_parent_env();
    for (key, value) in extra.into_iter().flatten() {
        #[cfg(windows)]
        {
            let normalized = key.to_ascii_uppercase();
            environment
                .retain(|ambient, _| ambient.to_string_lossy().to_ascii_uppercase() != normalized);
        }
        match value {
            Some(value) => {
                environment.insert(OsString::from(key), OsString::from(value));
            }
            None => {
                environment.remove(&OsString::from(key));
            }
        }
    }
    environment
}

/// Spawns one detached process tree after synchronous source-compatible validation.
///
/// # Errors
///
/// Rejects invalid grace, pre-cancellation, argv, or collection limits before spawn.
pub fn spawn_subprocess(
    spec: SubprocessSpawnSpec,
    spill_dir: Option<&Path>,
) -> anyhow::Result<Arc<LocalSubprocessHandle>> {
    spawn_subprocess_with(
        spec,
        SpawnInternals {
            spill_dir: spill_dir.map(Path::to_path_buf),
            ..SpawnInternals::default()
        },
    )
}

/// Spawns one process tree through an injectable source-compatible host boundary.
///
/// # Errors
///
/// Rejects invalid grace, pre-cancellation, argv, collection limits, or spill setup.
pub fn spawn_subprocess_with(
    spec: SubprocessSpawnSpec,
    internals: SpawnInternals,
) -> anyhow::Result<Arc<LocalSubprocessHandle>> {
    let grace = validate_grace(spec.grace_ms)?;
    if let Some(signal) = spec.signal.as_ref()
        && signal.is_aborted()
    {
        anyhow::bail!("aborted before spawn: {}", abort_reason(signal));
    }
    let Some(_) = spec.argv.first().filter(|program| !program.is_empty()) else {
        anyhow::bail!("invalid argv: expected a non-empty program name at argv[0]");
    };

    let operations = TreeOperations::from_internals(&internals);
    let spill_dir = match internals.spill_dir {
        Some(path) => path,
        None => default_spill_dir()?,
    };
    let stdout_collector = collector_for(&spec.stdio.stdout, "stdout", &spill_dir)?;
    let stderr_collector = collector_for(&spec.stdio.stderr, "stderr", &spill_dir)?;

    let (mut child, stdin, stdout, stderr) = match spawn_native(&spec, &operations.platform) {
        Ok(spawned) => spawned,
        Err(error) => {
            return Ok(LocalSubprocessHandle::failed(
                error.to_string(),
                grace,
                operations,
                collected_outputs(stdout_collector.as_ref(), stderr_collector.as_ref()),
            ));
        }
    };
    let pid = ProcessId::new(child.id().map_or(-1, i64::from));

    let exposed_stdin = match (&spec.stdio.stdin, stdin) {
        (SubprocessStdinMode::Pipe, Some(stream)) => Some(SubprocessInput::new(stream)),
        (SubprocessStdinMode::Data(data), Some(mut stream)) => {
            let data = data.clone();
            tokio::spawn(async move {
                let _ = stream.write_all(data.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
            None
        }
        _ => None,
    };

    let (exposed_stdout, stdout_reader) =
        configure_output(&spec.stdio.stdout, stdout, stdout_collector.clone());
    let (exposed_stderr, stderr_reader) =
        configure_output(&spec.stdio.stderr, stderr, stderr_collector.clone());

    let done = Arc::new(DoneSlot::default());
    let direct_exited = Arc::new(AtomicBool::new(false));
    let outcome_settled = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(LocalSubprocessHandle {
        pid,
        stdin: exposed_stdin,
        stdout: exposed_stdout,
        stderr: exposed_stderr,
        collected: collected_outputs(stdout_collector.as_ref(), stderr_collector.as_ref()),
        done: done.clone(),
        direct_exited: direct_exited.clone(),
        outcome_settled: outcome_settled.clone(),
        tree_exit_observed: Arc::new(AtomicBool::new(false)),
        termination_started: AtomicBool::new(false),
        grace,
        operations,
    });

    let output_collectors = [stdout_collector, stderr_collector];
    tokio::spawn(async move {
        let outcome = child.wait().await;
        direct_exited.store(true, Ordering::Release);
        let output_failure = drain_collectors([stdout_reader, stderr_reader], grace).await;
        for collector in output_collectors.into_iter().flatten() {
            collector.seal();
        }
        outcome_settled.store(true, Ordering::Release);
        match (outcome, output_failure) {
            (_, Some(error)) => done.complete(StoredDone::Failure(error)),
            (Ok(status), None) => done.complete(StoredDone::Success(status_outcome(status))),
            (Err(error), None) => done.complete(StoredDone::Failure(Arc::from(error.to_string()))),
        }
    });

    if let Some(signal) = spec.signal {
        let weak = Arc::downgrade(&handle);
        let settled = handle.done.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = signal.cancelled() => {
                    if let Some(handle) = weak.upgrade() {
                        handle.terminate();
                    }
                }
                _ = settled.wait() => {}
            }
        });
    }
    Ok(handle)
}

fn collected_outputs(
    stdout: Option<&Arc<OutputCollector>>,
    stderr: Option<&Arc<OutputCollector>>,
) -> SubprocessCollectedOutputs {
    let erase = |collector: &Arc<OutputCollector>| {
        collector.clone() as Arc<dyn seekdeep_subprocess::SubprocessOutputReader>
    };
    SubprocessCollectedOutputs {
        stdout: stdout.map(erase),
        stderr: stderr.map(erase),
    }
}

pub(crate) fn validate_grace(value: f64) -> anyhow::Result<Duration> {
    anyhow::ensure!(
        value.is_finite() && value > 0.0 && value <= MAX_TIMER_DELAY_MS,
        "subprocess graceMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MILLIS}"
    );
    Ok(Duration::from_secs_f64(value / 1000.0))
}

fn build_command(spec: &SubprocessSpawnSpec, platform: &SpawnPlatform) -> Command {
    let mut command = Command::new(&spec.argv[0]);
    command.args(&spec.argv[1..]);
    command.current_dir(&spec.cwd);
    command.env_clear();
    command.envs(child_env(spec.env.as_ref()));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        if !platform.is_windows() {
            command.as_std_mut().process_group(0);
        }
    }
    #[cfg(not(unix))]
    let _ = platform;
    command
}

#[cfg(unix)]
fn spawn_native(
    spec: &SubprocessSpawnSpec,
    platform: &SpawnPlatform,
) -> std::io::Result<NativeSpawn> {
    let mut command = build_command(spec, platform);
    let stdin_parent = match spec.stdio.stdin {
        SubprocessStdinMode::Ignore => {
            command.stdin(Stdio::null());
            None
        }
        SubprocessStdinMode::Pipe | SubprocessStdinMode::Data(_) => {
            let (parent, child) = unix_socket_pair()?;
            command.stdin(unix_stream_stdio(child));
            Some(parent)
        }
    };
    let stdout = prepare_unix_output(&mut command, &spec.stdio.stdout, true)?;
    let stderr = prepare_unix_output(&mut command, &spec.stdio.stderr, false)?;
    let child = command.spawn()?;
    Ok((
        child,
        stdin_parent.map(|stream| Box::pin(stream) as ChildInput),
        stdout,
        stderr,
    ))
}

#[cfg(unix)]
fn unix_socket_pair() -> std::io::Result<(tokio::net::UnixStream, std::os::unix::net::UnixStream)> {
    let (parent, child) = std::os::unix::net::UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(parent).map(|parent| (parent, child))
}

#[cfg(unix)]
fn unix_stream_stdio(stream: std::os::unix::net::UnixStream) -> Stdio {
    let descriptor: std::os::fd::OwnedFd = stream.into();
    Stdio::from(descriptor)
}

#[cfg(unix)]
fn prepare_unix_output(
    command: &mut Command,
    mode: &SubprocessOutputMode,
    stdout: bool,
) -> std::io::Result<Option<ChildOutput>> {
    let stdio = if matches!(mode, SubprocessOutputMode::Inherit) {
        None
    } else {
        let (parent, child) = unix_socket_pair()?;
        if stdout {
            command.stdout(unix_stream_stdio(child));
        } else {
            command.stderr(unix_stream_stdio(child));
        }
        Some(Box::pin(parent) as ChildOutput)
    };
    if stdio.is_none() {
        if stdout {
            command.stdout(Stdio::inherit());
        } else {
            command.stderr(Stdio::inherit());
        }
    }
    Ok(stdio)
}

#[cfg(not(unix))]
fn spawn_native(
    spec: &SubprocessSpawnSpec,
    platform: &SpawnPlatform,
) -> std::io::Result<NativeSpawn> {
    let mut command = build_command(spec, platform);
    command.stdin(match spec.stdio.stdin {
        SubprocessStdinMode::Ignore => Stdio::null(),
        SubprocessStdinMode::Pipe | SubprocessStdinMode::Data(_) => Stdio::piped(),
    });
    command.stdout(output_stdio(&spec.stdio.stdout));
    command.stderr(output_stdio(&spec.stdio.stderr));
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .map(|stream| Box::pin(stream) as ChildInput);
    let stdout = child
        .stdout
        .take()
        .map(|stream| Box::pin(stream) as ChildOutput);
    let stderr = child
        .stderr
        .take()
        .map(|stream| Box::pin(stream) as ChildOutput);
    Ok((child, stdin, stdout, stderr))
}

fn checked_byte_limit(value: f64, field: &str) -> anyhow::Result<usize> {
    anyhow::ensure!(
        value.is_finite() && value >= 0.0 && value.fract() == 0.0,
        "{field} must be a non-negative integer"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

fn collector_for(
    mode: &SubprocessOutputMode,
    label: &str,
    spill_dir: &Path,
) -> anyhow::Result<Option<Arc<OutputCollector>>> {
    let SubprocessOutputMode::Collect(config) = mode else {
        return Ok(None);
    };
    let max_bytes = checked_byte_limit(config.max_bytes, "maxBytes")?;
    let max_spill_bytes = config
        .spill
        .as_ref()
        .map(|spill| checked_byte_limit(spill.max_bytes, "spill.maxBytes"))
        .transpose()?
        .map(|value| u64::try_from(value).unwrap_or(u64::MAX));
    Ok(Some(Arc::new(OutputCollector::new(
        max_bytes,
        max_spill_bytes,
        label,
        spill_dir,
    ))))
}

#[cfg(not(unix))]
fn output_stdio(mode: &SubprocessOutputMode) -> Stdio {
    match mode {
        SubprocessOutputMode::Inherit => Stdio::inherit(),
        SubprocessOutputMode::Pipe | SubprocessOutputMode::Collect(_) => Stdio::piped(),
    }
}

fn configure_output(
    mode: &SubprocessOutputMode,
    stream: Option<ChildOutput>,
    collector: Option<Arc<OutputCollector>>,
) -> (
    Option<SubprocessOutput>,
    Option<JoinHandle<std::io::Result<()>>>,
) {
    match mode {
        SubprocessOutputMode::Pipe => {
            let output = stream.map(|stream| Arc::new(tokio::sync::Mutex::new(stream)));
            (output, None)
        }
        SubprocessOutputMode::Inherit => (None, None),
        SubprocessOutputMode::Collect(_) => {
            let reader = stream.zip(collector).map(|(mut stream, collector)| {
                tokio::spawn(async move {
                    let mut buffer = vec![0_u8; 16 * 1024];
                    loop {
                        match stream.read(&mut buffer).await {
                            Ok(0) => break,
                            Err(error) => return Err(error),
                            Ok(read) => {
                                collector.push(&buffer[..read])?;
                            }
                        }
                    }
                    Ok(())
                })
            });
            (None, reader)
        }
    }
}

async fn drain_collectors(
    readers: [Option<JoinHandle<std::io::Result<()>>>; 2],
    grace: Duration,
) -> Option<Arc<str>> {
    let mut readers = readers.into_iter().flatten().collect::<Vec<_>>();
    let mut failure = None;
    let drained = tokio::time::timeout(grace, async {
        for reader in &mut readers {
            match reader.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failure.get_or_insert_with(|| Arc::from(error.to_string()));
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    failure.get_or_insert_with(|| Arc::from(error.to_string()));
                }
            }
        }
    })
    .await;
    if drained.is_err() {
        for reader in &readers {
            reader.abort();
        }
        for reader in readers {
            let _ = reader.await;
        }
    }
    failure
}

fn abort_reason(signal: &AbortSignal) -> String {
    match signal.reason() {
        Some(serde_json::Value::String(reason)) => reason,
        Some(serde_json::Value::Null) | None => "aborted".to_owned(),
        Some(reason) => reason.to_string(),
    }
}

fn status_outcome(status: std::process::ExitStatus) -> SubprocessOutcome {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        let signal = status.signal().map(|number| {
            let name = nix::sys::signal::Signal::try_from(number)
                .map_or_else(|_| format!("SIG{number}"), |signal| format!("{signal:?}"));
            ProcessSignal::new(name)
        });
        SubprocessOutcome {
            exit_code: status.code(),
            signal,
        }
    }
    #[cfg(not(unix))]
    {
        SubprocessOutcome {
            exit_code: status.code(),
            signal: None,
        }
    }
}

#[derive(Clone, Copy)]
enum TreeSignal {
    Term,
    Kill,
}

/// Terminates one Windows process tree with `taskkill /T /F`.
///
/// Delivery races and command failures are deliberately contained.
pub fn taskkill_process_tree(pid: ProcessId) {
    if pid.as_i64() <= 0 {
        return;
    }
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.as_i64().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Sends one supported signal to a detached POSIX process group.
///
/// Non-positive ids, unsupported signal spellings, absent groups, permission
/// failures, and non-POSIX hosts are all contained like the source helper.
pub fn kill_group(pid: ProcessId, signal: &ProcessSignal) {
    #[cfg(unix)]
    {
        use nix::{sys::signal::kill, unistd::Pid};
        let Ok(pid) = i32::try_from(pid.as_i64()) else {
            return;
        };
        if pid <= 0 {
            return;
        }
        let signal = match signal.as_str() {
            "SIGHUP" => nix::sys::signal::Signal::SIGHUP,
            "SIGINT" => nix::sys::signal::Signal::SIGINT,
            "SIGKILL" => nix::sys::signal::Signal::SIGKILL,
            "SIGTERM" => nix::sys::signal::Signal::SIGTERM,
            "SIGTSTP" => nix::sys::signal::Signal::SIGTSTP,
            _ => return,
        };
        let _ = kill(Pid::from_raw(-pid), Some(signal));
    }
    #[cfg(not(unix))]
    let _ = (pid, signal);
}

#[cfg(unix)]
fn kill_group_with_fallback(pid: ProcessId, signal: TreeSignal) {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let Ok(pid) = i32::try_from(pid.as_i64()) else {
        return;
    };
    if pid <= 0 {
        return;
    }
    let signal = match signal {
        TreeSignal::Term => Signal::SIGTERM,
        TreeSignal::Kill => Signal::SIGKILL,
    };
    if kill(Pid::from_raw(-pid), Some(signal)).is_err() {
        let _ = kill(Pid::from_raw(pid), Some(signal));
    }
}

#[cfg(not(unix))]
fn kill_group_with_fallback(_pid: ProcessId, _signal: TreeSignal) {}

#[cfg(unix)]
fn posix_tree_alive(
    pid: ProcessId,
    direct_exited: bool,
    outcome_settled: bool,
    linux: bool,
    linux_group_has_live_members: &LinuxGroupProbeFn,
) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    let Ok(pid) = i32::try_from(pid.as_i64()) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    match kill(Pid::from_raw(-pid), None) {
        Ok(()) | Err(Errno::EPERM) => {
            if outcome_settled && linux {
                linux_group_has_live_members(ProcessGroupId::new(i64::from(pid))).unwrap_or(true)
            } else {
                true
            }
        }
        Err(Errno::ESRCH) => false,
        Err(_) => !direct_exited,
    }
}

#[cfg(not(unix))]
fn posix_tree_alive(
    _pid: ProcessId,
    direct_exited: bool,
    _outcome_settled: bool,
    _linux: bool,
    _linux_group_has_live_members: &LinuxGroupProbeFn,
) -> bool {
    !direct_exited
}

fn default_linux_group_has_live_members(group: ProcessGroupId) -> Option<bool> {
    crate::process_inspector::linux_process_group_has_live_members(
        group,
        &crate::process_inspector::HostProcessInspectorInternals,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use seekdeep_subprocess::{SubprocessCollect, SubprocessSpill, SubprocessStdio};

    use super::*;

    fn spec(script: &str, temp: &Path) -> SubprocessSpawnSpec {
        SubprocessSpawnSpec {
            argv: vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: temp.to_path_buf(),
            stdio: SubprocessStdio {
                stdin: SubprocessStdinMode::Ignore,
                stdout: SubprocessOutputMode::Collect(SubprocessCollect {
                    max_bytes: 64_000.0,
                    spill: Some(SubprocessSpill {
                        max_bytes: 64.0 * 1024.0 * 1024.0,
                    }),
                }),
                stderr: SubprocessOutputMode::Collect(SubprocessCollect {
                    max_bytes: 64_000.0,
                    spill: Some(SubprocessSpill {
                        max_bytes: 64.0 * 1024.0 * 1024.0,
                    }),
                }),
            },
            grace_ms: 200.0,
            signal: None,
            env: None,
        }
    }

    #[tokio::test]
    async fn captures_streams_exit_codes_cwd_and_explicit_environment() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = spec(
            "printf '%s' \"$EXTRA\"; printf err >&2; exit 42",
            temp.path(),
        );
        request.env = Some(BTreeMap::from([(
            "EXTRA".to_owned(),
            Some("out".to_owned()),
        )]));
        let running = spawn_subprocess(request, Some(temp.path())).unwrap();
        assert_eq!(
            running.done().await.unwrap(),
            SubprocessOutcome {
                exit_code: Some(42),
                signal: None,
            }
        );
        assert_eq!(running.collected().stdout.unwrap().read_from(0).text, "out");
        assert_eq!(running.collected().stderr.unwrap().read_from(0).text, "err");
    }

    #[tokio::test]
    async fn batch_stdin_is_written_and_closed() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = spec("cat", temp.path());
        request.stdio.stdin = SubprocessStdinMode::Data("hello\n".to_owned());
        let running = spawn_subprocess(request, Some(temp.path())).unwrap();
        assert_eq!(running.done().await.unwrap().exit_code, Some(0));
        assert_eq!(
            running.collected().stdout.unwrap().read_from(0).text,
            "hello\n"
        );
    }

    #[tokio::test]
    async fn cancellation_terminates_the_detached_group() {
        let temp = tempfile::tempdir().unwrap();
        let signal = AbortSignal::default();
        let mut request = spec("exec sleep 60", temp.path());
        request.signal = Some(signal.clone());
        let running = spawn_subprocess(request, Some(temp.path())).unwrap();
        signal.abort_with_reason(serde_json::Value::String("deadline".to_owned()));
        let outcome = tokio::time::timeout(Duration::from_secs(3), running.done())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.signal.unwrap().as_str(), "SIGTERM");
    }

    #[tokio::test]
    async fn spawn_failure_is_a_pid_minus_one_handle_with_rejecting_done() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = spec("true", temp.path());
        request.cwd = temp.path().join("missing");
        let running = spawn_subprocess(request, Some(temp.path())).unwrap();
        assert_eq!(running.pid(), ProcessId::new(-1));
        assert!(running.done().await.is_err());
        assert!(running.wait_for_exit(None).await.unwrap());
    }

    #[test]
    fn rejects_invalid_grace_argv_and_pre_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = spec("true", temp.path());
        request.grace_ms = 0.0;
        assert!(spawn_subprocess(request, Some(temp.path())).is_err());

        let mut request = spec("true", temp.path());
        request.argv.clear();
        assert!(spawn_subprocess(request, Some(temp.path())).is_err());

        let signal = AbortSignal::default();
        signal.abort_with_reason(serde_json::Value::String("too late".to_owned()));
        let mut request = spec("true", temp.path());
        request.signal = Some(signal);
        assert_eq!(
            spawn_subprocess(request, Some(temp.path()))
                .unwrap_err()
                .to_string(),
            "aborted before spawn: too late"
        );
    }
}
