//! E2B PTY allocation and complete remote-session ownership.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, future::Shared};
use parking_lot::Mutex;
use seekdeep_e2b::{
    E2bCommandCompletion, E2bCommandExit, E2bCommandHandleRef, E2bFileNotFound,
    E2bPtyCreateOptions, E2bSandbox, E2bSandboxNotFound, E2bService, e2b_control_envs,
    quote_e2b_shell_arg,
};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    ProcessGroupId, ProcessId, ProcessSignal, SubprocessOutcome, SubprocessOutput,
    SubprocessTerminalForeground, SubprocessTerminalHandle, SubprocessTerminalSignal,
    SubprocessTerminalSpawnSpec,
};
use tokio::io::AsyncWriteExt as _;

use crate::{
    environment::{bootstrap_environment, read_remote_environment, serialize_remote_environment},
    remote::{command_environment, delay, signal_remote_groups},
};

const TERMINAL_RUNNER_SOURCE: &str = concat!(
    "#!/bin/bash\n",
    "set -euo pipefail\n",
    "seekdeep_state=$1\n",
    "mapfile -d '' -t seekdeep_env < \"$seekdeep_state/environment\"\n",
    "mapfile -d '' -t seekdeep_argv < \"$seekdeep_state/argv\"\n",
    "seekdeep_output_marker=$(<\"$seekdeep_state/output-marker\")\n",
    "rm -f -- \"$seekdeep_state/environment\" \"$seekdeep_state/argv\" \"$seekdeep_state/output-marker\" \"$seekdeep_state/runner.bash\"\n",
    "if (( ${#seekdeep_argv[@]} == 0 )); then\n",
    "  printf 'terminal runner received empty argv\\n' >&2\n",
    "  exit 125\n",
    "fi\n",
    "printf '%s' \"$seekdeep_output_marker\"\n",
    "exec env -i -- \"${seekdeep_env[@]}\" \"${seekdeep_argv[@]}\"\n",
);

static NEXT_OUTPUT_MARKER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct TerminalPaths {
    runner: String,
    environment: String,
    argv: String,
    output_marker: String,
}

#[derive(Clone, Debug)]
enum TerminalSettlement {
    Result(E2bCommandCompletion),
    Exit(E2bCommandExit),
    Error(Arc<str>),
}

#[derive(Clone, Debug)]
enum TerminalDone {
    Pending,
    Settled(Result<SubprocessOutcome, Arc<str>>),
}

type SharedTerminalCompletion = Shared<futures::future::BoxFuture<'static, TerminalSettlement>>;
type SharedCleanup = Shared<futures::future::BoxFuture<'static, Result<(), Arc<str>>>>;

#[derive(Debug)]
struct BootstrapOutputFilter {
    marker: Vec<u8>,
    state: Mutex<BootstrapFilterState>,
    ready: AtomicBool,
    ready_notify: tokio::sync::Notify,
    sender: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
}

#[derive(Debug, Default)]
struct BootstrapFilterState {
    pending: Vec<u8>,
    published: bool,
}

