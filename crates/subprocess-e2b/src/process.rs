//! Asynchronously started E2B commands projected onto the subprocess seam.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use futures::{FutureExt as _, future::Shared};
use parking_lot::Mutex;
use seekdeep_e2b::{
    E2bCommandCompletion, E2bCommandExit, E2bCommandHandleRef, E2bCommandStartOptions,
    E2bFileNotFound, E2bOutputCallback, E2bSandbox, E2bSandboxNotFound, E2bService,
    e2b_control_envs, quote_e2b_shell_arg,
};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    ProcessId, ProcessSignal, SubprocessCollectedOutputs, SubprocessHandle, SubprocessInput,
    SubprocessOutcome, SubprocessOutput, SubprocessOutputMode, SubprocessOutputReaderHandle,
    SubprocessSpawnSpec, SubprocessStdinMode,
};
use tokio::io::{AsyncWrite, AsyncWriteExt as _};

use crate::{
    environment::{bootstrap_environment, read_remote_environment, serialize_remote_environment},
    output::{E2bBase64Decoder, E2bOutputReader},
    remote::{command_environment, signal_remote_groups, wait_tick},
};

const OUTPUT_ENCODER_SOURCE: &str = concat!(
    "(async () => {\n",
    "  for await (const chunk of process.stdin) {\n",
    "    if (!process.stdout.write(chunk.toString('base64') + '\\n')) {\n",
    "      await new Promise(resolve => process.stdout.once('drain', resolve))\n",
    "    }\n",
    "  }\n",
    "  if (!process.stdout.write('",
    "!seekdeep-e2b-output-complete!",
    "' + '\\n')) {\n",
    "    await new Promise(resolve => process.stdout.once('drain', resolve))\n",
    "  }\n",
    "})().catch(() => { process.exitCode = 1 })",
);

#[derive(Clone, Debug)]
struct RemotePaths {
    pid: String,
    status: String,
    environment: String,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Debug)]
enum CommandState {
    Pending,
    Published(E2bCommandHandleRef),
    Absent,
}

#[derive(Clone, Debug)]
enum ReadyState {
    Pending,
    Published(E2bCommandHandleRef),
    Failed(Arc<str>),
}

#[derive(Clone, Debug)]
enum DoneState {
    Pending,
    Settled(Result<SubprocessOutcome, Arc<str>>),
}

#[derive(Clone, Debug)]
enum CommandSettlement {
    Result(E2bCommandCompletion),
    Exit(E2bCommandExit),
    Error(Arc<str>),
}

type SharedTermination = Shared<futures::future::BoxFuture<'static, Result<(), Arc<str>>>>;
type SharedCompletion = Shared<futures::future::BoxFuture<'static, CommandSettlement>>;

struct PipeOutput {
    reader: SubprocessOutput,
    writer: Arc<tokio::sync::Mutex<Option<tokio::io::DuplexStream>>>,
}

impl std::fmt::Debug for PipeOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PipeOutput").finish_non_exhaustive()
    }
}

impl PipeOutput {
    fn new() -> Self {
        let (reader, writer) = tokio::io::duplex(64 * 1024);
        Self {
            reader: SubprocessOutput::new(Box::pin(reader)),
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
        }
    }

    async fn close(&self) {
        if let Some(mut writer) = self.writer.lock().await.take() {
            let _ = writer.shutdown().await;
        }
    }
}

struct DeferredStdin {
    ready: tokio::sync::watch::Receiver<ReadyState>,
    pending_write: Option<futures::future::BoxFuture<'static, io::Result<usize>>>,
    pending_close: Option<futures::future::BoxFuture<'static, io::Result<()>>>,
    closed: bool,
}

impl std::fmt::Debug for DeferredStdin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeferredStdin")
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl DeferredStdin {
    fn new(ready: tokio::sync::watch::Receiver<ReadyState>) -> Self {
        Self {
            ready,
            pending_write: None,
            pending_close: None,
            closed: false,
        }
    }

    fn poll_pending_write(&mut self, context: &mut TaskContext<'_>) -> Poll<io::Result<usize>> {
        let Some(pending) = self.pending_write.as_mut() else {
            return Poll::Ready(Ok(0));
        };
        match pending.as_mut().poll(context) {
            Poll::Ready(result) => {
                self.pending_write = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for DeferredStdin {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "subprocess stdin is closed",
            )));
        }
        if self.pending_write.is_some() {
            return self.poll_pending_write(context);
        }
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let ready = self.ready.clone();
        let bytes = buffer.to_vec();
        let length = bytes.len();
        self.pending_write = Some(
            async move {
                let handle = wait_ready(ready).await.map_err(io::Error::other)?;
                handle
                    .send_stdin(bytes)
                    .await
                    .map_err(|error| io::Error::other(format!("{error:#}")))?;
                Ok(length)
            }
            .boxed(),
        );
        self.poll_pending_write(context)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        if self.pending_write.is_some() {
            return self.poll_pending_write(context).map_ok(|_| ());
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pending_write.is_some() {
            match self.poll_pending_write(context) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }
        if self.closed {
            return Poll::Ready(Ok(()));
        }
        if self.pending_close.is_none() {
            let ready = self.ready.clone();
            self.pending_close = Some(
                async move {
                    let handle = wait_ready(ready).await.map_err(io::Error::other)?;
                    handle
                        .close_stdin()
                        .await
                        .map_err(|error| io::Error::other(format!("{error:#}")))
                }
                .boxed(),
            );
        }
        let pending = self.pending_close.as_mut().expect("close future exists");
        match pending.as_mut().poll(context) {
            Poll::Ready(result) => {
                self.pending_close = None;
                self.closed = true;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn wait_ready(
    mut receiver: tokio::sync::watch::Receiver<ReadyState>,
) -> Result<E2bCommandHandleRef, String> {
    loop {
        let state = receiver.borrow().clone();
        match state {
            ReadyState::Pending => {}
            ReadyState::Published(handle) => return Ok(handle),
            ReadyState::Failed(error) => return Err(error.to_string()),
        }
        if receiver.changed().await.is_err() {
            return Err("subprocess-e2b: command readiness ended without a result".to_owned());
        }
    }
}

/// E2B-backed subprocess handle with deferred remote process-group publication.
pub struct E2bSubprocessHandle {
    core: Arc<ProcessCore>,
    stdin: Option<SubprocessInput>,
    stdout: Option<SubprocessOutput>,
    stderr: Option<SubprocessOutput>,
    collected: SubprocessCollectedOutputs,
    done: tokio::sync::watch::Receiver<DoneState>,
}

impl std::fmt::Debug for E2bSubprocessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bSubprocessHandle")
            .field("pid", &self.pid())
            .field("state_dir", &self.core.state_dir)
            .finish_non_exhaustive()
    }
}

impl E2bSubprocessHandle {
    /// Starts asynchronous remote preparation while returning the seam handle immediately.
    ///
    /// # Errors
    ///
    /// Rejects invalid output limits or a non-UTF-8 working directory before remote work.
    #[allow(
        clippy::too_many_lines,
        reason = "one construction transaction wires every public stream and lifecycle publication channel"
    )]
    pub fn spawn(
        runtime: Arc<E2bService>,
        spec: SubprocessSpawnSpec,
        state_dir: impl Into<String>,
        poll_ms: u64,
    ) -> anyhow::Result<Arc<Self>> {
        let state_dir = state_dir.into();
        let cwd = spec
            .cwd
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("subprocess-e2b: cwd must be a UTF-8 remote path"))?;
        anyhow::ensure!(cwd.starts_with('/'), "subprocess-e2b: cwd must be absolute");
        let stdout_reader = output_reader(&spec.stdio.stdout, &format!("{state_dir}/stdout.log"))?;
        let stderr_reader = output_reader(&spec.stdio.stderr, &format!("{state_dir}/stderr.log"))?;
        let stdout_pipe =
            matches!(spec.stdio.stdout, SubprocessOutputMode::Pipe).then(PipeOutput::new);
        let stderr_pipe =
            matches!(spec.stdio.stderr, SubprocessOutputMode::Pipe).then(PipeOutput::new);
        let (command_sender, command) = tokio::sync::watch::channel(CommandState::Pending);
        let (ready_sender, ready) = tokio::sync::watch::channel(ReadyState::Pending);
        let (done_sender, done) = tokio::sync::watch::channel(DoneState::Pending);
        let core = Arc::new(ProcessCore {
            runtime,
            spec,
            state_dir: state_dir.clone(),
            paths: RemotePaths {
                pid: format!("{state_dir}/pid"),
                status: format!("{state_dir}/exit-code"),
                environment: format!("{state_dir}/environment"),
                stdout: format!("{state_dir}/stdout.log"),
                stderr: format!("{state_dir}/stderr.log"),
            },
            poll_ms,
            command_sender,
            command,
            ready_sender,
            ready: ready.clone(),
            remote_pid: AtomicI64::new(-1),
            stdout_decoder: Mutex::new(E2bBase64Decoder::default()),
            stderr_decoder: Mutex::new(E2bBase64Decoder::default()),
            stdout_pipe,
            stderr_pipe,
            stdout_reader,
            stderr_reader,
            control_env: Mutex::new(BTreeMap::new()),
            termination: AbortSignal::default(),
            output_released: AbortSignal::default(),
            output_error: Mutex::new(None),
            output_drain_expired: AtomicBool::new(false),
            state_created: AtomicBool::new(false),
            quiescent: AtomicBool::new(false),
            termination_attempt: Mutex::new(None),
            termination_failure: Mutex::new(None),
            termination_signal: Mutex::new(None),
        });
        let stdin = matches!(core.spec.stdio.stdin, SubprocessStdinMode::Pipe)
            .then(|| SubprocessInput::new(Box::pin(DeferredStdin::new(ready))));
        let stdout = core.stdout_pipe.as_ref().map(|pipe| pipe.reader.clone());
        let stderr = core.stderr_pipe.as_ref().map(|pipe| pipe.reader.clone());
        let collected = SubprocessCollectedOutputs {
            stdout: core
                .stdout_reader
                .as_ref()
                .map(|reader| reader.clone() as SubprocessOutputReaderHandle),
            stderr: core
                .stderr_reader
                .as_ref()
                .map(|reader| reader.clone() as SubprocessOutputReaderHandle),
        };
        let owner = core.clone();
        tokio::spawn(async move {
            let outcome = owner
                .run()
                .await
                .map_err(|error| Arc::<str>::from(format!("{error:#}")));
            done_sender.send_replace(DoneState::Settled(outcome));
        });
        if let Some(signal) = core.spec.signal.clone() {
            let weak = Arc::downgrade(&core);
            let mut completion = done.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = signal.cancelled() => {
                        if let Some(core) = weak.upgrade() {
                            core.start_termination();
                        }
                    }
                    () = wait_done_change(&mut completion) => {}
                }
            });
        }
        let handle = Arc::new(Self {
            core,
            stdin,
            stdout,
            stderr,
            collected,
            done,
        });
        if handle
            .core
            .spec
            .signal
            .as_ref()
            .is_some_and(AbortSignal::is_aborted)
        {
            handle.terminate();
        }
        Ok(handle)
    }

    /// Remote private state directory for diagnostics and tests.
    #[must_use]
    pub fn state_dir(&self) -> &str {
        &self.core.state_dir
    }
}