impl BootstrapOutputFilter {
    fn new(marker: Vec<u8>) -> (Arc<Self>, SubprocessOutput) {
        let (reader, mut writer) = tokio::io::duplex(64 * 1024);
        let output = SubprocessOutput::new(Box::pin(reader));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = receiver.recv().await {
                if writer.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        });
        (
            Arc::new(Self {
                marker,
                state: Mutex::new(BootstrapFilterState::default()),
                ready: AtomicBool::new(false),
                ready_notify: tokio::sync::Notify::new(),
                sender: Mutex::new(Some(sender)),
            }),
            output,
        )
    }

    fn push(&self, data: &[u8]) {
        let mut state = self.state.lock();
        if state.published {
            self.send(data);
            return;
        }
        state.pending.extend_from_slice(data);
        let Some(offset) = find_bytes(&state.pending, &self.marker) else {
            let retained = state.pending.len().min(self.marker.len().saturating_sub(1));
            let drop = state.pending.len().saturating_sub(retained);
            state.pending.drain(..drop);
            return;
        };
        let after = state.pending.split_off(offset + self.marker.len());
        state.pending.clear();
        state.published = true;
        self.ready.store(true, Ordering::Release);
        self.ready_notify.notify_waiters();
        drop(state);
        self.send(&after);
    }

    fn send(&self, data: &[u8]) {
        if !data.is_empty()
            && let Some(sender) = self.sender.lock().as_ref()
        {
            let _ = sender.send(data.to_vec());
        }
    }

    fn close(&self) {
        self.sender.lock().take();
    }

    async fn wait_ready(
        &self,
        completion: SharedTerminalCompletion,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()> {
        loop {
            if self.ready.load(Ordering::Acquire) {
                return Ok(());
            }
            let notified = self.ready_notify.notified();
            if let Some(signal) = signal {
                tokio::select! {
                    () = notified => {}
                    _ = completion.clone() => anyhow::bail!("subprocess-e2b: terminal exited before publishing its output boundary"),
                    () = signal.cancelled() => anyhow::bail!("subprocess-e2b: terminal allocation aborted"),
                }
            } else {
                tokio::select! {
                    () = notified => {}
                    _ = completion.clone() => anyhow::bail!("subprocess-e2b: terminal exited before publishing its output boundary"),
                }
            }
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

struct ActivityGuard<'a> {
    core: &'a TerminalCore,
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        if self.core.operations.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.core.operations_idle.notify_waiters();
        }
    }
}

/// One E2B PTY and every live group in its remote POSIX session.
pub struct E2bTerminalHandle {
    core: Arc<TerminalCore>,
    output: SubprocessOutput,
    done: tokio::sync::watch::Receiver<TerminalDone>,
}

impl std::fmt::Debug for E2bTerminalHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bTerminalHandle")
            .field("pid", &self.core.handle.pid())
            .field("session_id", &self.core.session_id)
            .finish_non_exhaustive()
    }
}

struct TerminalCore {
    sandbox: Arc<dyn E2bSandbox>,
    handle: E2bCommandHandleRef,
    completion: SharedTerminalCompletion,
    filter: Arc<BootstrapOutputFilter>,
    session_id: i64,
    control_env: BTreeMap<String, String>,
    state_dir: String,
    grace_ms: f64,
    poll_ms: u64,
    top_level_exited: AtomicBool,
    operation_abort: AbortSignal,
    operations: AtomicUsize,
    operations_idle: tokio::sync::Notify,
    cleanup: Mutex<Option<SharedCleanup>>,
    termination_signal: Mutex<Option<ProcessSignal>>,
}

impl TerminalCore {
    fn begin_operation(&self) -> anyhow::Result<ActivityGuard<'_>> {
        anyhow::ensure!(
            !self.operation_abort.is_aborted(),
            "subprocess-e2b: terminal is terminating"
        );
        self.operations.fetch_add(1, Ordering::AcqRel);
        if self.operation_abort.is_aborted() {
            self.operations.fetch_sub(1, Ordering::AcqRel);
            anyhow::bail!("subprocess-e2b: terminal is terminating");
        }
        Ok(ActivityGuard { core: self })
    }

    async fn wait_operations(&self) {
        loop {
            let idle = self.operations_idle.notified();
            if self.operations.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }

    async fn inspect_foreground_once(
        &self,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        let result = self
            .sandbox
            .commands()
            .run(
                &format!("ps -o tpgid= -p {}", self.handle.pid()),
                command_environment(&self.control_env),
                signal,
            )
            .await;
        match result {
            Ok(result) => Ok(Some(SubprocessTerminalForeground {
                process_group_id: ProcessGroupId::new(parse_positive_id(
                    &result.stdout,
                    &format!(
                        "subprocess-e2b: cannot resolve foreground process group for terminal {}",
                        self.handle.pid()
                    ),
                )?),
                input_waiting: false,
            })),
            Err(error)
                if error
                    .downcast_ref::<E2bCommandExit>()
                    .is_some_and(|error| error.status == 1)
                    || self.top_level_exited.load(Ordering::Acquire) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn cleanup(self: &Arc<Self>) -> SharedCleanup {
        if let Some(cleanup) = self.cleanup.lock().clone() {
            return cleanup;
        }
        self.operation_abort.abort();
        let core = self.clone();
        let cleanup = async move {
            core.wait_operations().await;
            core.close_once()
                .await
                .map_err(|error| Arc::<str>::from(format!("{error:#}")))
        }
        .boxed()
        .shared();
        *self.cleanup.lock() = Some(cleanup.clone());
        cleanup
    }

    async fn close_once(&self) -> anyhow::Result<()> {
        let mut groups =
            session_process_groups(self.sandbox.as_ref(), self.session_id, &self.control_env)
                .await?;
        if !groups.is_empty() {
            *self.termination_signal.lock() = Some(ProcessSignal::new("SIGTERM"));
            signal_remote_groups(
                self.sandbox.commands().as_ref(),
                &self.control_env,
                &groups,
                "TERM",
            )
            .await?;
            groups = await_session_empty(
                self.sandbox.as_ref(),
                self.session_id,
                &self.control_env,
                self.grace_ms,
                self.poll_ms,
                false,
            )
            .await?;
        }
        if groups.is_empty()
            && !self.top_level_exited.load(Ordering::Acquire)
            && tokio::time::timeout(grace_duration(self.grace_ms), self.completion.clone())
                .await
                .is_ok()
        {
            self.top_level_exited.store(true, Ordering::Release);
        }
        if !groups.is_empty() || !self.top_level_exited.load(Ordering::Acquire) {
            *self.termination_signal.lock() = Some(ProcessSignal::new("SIGKILL"));
            if !self.top_level_exited.load(Ordering::Acquire) {
                match self.handle.kill().await {
                    Ok(_) => {}
                    Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => {
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
            groups = await_session_empty(
                self.sandbox.as_ref(),
                self.session_id,
                &self.control_env,
                self.grace_ms,
                self.poll_ms,
                true,
            )
            .await?;
            if !self.top_level_exited.load(Ordering::Acquire)
                && tokio::time::timeout(grace_duration(self.grace_ms), self.completion.clone())
                    .await
                    .is_ok()
            {
                self.top_level_exited.store(true, Ordering::Release);
            }
        }
        anyhow::ensure!(
            groups.is_empty(),
            "subprocess-e2b: terminal cleanup failed; surviving process groups: {}",
            groups
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        anyhow::ensure!(
            self.top_level_exited.load(Ordering::Acquire),
            "subprocess-e2b: terminal cleanup failed; surviving pid: {}",
            self.handle.pid()
        );
        match self.handle.disconnect().await {
            Ok(()) => {}
            Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => {}
            Err(error) => return Err(error),
        }
        let _ = self.sandbox.files().remove(&self.state_dir).await;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SubprocessTerminalHandle for E2bTerminalHandle {
    fn pid(&self) -> ProcessId {
        ProcessId::new(self.core.handle.pid())
    }

    fn output(&self) -> SubprocessOutput {
        self.output.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        wait_terminal_done(self.done.clone()).await
    }

    async fn write(&self, data: &str) -> anyhow::Result<()> {
        let _operation = self.core.begin_operation()?;
        anyhow::ensure!(
            !self.core.top_level_exited.load(Ordering::Acquire),
            "terminal process has exited"
        );
        let pty = self
            .core
            .sandbox
            .pty()
            .ok_or_else(|| anyhow::anyhow!("E2B SDK binding does not expose PTY operations"))?;
        pty.send_input(
            self.core.handle.pid(),
            data.as_bytes().to_vec(),
            Some(&self.core.operation_abort),
        )
        .await
    }

    async fn inspect_foreground(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        let _operation = self.core.begin_operation()?;
        self.core
            .inspect_foreground_once(Some(&self.core.operation_abort))
            .await
    }

    async fn signal_foreground(
        &self,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<ProcessGroupId> {
        let _operation = self.core.begin_operation()?;
        let foreground = self
            .core
            .inspect_foreground_once(Some(&self.core.operation_abort))
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "subprocess-e2b: cannot resolve foreground process group for terminal {}",
                    self.core.handle.pid()
                )
            })?;
        anyhow::ensure!(
            signal != SubprocessTerminalSignal::Sigkill
                || foreground.process_group_id.as_i64() != self.core.handle.pid(),
            "refusing to SIGKILL the terminal shell; terminate the terminal session instead"
        );
        let name = signal.as_str().trim_start_matches("SIG");
        self.core
            .sandbox
            .commands()
            .run(
                &format!("kill -{name} -- -{}", foreground.process_group_id.as_i64()),
                command_environment(&self.core.control_env),
                Some(&self.core.operation_abort),
            )
            .await?;
        Ok(foreground.process_group_id)
    }

    async fn terminate(&self) -> anyhow::Result<()> {
        let cleanup = self.core.cleanup();
        let result = cleanup
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()));
        if result.is_err() {
            self.core.cleanup.lock().take();
        }
        result
    }
}

/// Allocates an E2B PTY and publishes it only after the requested process output boundary.
///
/// # Errors
///
/// Returns validation, allocation, readiness, session lookup, or rollback failures.
#[allow(
    clippy::too_many_lines,
    reason = "the allocation transaction must keep private-state ownership and unpublished-handle rollback together"
)]
pub async fn spawn_e2b_terminal(
    runtime: Arc<E2bService>,
    spec: SubprocessTerminalSpawnSpec,
    state_dir: impl Into<String>,
    poll_ms: u64,
) -> anyhow::Result<Arc<E2bTerminalHandle>> {
    let state_dir = state_dir.into();
    let sandbox = runtime.get_sandbox().await?;
    ensure_not_aborted(spec.signal.as_ref(), "terminal allocation aborted")?;
    let paths = TerminalPaths {
        runner: format!("{state_dir}/runner.bash"),
        environment: format!("{state_dir}/environment"),
        argv: format!("{state_dir}/argv"),
        output_marker: format!("{state_dir}/output-marker"),
    };
    let marker = format!(
        "seekdeep-e2b-bootstrap:{}",
        NEXT_OUTPUT_MARKER.fetch_add(1, Ordering::Relaxed)
    )
    .into_bytes();
    let (filter, output) = BootstrapOutputFilter::new(marker.clone());
    let mut command = None;
    let mut completion = None;
    let mut state_created = false;
    let mut control_env = BTreeMap::new();
    let result = async {
        let pty = sandbox
            .pty()
            .ok_or_else(|| anyhow::anyhow!("E2B SDK binding does not expose PTY operations"))?;
        let ambient =
            read_remote_environment(sandbox.commands().as_ref(), spec.signal.as_ref()).await?;
        control_env = bootstrap_environment(&ambient);
        let environment = serialize_terminal_environment(&ambient, spec.env.as_ref())?;
        let argv = serialize_values(&spec.argv, "argv")?;
        state_created = true;
        sandbox
            .files()
            .make_dir(&state_dir, spec.signal.as_ref())
            .await?;
        sandbox
            .commands()
            .run(
                &format!("chmod 700 -- {}", quote_e2b_shell_arg(&state_dir)),
                command_environment(&control_env),
                spec.signal.as_ref(),
            )
            .await?;
        for (path, content) in [
            (paths.runner.as_str(), TERMINAL_RUNNER_SOURCE.to_owned()),
            (paths.environment.as_str(), environment),
            (paths.argv.as_str(), argv),
            (
                paths.output_marker.as_str(),
                String::from_utf8(marker.clone())?,
            ),
        ] {
            sandbox
                .files()
                .write(path, &content, BTreeMap::new(), spec.signal.as_ref())
                .await?;
        }
        let quoted = [
            &paths.runner,
            &paths.environment,
            &paths.argv,
            &paths.output_marker,
        ]
        .map(|path| quote_e2b_shell_arg(path))
        .join(" ");
        sandbox
            .commands()
            .run(
                &format!("chmod 600 -- {quoted}"),
                command_environment(&control_env),
                spec.signal.as_ref(),
            )
            .await?;
        let callback_filter = filter.clone();
        let handle = pty
            .create(E2bPtyCreateOptions {
                rows: spec.rows,
                cols: spec.cols,
                cwd: spec
                    .cwd
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("subprocess-e2b: terminal cwd must be UTF-8"))?
                    .to_owned(),
                env: e2b_control_envs(&control_env),
                timeout_ms: 0.0,
                on_data: Arc::new(move |data| callback_filter.push(&data)),
            })
            .await?;
        let settled = terminal_completion(handle.clone());
        command = Some(handle.clone());
        completion = Some(settled.clone());
        ensure_not_aborted(spec.signal.as_ref(), "terminal allocation aborted")?;
        anyhow::ensure!(
            handle.pid() > 0,
            "subprocess-e2b: E2B returned invalid terminal pid {}",
            handle.pid()
        );
        pty.send_input(
            handle.pid(),
            format!(
                "exec /bin/bash {} {}\r",
                quote_e2b_shell_arg(&paths.runner),
                quote_e2b_shell_arg(&state_dir)
            )
            .into_bytes(),
            spec.signal.as_ref(),
        )
        .await?;
        filter
            .wait_ready(settled.clone(), spec.signal.as_ref())
            .await?;
        let session_id = terminal_session_id(
            sandbox.as_ref(),
            handle.pid(),
            &control_env,
            spec.signal.as_ref(),
        )
        .await?;
        let (done_sender, done) = tokio::sync::watch::channel(TerminalDone::Pending);
        let core = Arc::new(TerminalCore {
            sandbox: sandbox.clone(),
            handle,
            completion: settled,
            filter: filter.clone(),
            session_id,
            control_env: control_env.clone(),
            state_dir: state_dir.clone(),
            grace_ms: spec.grace_ms,
            poll_ms,
            top_level_exited: AtomicBool::new(false),
            operation_abort: AbortSignal::default(),
            operations: AtomicUsize::new(0),
            operations_idle: tokio::sync::Notify::new(),
            cleanup: Mutex::new(None),
            termination_signal: Mutex::new(None),
        });
        let owner = core.clone();
        tokio::spawn(async move {
            let outcome = terminal_outcome(&owner).await;
            owner.top_level_exited.store(true, Ordering::Release);
            owner.filter.close();
            done_sender.send_replace(TerminalDone::Settled(
                outcome.map_err(|error| Arc::<str>::from(format!("{error:#}"))),
            ));
        });
        Ok(Arc::new(E2bTerminalHandle { core, output, done }))
    }
    .await;
    match result {
        Ok(handle) => Ok(handle),
        Err(error) => {
            filter.close();
            let mut failures = Vec::new();
            if let Some(handle) = command {
                let rollback = if let Some(completion) = completion {
                    rollback_unpublished_terminal(
                        sandbox.as_ref(),
                        handle.as_ref(),
                        completion,
                        &control_env,
                        spec.grace_ms,
                        poll_ms,
                    )
                    .await
                } else {
                    handle.kill().await.map(|_| ())
                };
                if let Err(cleanup) = rollback
                    && cleanup.downcast_ref::<E2bSandboxNotFound>().is_none()
                {
                    failures.push(format!("{cleanup:#}"));
                }
            }
            if state_created
                && let Err(cleanup) = sandbox.files().remove(&state_dir).await
                && cleanup.downcast_ref::<E2bFileNotFound>().is_none()
                && cleanup.downcast_ref::<E2bSandboxNotFound>().is_none()
            {
                failures.push(format!("{cleanup:#}"));
            }
            if failures.is_empty() {
                Err(error)
            } else {
                anyhow::bail!(
                    "{error:#}\nsubprocess-e2b: terminal setup cleanup did not complete: {}",
                    failures.join("; ")
                )
            }
        }
    }
}

async fn terminal_outcome(core: &TerminalCore) -> anyhow::Result<SubprocessOutcome> {
    match core.completion.clone().await {
        TerminalSettlement::Result(result) => Ok(SubprocessOutcome {
            exit_code: Some(result.exit_code),
            signal: None,
        }),
        TerminalSettlement::Exit(error) => {
            let signal = core.termination_signal.lock().clone();
            Ok(if signal.is_some() {
                SubprocessOutcome {
                    exit_code: None,
                    signal,
                }
            } else {
                SubprocessOutcome {
                    exit_code: Some(error.status),
                    signal: None,
                }
            })
        }
        TerminalSettlement::Error(error) => anyhow::bail!(error.to_string()),
    }
}

async fn wait_terminal_done(
    mut receiver: tokio::sync::watch::Receiver<TerminalDone>,
) -> anyhow::Result<SubprocessOutcome> {
    loop {
        let state = receiver.borrow().clone();
        match state {
            TerminalDone::Pending => {}
            TerminalDone::Settled(Ok(outcome)) => return Ok(outcome),
            TerminalDone::Settled(Err(error)) => anyhow::bail!(error.to_string()),
        }
        anyhow::ensure!(
            receiver.changed().await.is_ok(),
            "subprocess-e2b: terminal completion ended without a result"
        );
    }
}

fn terminal_completion(handle: E2bCommandHandleRef) -> SharedTerminalCompletion {
    async move {
        match handle.wait().await {
            Ok(result) => TerminalSettlement::Result(result),
            Err(error) => error.downcast_ref::<E2bCommandExit>().map_or_else(
                || TerminalSettlement::Error(Arc::<str>::from(format!("{error:#}"))),
                |exit| TerminalSettlement::Exit(exit.clone()),
            ),
        }
    }
    .boxed()
    .shared()
}

fn parse_positive_id(value: &str, message: &str) -> anyhow::Result<i64> {
    let raw = value.trim();
    anyhow::ensure!(
        !raw.is_empty() && !raw.starts_with('0') && raw.bytes().all(|byte| byte.is_ascii_digit()),
        "{message}"
    );
    raw.parse::<i64>().map_err(Into::into)
}

fn serialize_values(values: &[String], kind: &str) -> anyhow::Result<String> {
    let mut output = String::new();
    for value in values {
        anyhow::ensure!(
            !value.contains('\0'),
            "subprocess-e2b: terminal {kind} must not contain NUL bytes"
        );
        write!(output, "{value}\0").expect("writing to a String is infallible");
    }
    Ok(output)
}

fn serialize_terminal_environment(
    ambient: &str,
    explicit: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<String> {
    let converted = explicit.map(|values| {
        values
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect()
    });
    serialize_remote_environment(ambient, converted.as_ref())
}

async fn terminal_session_id(
    sandbox: &dyn E2bSandbox,
    pid: i64,
    environment: &BTreeMap<String, String>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<i64> {
    let result = sandbox
        .commands()
        .run(
            &format!("ps -o sid= -p {pid}"),
            command_environment(environment),
            signal,
        )
        .await?;
    ensure_not_aborted(signal, "terminal allocation aborted")?;
    parse_positive_id(
        &result.stdout,
        &format!("subprocess-e2b: cannot resolve process session for terminal {pid}"),
    )
}

async fn session_process_groups(
    sandbox: &dyn E2bSandbox,
    session_id: i64,
    environment: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<i64>> {
    let result = sandbox
        .commands()
        .run(
            &format!(
                "set -o pipefail; ps -eo sid=,pgid=,stat= | awk '$1 == {session_id} && $3 !~ /^[ZXx]/ {{ print $2 }}'"
            ),
            command_environment(environment),
            None,
        )
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    let mut groups = BTreeSet::new();
    for raw in result.stdout.split_whitespace() {
        let group = parse_positive_id(
            raw,
            &format!(
                "subprocess-e2b: invalid process group {raw:?} in terminal session {session_id}"
            ),
        )?;
        anyhow::ensure!(
            group > 1,
            "subprocess-e2b: unsafe process group {group} in terminal session {session_id}"
        );
        groups.insert(group);
    }
    Ok(groups.into_iter().collect())
}

async fn await_session_empty(
    sandbox: &dyn E2bSandbox,
    session_id: i64,
    environment: &BTreeMap<String, String>,
    grace_ms: f64,
    poll_ms: u64,
    kill: bool,
) -> anyhow::Result<Vec<i64>> {
    let deadline = tokio::time::Instant::now() + grace_duration(grace_ms);
    loop {
        let groups = session_process_groups(sandbox, session_id, environment).await?;
        if groups.is_empty() {
            return Ok(groups);
        }
        if kill {
            signal_remote_groups(sandbox.commands().as_ref(), environment, &groups, "KILL").await?;
            if tokio::time::Instant::now() >= deadline {
                return session_process_groups(sandbox, session_id, environment).await;
            }
        } else if tokio::time::Instant::now() >= deadline {
            return Ok(groups);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        delay(
            poll_ms.min(
                u64::try_from(remaining.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1),
            ),
        )
        .await;
    }
}

async fn rollback_unpublished_terminal(
    sandbox: &dyn E2bSandbox,
    handle: &dyn seekdeep_e2b::E2bCommandHandle,
    completion: SharedTerminalCompletion,
    environment: &BTreeMap<String, String>,
    grace_ms: f64,
    poll_ms: u64,
) -> anyhow::Result<()> {
    let mut session_id = (handle.pid() > 1).then_some(handle.pid());
    if let Some(pid) = session_id {
        session_id = terminal_session_id(sandbox, pid, environment, None)
            .await
            .ok()
            .or(Some(pid));
        let mut groups = session_process_groups(sandbox, session_id.expect("session"), environment)
            .await
            .unwrap_or_default();
        if !groups.is_empty() {
            signal_remote_groups(sandbox.commands().as_ref(), environment, &groups, "TERM").await?;
            groups = await_session_empty(
                sandbox,
                session_id.expect("session"),
                environment,
                grace_ms,
                poll_ms,
                false,
            )
            .await?;
        }
        if !groups.is_empty() {
            let _ = await_session_empty(
                sandbox,
                session_id.expect("session"),
                environment,
                grace_ms,
                poll_ms,
                true,
            )
            .await?;
        }
    }
    if completion.clone().now_or_never().is_none() {
        handle.kill().await?;
        let _ = tokio::time::timeout(grace_duration(grace_ms), completion.clone()).await;
    }
    if let Some(session_id) = session_id {
        let survivors =
            await_session_empty(sandbox, session_id, environment, grace_ms, poll_ms, true).await?;
        anyhow::ensure!(
            survivors.is_empty(),
            "subprocess-e2b: terminal setup rollback failed; surviving process groups: {}",
            survivors
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    anyhow::ensure!(
        completion.clone().now_or_never().is_some(),
        "subprocess-e2b: terminal setup rollback failed; surviving pid: {}",
        handle.pid()
    );
    match handle.disconnect().await {
        Ok(()) => Ok(()),
        Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_not_aborted(signal: Option<&AbortSignal>, message: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!signal.is_some_and(AbortSignal::is_aborted), "{message}");
    Ok(())
}

fn grace_duration(value: f64) -> Duration {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Duration::from_millis(value.max(0.0) as u64)
}