#[async_trait::async_trait]
impl SubprocessHandle for E2bSubprocessHandle {
    fn pid(&self) -> ProcessId {
        ProcessId::new(self.core.remote_pid.load(Ordering::Acquire))
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
        wait_done(self.done.clone()).await
    }

    fn terminate(&self) {
        self.core.start_termination();
    }

    async fn wait_for_exit(&self, signal: Option<AbortSignal>) -> anyhow::Result<bool> {
        self.core.wait_for_exit(signal.as_ref()).await
    }
}

async fn wait_done(
    mut receiver: tokio::sync::watch::Receiver<DoneState>,
) -> anyhow::Result<SubprocessOutcome> {
    loop {
        let state = receiver.borrow().clone();
        match state {
            DoneState::Pending => {}
            DoneState::Settled(Ok(outcome)) => return Ok(outcome),
            DoneState::Settled(Err(error)) => anyhow::bail!(error.to_string()),
        }
        anyhow::ensure!(
            receiver.changed().await.is_ok(),
            "subprocess-e2b: command completion ended without a result"
        );
    }
}

async fn wait_done_change(receiver: &mut tokio::sync::watch::Receiver<DoneState>) {
    loop {
        if !matches!(*receiver.borrow(), DoneState::Pending) {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

struct ProcessCore {
    runtime: Arc<E2bService>,
    spec: SubprocessSpawnSpec,
    state_dir: String,
    paths: RemotePaths,
    poll_ms: u64,
    command_sender: tokio::sync::watch::Sender<CommandState>,
    command: tokio::sync::watch::Receiver<CommandState>,
    ready_sender: tokio::sync::watch::Sender<ReadyState>,
    ready: tokio::sync::watch::Receiver<ReadyState>,
    remote_pid: AtomicI64,
    stdout_decoder: Mutex<E2bBase64Decoder>,
    stderr_decoder: Mutex<E2bBase64Decoder>,
    stdout_pipe: Option<PipeOutput>,
    stderr_pipe: Option<PipeOutput>,
    stdout_reader: Option<Arc<E2bOutputReader>>,
    stderr_reader: Option<Arc<E2bOutputReader>>,
    control_env: Mutex<BTreeMap<String, String>>,
    termination: AbortSignal,
    output_released: AbortSignal,
    output_error: Mutex<Option<Arc<str>>>,
    output_drain_expired: AtomicBool,
    state_created: AtomicBool,
    quiescent: AtomicBool,
    termination_attempt: Mutex<Option<SharedTermination>>,
    termination_failure: Mutex<Option<Arc<str>>>,
    termination_signal: Mutex<Option<ProcessSignal>>,
}

impl ProcessCore {
    #[allow(
        clippy::too_many_lines,
        reason = "the command owner keeps preparation, publication, monitoring, and rollback in one cancellation transaction"
    )]
    async fn run(self: &Arc<Self>) -> anyhow::Result<SubprocessOutcome> {
        let mut sandbox = None;
        let mut preparing = true;
        let outcome = async {
            let opened = self.runtime.get_sandbox().await?;
            sandbox = Some(opened.clone());
            self.prepare_state(opened.as_ref()).await?;
            preparing = false;
            let weak_stdout = Arc::downgrade(self);
            let weak_stderr = weak_stdout.clone();
            let stdout: E2bOutputCallback = Arc::new(move |text| {
                let core = weak_stdout.clone();
                async move {
                    if let Some(core) = core.upgrade() {
                        core.dispatch_output(true, &text).await;
                    }
                    Ok(())
                }
                .boxed()
            });
            let stderr: E2bOutputCallback = Arc::new(move |text| {
                let core = weak_stderr.clone();
                async move {
                    if let Some(core) = core.upgrade() {
                        core.dispatch_output(false, &text).await;
                    }
                    Ok(())
                }
                .boxed()
            });
            let command_env = e2b_control_envs(&self.control_env.lock().clone());
            let handle = opened
                .commands()
                .start(
                    &command_text(&self.spec, &self.paths),
                    E2bCommandStartOptions {
                        cwd: self
                            .spec
                            .cwd
                            .to_str()
                            .expect("validated UTF-8 cwd")
                            .to_owned(),
                        stdin: !matches!(self.spec.stdio.stdin, SubprocessStdinMode::Ignore),
                        timeout_ms: 0.0,
                        env: command_env,
                        signal: None,
                        on_stdout: stdout,
                        on_stderr: stderr,
                    },
                )
                .await?;
            if handle.pid() <= 0 {
                let invalid = anyhow::anyhow!(
                    "subprocess-e2b: E2B returned invalid command pid {}",
                    handle.pid()
                );
                match handle.kill().await {
                    Ok(_) => self.mark_quiescent(),
                    Err(cleanup) => {
                        *self.termination_failure.lock() = Some(Arc::<str>::from(format!(
                            "{cleanup:#}"
                        )));
                        self.command_sender
                            .send_replace(CommandState::Published(handle.clone()));
                        anyhow::bail!(
                            "{invalid:#}\nsubprocess-e2b: invalid command pid rollback did not reach quiescence: {cleanup:#}"
                        );
                    }
                }
                return Err(invalid);
            }
            self.command_sender
                .send_replace(CommandState::Published(handle.clone()));
            let completion = command_completion(handle.clone());
            let process_group = match self
                .wait_for_process_group_id(opened.as_ref(), completion.clone())
                .await
            {
                Ok(pid) => pid,
                Err(error) => {
                    if let Err(cleanup) = self
                        .force_kill_group(opened.as_ref(), handle.as_ref(), handle.pid())
                        .await
                    {
                        *self.termination_failure.lock() = Some(Arc::<str>::from(format!(
                            "{cleanup:#}"
                        )));
                        anyhow::bail!(
                            "{error:#}\nsubprocess-e2b: process-group publication failed and rollback did not reach quiescence: {cleanup:#}"
                        );
                    }
                    self.mark_quiescent();
                    return Err(error);
                }
            };
            self.remote_pid.store(process_group, Ordering::Release);
            self.ready_sender
                .send_replace(ReadyState::Published(handle.clone()));
            self.write_batch_stdin(handle.as_ref()).await;
            let outcome = self
                .wait_for_command(opened.as_ref(), handle.as_ref(), completion)
                .await?;
            if let Some(error) = self.output_error.lock().clone() {
                anyhow::bail!(error.to_string());
            }
            let require_complete = self.termination_signal.lock().is_none()
                && !self.output_drain_expired.load(Ordering::Acquire);
            self.stdout_decoder.lock().finish(require_complete)?;
            self.stderr_decoder.lock().finish(require_complete)?;
            self.finalize_spills(opened.as_ref()).await;
            Ok(outcome)
        }
        .await;

        let result = match outcome {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let canceled_preparation = preparing && self.termination.is_aborted();
                let mut failure = self.rollback_published_failure(error).await;
                let mut cleanup_failed = false;
                if let Some(sandbox) = sandbox.as_ref()
                    && self.state_created.load(Ordering::Acquire)
                    && let Err(cleanup) = self.remove_failed_state(sandbox.as_ref()).await
                {
                    cleanup_failed = true;
                    failure = anyhow::anyhow!(
                        "{failure:#}\nsubprocess-e2b: command failed and private state cleanup failed: {cleanup:#}"
                    );
                }
                if matches!(*self.command.borrow(), CommandState::Pending) {
                    self.command_sender.send_replace(CommandState::Absent);
                }
                self.ready_sender
                    .send_replace(ReadyState::Failed(Arc::<str>::from(format!("{failure:#}"))));
                if canceled_preparation && !cleanup_failed {
                    Ok(SubprocessOutcome {
                        exit_code: None,
                        signal: Some(ProcessSignal::new("SIGTERM")),
                    })
                } else {
                    Err(failure)
                }
            }
        };
        self.close_outputs().await;
        result
    }

    async fn prepare_state(&self, sandbox: &dyn E2bSandbox) -> anyhow::Result<()> {
        let ambient =
            read_remote_environment(sandbox.commands().as_ref(), Some(&self.termination)).await?;
        *self.control_env.lock() = bootstrap_environment(&ambient);
        self.state_created.store(true, Ordering::Release);
        sandbox
            .files()
            .make_dir(&self.state_dir, Some(&self.termination))
            .await?;
        let control_environment = command_environment(&self.control_env.lock().clone());
        sandbox
            .commands()
            .run(
                &format!("chmod 700 -- {}", quote_e2b_shell_arg(&self.state_dir)),
                control_environment,
                Some(&self.termination),
            )
            .await?;
        let mut files = vec![
            (self.paths.pid.as_str(), String::new()),
            (self.paths.status.as_str(), String::new()),
            (
                self.paths.environment.as_str(),
                serialize_remote_environment(&ambient, self.spec.env.as_ref())?,
            ),
        ];
        if has_spill(&self.spec.stdio.stdout) {
            files.push((self.paths.stdout.as_str(), String::new()));
        }
        if has_spill(&self.spec.stdio.stderr) {
            files.push((self.paths.stderr.as_str(), String::new()));
        }
        for (path, content) in &files {
            sandbox
                .files()
                .write(path, content, BTreeMap::new(), Some(&self.termination))
                .await?;
        }
        let quoted = files
            .iter()
            .map(|(path, _)| quote_e2b_shell_arg(path))
            .collect::<Vec<_>>()
            .join(" ");
        let control_environment = command_environment(&self.control_env.lock().clone());
        sandbox
            .commands()
            .run(
                &format!("chmod 600 -- {quoted}"),
                control_environment,
                Some(&self.termination),
            )
            .await?;
        anyhow::ensure!(
            !self.termination.is_aborted(),
            "subprocess-e2b: command terminated"
        );
        Ok(())
    }

    async fn dispatch_output(&self, stdout: bool, text: &str) {
        let decoded = if stdout {
            self.stdout_decoder.lock().push(text)
        } else {
            self.stderr_decoder.lock().push(text)
        };
        let bytes = match decoded {
            Ok(bytes) => bytes,
            Err(error) => {
                let mut stored = self.output_error.lock();
                if stored.is_none() {
                    *stored = Some(Arc::<str>::from(format!("{error:#}")));
                }
                return;
            }
        };
        let reader = if stdout {
            self.stdout_reader.as_ref()
        } else {
            self.stderr_reader.as_ref()
        };
        if let Some(reader) = reader
            && let Err(error) = reader.push(&bytes)
        {
            let mut stored = self.output_error.lock();
            if stored.is_none() {
                *stored = Some(Arc::<str>::from(format!("{error:#}")));
            }
        }
        let mode = if stdout {
            &self.spec.stdio.stdout
        } else {
            &self.spec.stdio.stderr
        };
        let pipe = if stdout {
            self.stdout_pipe.as_ref()
        } else {
            self.stderr_pipe.as_ref()
        };
        if let Err(error) = self.write_output(pipe, mode, stdout, &bytes).await {
            tracing::debug!(%error, "E2B subprocess output sink closed");
        }
    }

    async fn write_output(
        &self,
        pipe: Option<&PipeOutput>,
        mode: &SubprocessOutputMode,
        stdout: bool,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        if bytes.is_empty() || self.termination.is_aborted() {
            return Ok(());
        }
        if let Some(pipe) = pipe {
            let mut writer = pipe.writer.lock().await;
            let Some(writer) = writer.as_mut() else {
                anyhow::bail!("subprocess output stream is closed");
            };
            tokio::select! {
                result = writer.write_all(bytes) => result?,
                () = self.termination.cancelled() => {},
                () = self.output_released.cancelled() => {},
            }
        } else if matches!(mode, SubprocessOutputMode::Inherit) {
            if stdout {
                let mut target = tokio::io::stdout();
                target.write_all(bytes).await?;
                target.flush().await?;
            } else {
                let mut target = tokio::io::stderr();
                target.write_all(bytes).await?;
                target.flush().await?;
            }
        }
        Ok(())
    }

    async fn close_outputs(&self) {
        if let Some(pipe) = &self.stdout_pipe {
            pipe.close().await;
        }
        if let Some(pipe) = &self.stderr_pipe {
            pipe.close().await;
        }
    }

    async fn write_batch_stdin(&self, handle: &dyn seekdeep_e2b::E2bCommandHandle) {
        let SubprocessStdinMode::Data(data) = &self.spec.stdio.stdin else {
            return;
        };
        if handle.send_stdin(data.as_bytes().to_vec()).await.is_ok() {
            let _ = handle.close_stdin().await;
        }
    }

    async fn wait_for_process_group_id(
        &self,
        sandbox: &dyn E2bSandbox,
        completion: SharedCompletion,
    ) -> anyhow::Result<i64> {
        loop {
            let raw = read_text(sandbox, &self.paths.pid, None).await?;
            let value = raw.trim();
            if !value.is_empty() {
                anyhow::ensure!(
                    value.bytes().all(|byte| byte.is_ascii_digit()) && !value.starts_with('0'),
                    "subprocess-e2b: remote wrapper published invalid process-group id {value:?}"
                );
                let pid = value.parse::<i64>()?;
                anyhow::ensure!(
                    pid > 1,
                    "subprocess-e2b: unsafe published process-group id {pid}"
                );
                return Ok(pid);
            }
            tokio::select! {
                _ = completion.clone() => {
                    anyhow::bail!("subprocess-e2b: remote command exited before publishing its process-group id");
                }
                () = tokio::time::sleep(Duration::from_millis(self.poll_ms)) => {}
            }
        }
    }

    async fn wait_for_command(
        &self,
        sandbox: &dyn E2bSandbox,
        handle: &dyn seekdeep_e2b::E2bCommandHandle,
        completion: SharedCompletion,
    ) -> anyhow::Result<SubprocessOutcome> {
        let has_pipe = matches!(self.spec.stdio.stdout, SubprocessOutputMode::Pipe)
            || matches!(self.spec.stdio.stderr, SubprocessOutputMode::Pipe);
        let mut completed = if has_pipe {
            Some(completion.clone().await)
        } else {
            None
        };
        loop {
            let raw = read_text(sandbox, &self.paths.status, None).await?;
            let status = raw.trim();
            if !status.is_empty() {
                anyhow::ensure!(
                    status.bytes().all(|byte| byte.is_ascii_digit()),
                    "subprocess-e2b: remote wrapper published invalid exit code {status:?}"
                );
                let exit_code = status.parse::<i32>()?;
                anyhow::ensure!(
                    (0..=255).contains(&exit_code),
                    "subprocess-e2b: remote wrapper published invalid exit code {status:?}"
                );
                if let Some(completed) = completed {
                    return self.command_outcome(completed, Some(exit_code));
                }
                if let Ok(settlement) =
                    tokio::time::timeout(grace_duration(self.spec.grace_ms), completion.clone())
                        .await
                {
                    return self.command_outcome(settlement, Some(exit_code));
                }
                self.output_drain_expired.store(true, Ordering::Release);
                if let Some(reader) = &self.stdout_reader {
                    reader.invalidate_spill();
                }
                if let Some(reader) = &self.stderr_reader {
                    reader.invalidate_spill();
                }
                self.output_released.abort();
                handle.disconnect().await?;
                return Ok(SubprocessOutcome {
                    exit_code: Some(exit_code),
                    signal: None,
                });
            }
            if let Some(completed) = completed {
                return self.command_outcome(completed, None);
            }
            tokio::select! {
                settlement = completion.clone() => completed = Some(settlement),
                () = tokio::time::sleep(Duration::from_millis(self.poll_ms)) => {}
            }
        }
    }

    fn command_outcome(
        &self,
        settlement: CommandSettlement,
        published_exit_code: Option<i32>,
    ) -> anyhow::Result<SubprocessOutcome> {
        match settlement {
            CommandSettlement::Result(result) => Ok(SubprocessOutcome {
                exit_code: Some(published_exit_code.unwrap_or(result.exit_code)),
                signal: None,
            }),
            CommandSettlement::Exit(error) if published_exit_code.is_some() => {
                Ok(SubprocessOutcome {
                    exit_code: published_exit_code,
                    signal: None,
                })
            }
            CommandSettlement::Exit(error) => {
                let signal = self.termination_signal.lock().clone();
                if signal.is_some() {
                    Ok(SubprocessOutcome {
                        exit_code: None,
                        signal,
                    })
                } else {
                    Ok(SubprocessOutcome {
                        exit_code: Some(error.status),
                        signal: None,
                    })
                }
            }
            CommandSettlement::Error(error) => anyhow::bail!(error.to_string()),
        }
    }

    fn start_termination(self: &Arc<Self>) {
        if self.quiescent.load(Ordering::Acquire) || self.termination_attempt.lock().is_some() {
            return;
        }
        self.termination.abort();
        *self.termination_failure.lock() = None;
        let core = self.clone();
        let attempt = async move {
            core.terminate_remote()
                .await
                .map_err(|error| Arc::<str>::from(format!("{error:#}")))
        }
        .boxed()
        .shared();
        *self.termination_attempt.lock() = Some(attempt.clone());
        let core = self.clone();
        tokio::spawn(async move {
            if let Err(error) = attempt.await
                && !core.quiescent.load(Ordering::Acquire)
            {
                *core.termination_failure.lock() = Some(error);
            }
            core.termination_attempt.lock().take();
        });
    }

    async fn terminate_remote(&self) -> anyhow::Result<()> {
        let Some(handle) = wait_command_state(self.command.clone(), None).await? else {
            self.mark_quiescent();
            return Ok(());
        };
        if handle.pid() <= 0 && self.remote_pid.load(Ordering::Acquire) <= 0 {
            handle.kill().await?;
            self.mark_quiescent();
            return Ok(());
        }
        let sandbox = match self.runtime.get_sandbox().await {
            Ok(sandbox) => sandbox,
            Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => {
                self.mark_quiescent();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let pid = if self.remote_pid.load(Ordering::Acquire) > 0 {
            self.remote_pid.load(Ordering::Acquire)
        } else {
            handle.pid()
        };
        self.terminate_group(sandbox.as_ref(), handle.as_ref(), pid)
            .await
    }

    async fn terminate_group(
        &self,
        sandbox: &dyn E2bSandbox,
        handle: &dyn seekdeep_e2b::E2bCommandHandle,
        pid: i64,
    ) -> anyhow::Result<()> {
        *self.termination_signal.lock() = Some(ProcessSignal::new("SIGTERM"));
        let control_environment = self.control_env.lock().clone();
        let graceful = async {
            signal_remote_groups(
                sandbox.commands().as_ref(),
                &control_environment,
                &[pid],
                "TERM",
            )
            .await?;
            self.wait_for_group_exit(sandbox, pid).await
        }
        .await;
        if matches!(graceful, Ok(true)) {
            self.mark_quiescent();
            return Ok(());
        }
        *self.termination_signal.lock() = Some(ProcessSignal::new("SIGKILL"));
        self.force_kill_group(sandbox, handle, pid).await?;
        self.mark_quiescent();
        Ok(())
    }

    async fn force_kill_group(
        &self,
        sandbox: &dyn E2bSandbox,
        handle: &dyn seekdeep_e2b::E2bCommandHandle,
        pid: i64,
    ) -> anyhow::Result<()> {
        let control_environment = self.control_env.lock().clone();
        let _ = signal_remote_groups(
            sandbox.commands().as_ref(),
            &control_environment,
            &[pid],
            "KILL",
        )
        .await;
        let _ = handle.kill().await;
        anyhow::ensure!(
            self.wait_for_group_exit(sandbox, pid).await?,
            "subprocess-e2b: remote process group {pid} remained live after force termination"
        );
        Ok(())
    }

    async fn wait_for_group_exit(
        &self,
        sandbox: &dyn E2bSandbox,
        pid: i64,
    ) -> anyhow::Result<bool> {
        let deadline = tokio::time::Instant::now() + grace_duration(self.spec.grace_ms);
        while self.group_alive(sandbox, pid, None).await? {
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            let _ = wait_tick(self.poll_ms, None).await;
        }
        Ok(true)
    }

    async fn group_alive(
        &self,
        sandbox: &dyn E2bSandbox,
        pid: i64,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<bool> {
        let control_environment = command_environment(&self.control_env.lock().clone());
        let result = sandbox
            .commands()
            .run(
                &format!(
                    "set -o pipefail; ps -eo pgid=,stat= | awk '$1 == {pid} && $2 !~ /^[ZXx]/ {{ live=1 }} END {{ if (live) print \"live\" }}'"
                ),
                control_environment,
                signal,
            )
            .await;
        match result {
            Ok(result) => Ok(result.stdout.trim() == "live"),
            Err(_) if signal.is_some_and(AbortSignal::is_aborted) => Ok(false),
            Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn wait_for_exit(&self, signal: Option<&AbortSignal>) -> anyhow::Result<bool> {
        if self.quiescent.load(Ordering::Acquire) {
            return Ok(true);
        }
        let handle = if self.termination.is_aborted() {
            wait_command_state(self.command.clone(), signal).await?
        } else {
            match wait_ready_state(self.ready.clone(), signal).await? {
                Some(handle) => Some(handle),
                None => wait_command_state(self.command.clone(), signal).await?,
            }
        };
        let Some(handle) = handle else {
            if signal.is_some_and(AbortSignal::is_aborted) {
                return Ok(false);
            }
            self.mark_quiescent();
            return Ok(true);
        };
        self.throw_termination_failure()?;
        let acquisition = self.runtime.get_sandbox();
        tokio::pin!(acquisition);
        let acquired = if let Some(signal) = signal {
            tokio::select! {
                result = &mut acquisition => Some(result),
                () = signal.cancelled() => None,
            }
        } else {
            Some(acquisition.await)
        };
        let Some(acquired) = acquired else {
            return Ok(false);
        };
        let sandbox = match acquired {
            Ok(sandbox) => sandbox,
            Err(_) if signal.is_some_and(AbortSignal::is_aborted) => return Ok(false),
            Err(error) if error.downcast_ref::<E2bSandboxNotFound>().is_some() => {
                self.mark_quiescent();
                return Ok(true);
            }
            Err(error) => return Err(error),
        };
        let pid = if self.remote_pid.load(Ordering::Acquire) > 0 {
            self.remote_pid.load(Ordering::Acquire)
        } else {
            handle.pid()
        };
        while self.group_alive(sandbox.as_ref(), pid, signal).await? {
            self.throw_termination_failure()?;
            if !wait_tick(self.poll_ms, signal).await {
                return Ok(false);
            }
        }
        self.throw_termination_failure()?;
        if signal.is_some_and(AbortSignal::is_aborted) {
            return Ok(false);
        }
        self.mark_quiescent();
        Ok(true)
    }

    fn throw_termination_failure(&self) -> anyhow::Result<()> {
        if let Some(error) = self.termination_failure.lock().clone() {
            anyhow::bail!(error.to_string());
        }
        Ok(())
    }

    fn mark_quiescent(&self) {
        self.quiescent.store(true, Ordering::Release);
        self.termination_failure.lock().take();
    }

    async fn rollback_published_failure(self: &Arc<Self>, error: anyhow::Error) -> anyhow::Error {
        if self.remote_pid.load(Ordering::Acquire) <= 0 || self.quiescent.load(Ordering::Acquire) {
            return error;
        }
        self.start_termination();
        match self.wait_for_exit(None).await {
            Ok(true) => error,
            Ok(false) => anyhow::anyhow!(
                "{error:#}\nsubprocess-e2b: command monitoring failed and rollback was interrupted"
            ),
            Err(cleanup) => anyhow::anyhow!(
                "{error:#}\nsubprocess-e2b: command monitoring failed and process-group rollback did not reach quiescence: {cleanup:#}"
            ),
        }
    }

    async fn finalize_spills(&self, sandbox: &dyn E2bSandbox) {
        for (mode, reader, path) in [
            (
                &self.spec.stdio.stdout,
                self.stdout_reader.as_ref(),
                self.paths.stdout.as_str(),
            ),
            (
                &self.spec.stdio.stderr,
                self.stderr_reader.as_ref(),
                self.paths.stderr.as_str(),
            ),
        ] {
            let (SubprocessOutputMode::Collect(config), Some(reader), Some(spill)) =
                (mode, reader, collect_spill(mode))
            else {
                continue;
            };
            let maximum = byte_limit(spill.max_bytes).unwrap_or(u64::MAX);
            if self.output_drain_expired.load(Ordering::Acquire)
                || reader.size() <= byte_limit(config.max_bytes).unwrap_or(u64::MAX)
                || reader.size() > maximum
            {
                let _ = sandbox.files().remove(path).await;
            }
        }
    }

    async fn remove_failed_state(&self, sandbox: &dyn E2bSandbox) -> anyhow::Result<()> {
        let mut failures = Vec::new();
        for path in [&self.paths.environment, &self.state_dir] {
            if let Err(error) = sandbox.files().remove(path).await
                && error.downcast_ref::<E2bFileNotFound>().is_none()
            {
                failures.push(format!("{error:#}"));
            }
        }
        anyhow::ensure!(
            failures.is_empty(),
            "subprocess-e2b: failed to remove private command state: {}",
            failures.join("; ")
        );
        Ok(())
    }
}

async fn read_text(
    sandbox: &dyn E2bSandbox,
    path: &str,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<String> {
    String::from_utf8(sandbox.files().read_bytes(path, signal).await?).map_err(|error| {
        anyhow::anyhow!("subprocess-e2b: control file {path:?} is not UTF-8: {error}")
    })
}

fn command_completion(handle: E2bCommandHandleRef) -> SharedCompletion {
    async move {
        match handle.wait().await {
            Ok(result) => CommandSettlement::Result(result),
            Err(error) => error.downcast_ref::<E2bCommandExit>().map_or_else(
                || CommandSettlement::Error(Arc::<str>::from(format!("{error:#}"))),
                |exit| CommandSettlement::Exit(exit.clone()),
            ),
        }
    }
    .boxed()
    .shared()
}

async fn wait_command_state(
    mut receiver: tokio::sync::watch::Receiver<CommandState>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Option<E2bCommandHandleRef>> {
    loop {
        let state = receiver.borrow().clone();
        match state {
            CommandState::Pending => {}
            CommandState::Published(handle) => return Ok(Some(handle)),
            CommandState::Absent => return Ok(None),
        }
        if let Some(signal) = signal {
            tokio::select! {
                changed = receiver.changed() => {
                    anyhow::ensure!(changed.is_ok(), "subprocess-e2b: command publication ended without a result");
                }
                () = signal.cancelled() => return Ok(None),
            }
        } else {
            anyhow::ensure!(
                receiver.changed().await.is_ok(),
                "subprocess-e2b: command publication ended without a result"
            );
        }
    }
}

async fn wait_ready_state(
    mut receiver: tokio::sync::watch::Receiver<ReadyState>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Option<E2bCommandHandleRef>> {
    loop {
        let state = receiver.borrow().clone();
        match state {
            ReadyState::Pending => {}
            ReadyState::Published(handle) => return Ok(Some(handle)),
            ReadyState::Failed(_) => return Ok(None),
        }
        if let Some(signal) = signal {
            tokio::select! {
                changed = receiver.changed() => {
                    anyhow::ensure!(changed.is_ok(), "subprocess-e2b: readiness ended without a result");
                }
                () = signal.cancelled() => return Ok(None),
            }
        } else {
            anyhow::ensure!(
                receiver.changed().await.is_ok(),
                "subprocess-e2b: readiness ended without a result"
            );
        }
    }
}

fn output_reader(
    mode: &SubprocessOutputMode,
    spill_path: &str,
) -> anyhow::Result<Option<Arc<E2bOutputReader>>> {
    let SubprocessOutputMode::Collect(config) = mode else {
        return Ok(None);
    };
    let max_bytes = checked_byte_limit(config.max_bytes, "maxBytes")?;
    let max_spill = config
        .spill
        .as_ref()
        .map(|spill| checked_byte_limit(spill.max_bytes, "spill.maxBytes"))
        .transpose()?
        .map(|value| u64::try_from(value).unwrap_or(u64::MAX));
    Ok(Some(Arc::new(E2bOutputReader::new(
        max_bytes,
        max_spill,
        PathBuf::from(spill_path),
    ))))
}

fn checked_byte_limit(value: f64, field: &str) -> anyhow::Result<usize> {
    anyhow::ensure!(
        value.is_finite() && value >= 0.0 && value.fract() == 0.0,
        "{field} must be a non-negative integer"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

fn byte_limit(value: f64) -> Option<u64> {
    checked_byte_limit(value, "byte limit")
        .ok()
        .map(|value| u64::try_from(value).unwrap_or(u64::MAX))
}

fn grace_duration(value: f64) -> Duration {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Duration::from_millis(value.max(0.0) as u64)
}

fn has_spill(mode: &SubprocessOutputMode) -> bool {
    collect_spill(mode).is_some()
}

fn collect_spill(mode: &SubprocessOutputMode) -> Option<&seekdeep_subprocess::SubprocessSpill> {
    match mode {
        SubprocessOutputMode::Collect(config) => config.spill.as_ref(),
        SubprocessOutputMode::Pipe | SubprocessOutputMode::Inherit => None,
    }
}

fn command_text(spec: &SubprocessSpawnSpec, paths: &RemotePaths) -> String {
    let encoder = format!(
        "\"$seekdeep_e2b_env_bin\" -i \"$seekdeep_e2b_node\" -e {}",
        quote_e2b_shell_arg(OUTPUT_ENCODER_SOURCE)
    );
    let redirect = |mode: &SubprocessOutputMode, path: &str, stderr: bool| {
        let target = if stderr { " >&2" } else { "" };
        collect_spill(mode).map_or_else(
            || format!("> >({encoder}{target} 2>/dev/null)"),
            |spill| {
                format!(
                    "> >(\"$seekdeep_e2b_tee\" --output-error=warn-nopipe >(\"$seekdeep_e2b_head\" -c {} > {}) | {encoder}{target} 2>/dev/null)",
                    spill.max_bytes,
                    quote_e2b_shell_arg(path)
                )
            },
        )
    };
    let stdout = redirect(&spec.stdio.stdout, &paths.stdout, false);
    let stderr = redirect(&spec.stdio.stderr, &paths.stderr, true).replacen("> >(", "2> >(", 1);
    let mut inner = String::new();
    writeln!(inner, "set +e").unwrap();
    for (index, name) in ["env_bin", "node", "ps", "tr", "tee", "head", "rm"]
        .into_iter()
        .enumerate()
    {
        writeln!(inner, "seekdeep_e2b_{name}=${}", index + 1).unwrap();
    }
    writeln!(inner, "shift 7").unwrap();
    writeln!(inner, "seekdeep_e2b_pgid=\"$(\"$seekdeep_e2b_ps\" -o pgid= -p \"$$\" | \"$seekdeep_e2b_tr\" -d \" \")\"").unwrap();
    writeln!(
        inner,
        "printf '%s\\n' \"$seekdeep_e2b_pgid\" > {}",
        quote_e2b_shell_arg(&paths.pid)
    )
    .unwrap();
    writeln!(
        inner,
        "mapfile -d '' -t seekdeep_e2b_env < {}",
        quote_e2b_shell_arg(&paths.environment)
    )
    .unwrap();
    writeln!(
        inner,
        "\"$seekdeep_e2b_rm\" -f -- {}",
        quote_e2b_shell_arg(&paths.environment)
    )
    .unwrap();
    writeln!(
        inner,
        "\"$seekdeep_e2b_env_bin\" -i -- \"${{seekdeep_e2b_env[@]}}\" \"$@\" {stdout} {stderr}"
    )
    .unwrap();
    writeln!(inner, "seekdeep_e2b_status=$?").unwrap();
    writeln!(
        inner,
        "printf '%s\\n' \"$seekdeep_e2b_status\" > {}",
        quote_e2b_shell_arg(&paths.status)
    )
    .unwrap();
    writeln!(inner, "wait\nexit \"$seekdeep_e2b_status\"").unwrap();
    let argv = spec
        .argv
        .iter()
        .map(|argument| quote_e2b_shell_arg(argument))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        concat!(
            "mapfile -d '' -t seekdeep_e2b_env < {environment}\n",
            "seekdeep_e2b_env_bin=\"$(command -v env)\"\n",
            "seekdeep_e2b_setsid=\"$(command -v setsid)\"\n",
            "seekdeep_e2b_bash=\"$(command -v bash)\"\n",
            "seekdeep_e2b_node=\"$(command -v node)\"\n",
            "seekdeep_e2b_ps=\"$(command -v ps)\"\n",
            "seekdeep_e2b_tr=\"$(command -v tr)\"\n",
            "seekdeep_e2b_tee=\"$(command -v tee)\"\n",
            "seekdeep_e2b_head=\"$(command -v head)\"\n",
            "seekdeep_e2b_rm=\"$(command -v rm)\"\n",
            "for seekdeep_e2b_tool in \"$seekdeep_e2b_env_bin\" \"$seekdeep_e2b_setsid\" \"$seekdeep_e2b_bash\" \"$seekdeep_e2b_node\" \"$seekdeep_e2b_ps\" \"$seekdeep_e2b_tr\" \"$seekdeep_e2b_tee\" \"$seekdeep_e2b_head\" \"$seekdeep_e2b_rm\"; do\n",
            "  [[ \"$seekdeep_e2b_tool\" == /* && -x \"$seekdeep_e2b_tool\" ]] || exit 125\n",
            "done\n",
            "exec \"$seekdeep_e2b_env_bin\" -i -- \"${{seekdeep_e2b_env[@]}}\" \"$seekdeep_e2b_setsid\" --wait -- \"$seekdeep_e2b_bash\" -c {inner} seekdeep-e2b \"$seekdeep_e2b_env_bin\" \"$seekdeep_e2b_node\" \"$seekdeep_e2b_ps\" \"$seekdeep_e2b_tr\" \"$seekdeep_e2b_tee\" \"$seekdeep_e2b_head\" \"$seekdeep_e2b_rm\" {argv}"
        ),
        environment = quote_e2b_shell_arg(&paths.environment),
        inner = quote_e2b_shell_arg(&inner),
        argv = argv,
    )
}
