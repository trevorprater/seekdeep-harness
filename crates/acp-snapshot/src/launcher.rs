//! Rust-native ACP subprocess launch, capture, update, permission, and shutdown ownership.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_acp::{AcpClient, AcpPermissionHandler, AcpSessionUpdate, AcpUpdateObserver};
use seekdeep_loader_smoke::{
    ExampleLaunchOptions, ExampleMode, SEEKDEEP_AGENTS_HOME_ENV, resolve_example_launch,
};
use seekdeep_sdk_protocol::JsonRpcLineTransport;
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, ReadBuf},
    process::{Child, Command},
    sync::{Mutex as AsyncMutex, oneshot},
};

const UPDATE_STREAM_CLOSED: &str =
    "ACP test agent update stream closed before a matching session update arrived";
const EXIT_MARKER_GRACE: Duration = Duration::from_millis(250);

/// Compiled source/publish artifacts and leaf configuration for one ACP agent.
#[derive(Clone, Debug)]
pub struct AgentUnderTest {
    /// Development-mode compiled Rust executable.
    pub source_bin: PathBuf,
    /// Explicit publish-shaped executable for `lib` mode.
    pub library_bin: Option<PathBuf>,
    /// Leaf Cordis configuration path passed to the executable.
    pub config_path: PathBuf,
    /// Retained source contract field; compiled Rust launch needs no TypeScript resolver.
    pub tsconfig_path: PathBuf,
}

/// Options for one ACP test subprocess.
#[derive(Clone)]
pub struct AcpTestLaunchOptions {
    /// Agent composition to boot.
    pub agent: AgentUnderTest,
    /// Process cwd and isolated home root.
    pub cwd: PathBuf,
    /// Alternate leaf configuration for this launch.
    pub config_path: Option<PathBuf>,
    /// Explicit source or publish-shaped mode.
    pub mode: Option<ExampleMode>,
    /// Environment layered over the parent process before isolated homes.
    pub environment: BTreeMap<OsString, OsString>,
    /// Permission handler; absent requests fail closed as cancelled.
    pub request_permission: Option<AcpPermissionHandler>,
}

impl std::fmt::Debug for AcpTestLaunchOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpTestLaunchOptions")
            .field("agent", &self.agent)
            .field("cwd", &self.cwd)
            .field("config_path", &self.config_path)
            .field("mode", &self.mode)
            .field("environment", &self.environment)
            .field(
                "request_permission",
                &self.request_permission.as_ref().map(|_| "<handler>"),
            )
            .finish()
    }
}

/// Signal requested by a process-level ACP test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpTestSignal {
    /// Hangup.
    Hangup,
    /// Interactive interrupt.
    Interrupt,
    /// Graceful termination.
    Terminate,
    /// Unconditional termination.
    Kill,
}

impl AcpTestSignal {
    /// Stable platform signal spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hangup => "SIGHUP",
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

/// Fallible predicate for one future ACP update.
pub type AcpUpdatePredicate = Box<dyn Fn(&Value) -> anyhow::Result<bool> + Send + Sync + 'static>;

struct UpdateWaiter {
    predicate: AcpUpdatePredicate,
    sender: oneshot::Sender<anyhow::Result<Value>>,
}

#[derive(Default)]
struct UpdateState {
    updates: Vec<Value>,
    waiters: Vec<UpdateWaiter>,
    stream_closed: bool,
}

struct LaunchInner {
    child: AsyncMutex<Child>,
    client: Arc<AcpClient>,
    transport: Arc<JsonRpcLineTransport>,
    updates: Arc<Mutex<UpdateState>>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_closed: AsyncMutex<Option<oneshot::Receiver<()>>>,
    stderr_closed: AsyncMutex<Option<oneshot::Receiver<()>>>,
    close_gate: AsyncMutex<()>,
    closed: AtomicBool,
}

/// Running ACP child, protocol client, captures, and deterministic shutdown owner.
#[derive(Clone)]
pub struct LaunchedAcpTestAgent {
    inner: Arc<LaunchInner>,
}

impl std::fmt::Debug for LaunchedAcpTestAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchedAcpTestAgent")
            .field("pid", &self.pid())
            .field("updates", &self.inner.updates.lock().updates.len())
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl LaunchedAcpTestAgent {
    /// Connected ACP client.
    #[must_use]
    pub fn client(&self) -> Arc<AcpClient> {
        self.inner.client.clone()
    }

    /// Child process id while available.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.inner
            .child
            .try_lock()
            .ok()
            .and_then(|child| child.id())
    }

    /// Session updates captured in receive order.
    #[must_use]
    pub fn updates(&self) -> Vec<Value> {
        self.inner.updates.lock().updates.clone()
    }

    /// All stdout bytes consumed by the ACP parser so far.
    #[must_use]
    pub fn raw_stdout(&self) -> String {
        String::from_utf8_lossy(&self.inner.stdout.lock()).into_owned()
    }

    /// All stderr bytes consumed so far.
    #[must_use]
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.inner.stderr.lock()).into_owned()
    }

    /// Waits for a future Session update matching a fallible predicate.
    ///
    /// # Errors
    ///
    /// Returns predicate failure or the source close-before-match diagnostic.
    pub fn wait_for_update(
        &self,
        predicate: AcpUpdatePredicate,
    ) -> BoxFuture<'static, anyhow::Result<Value>> {
        let receiver = {
            let mut state = self.inner.updates.lock();
            if state.stream_closed {
                return Box::pin(async { Err(anyhow::anyhow!(UPDATE_STREAM_CLOSED)) });
            }
            let (sender, receiver) = oneshot::channel();
            state.waiters.push(UpdateWaiter { predicate, sender });
            receiver
        };
        Box::pin(async move {
            receiver
                .await
                .map_err(|_| anyhow::anyhow!(UPDATE_STREAM_CLOSED))?
        })
    }

    /// Closes stdin or signals the process, then drains inherited stdio and protocol callbacks.
    ///
    /// # Errors
    ///
    /// Returns signal, fallback termination, process wait, or stream-drain failures.
    pub async fn close(&self, signal: Option<AcpTestSignal>) -> anyhow::Result<()> {
        let _gate = self.inner.close_gate.lock().await;
        if self.inner.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut child = self.inner.child.lock().await;
        if child.try_wait()?.is_some() {
            drop(child);
            self.drain().await?;
            self.inner.closed.store(true, Ordering::Release);
            return Ok(());
        }

        let requested_failure = match signal {
            None => {
                self.inner.client.shutdown_output().await?;
                None
            }
            Some(signal) => request_signal(&mut child, signal).err(),
        };
        if let Some(failure) = requested_failure {
            if exit_marker_within_grace(&mut child).await? {
                drop(child);
                self.drain().await?;
                self.inner.closed.store(true, Ordering::Release);
                return Err(failure.into());
            }
            if let Err(fallback) = child.start_kill() {
                if exit_marker_within_grace(&mut child).await? {
                    drop(child);
                    self.drain().await?;
                    self.inner.closed.store(true, Ordering::Release);
                    return Err(failure.into());
                }
                close_update_stream(&self.inner.updates);
                anyhow::bail!(
                    "ACP test agent failed and fallback termination was refused: {failure}; {fallback}"
                );
            }
            let _ = child.wait().await;
            drop(child);
            self.drain().await?;
            self.inner.closed.store(true, Ordering::Release);
            return Err(failure.into());
        }
        child.wait().await?;
        drop(child);
        self.drain().await?;
        self.inner.closed.store(true, Ordering::Release);
        Ok(())
    }

    async fn drain(&self) -> anyhow::Result<()> {
        if let Some(receiver) = self.inner.stdout_closed.lock().await.take() {
            let _ = receiver.await;
        }
        if let Some(receiver) = self.inner.stderr_closed.lock().await.take() {
            let _ = receiver.await;
        }
        self.inner.transport.when_incoming_idle().await;
        close_update_stream(&self.inner.updates);
        Ok(())
    }
}

/// Boots one compiled ACP agent and connects the Rust ACP client to its stdio.
///
/// # Errors
///
/// Returns launch resolution, cwd, pipe, or synchronous spawn failures.
pub fn launch_acp_test_agent(
    options: AcpTestLaunchOptions,
) -> anyhow::Result<LaunchedAcpTestAgent> {
    let config_path = options
        .config_path
        .clone()
        .unwrap_or_else(|| options.agent.config_path.clone());
    let mut environment = options.environment.clone();
    environment.insert(
        seekdeep_util::home_paths::SEEKDEEP_HOME_ENV.into(),
        options.cwd.join(".seekdeep").into_os_string(),
    );
    environment.insert(
        SEEKDEEP_AGENTS_HOME_ENV.into(),
        options.cwd.join(".agents").into_os_string(),
    );
    let launch = resolve_example_launch(ExampleLaunchOptions {
        source_bin: options.agent.source_bin,
        library_bin: options.agent.library_bin,
        config_args: vec!["--config".into(), config_path.into_os_string()],
        mode: options.mode,
        environment,
    })?;
    tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("launchAcpTestAgent requires an active Tokio runtime"))?;
    let mut command = Command::new(&launch.command);
    command
        .args(&launch.args)
        .current_dir(&options.cwd)
        .envs(&launch.environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("ACP test agent stdin was not captured"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("ACP test agent stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("ACP test agent stderr was not captured"))?;

    let stdout_capture = Arc::new(Mutex::new(Vec::new()));
    let (stdout_sender, stdout_closed) = oneshot::channel();
    let tee = CapturingReader::new(stdout, stdout_capture.clone(), stdout_sender);
    let transport = JsonRpcLineTransport::new(tee, stdin);
    let permission = options
        .request_permission
        .unwrap_or_else(cancelled_permission_handler);
    let client = AcpClient::new_with_permission_handler(&transport, permission);
    let updates = Arc::new(Mutex::new(UpdateState::default()));
    let observer_state = updates.clone();
    client.on_update(Arc::new(move |update: &AcpSessionUpdate| {
        publish_update(&observer_state, &update.update);
    }) as AcpUpdateObserver);
    let closed_state = updates.clone();
    transport.on_input_failure(Arc::new(move |_error| {
        close_update_stream(&closed_state);
    }));
    client.start();

    let stderr_capture = Arc::new(Mutex::new(Vec::new()));
    let (stderr_sender, stderr_closed) = oneshot::channel();
    capture_stderr(stderr, stderr_capture.clone(), stderr_sender);
    Ok(LaunchedAcpTestAgent {
        inner: Arc::new(LaunchInner {
            child: AsyncMutex::new(child),
            client,
            transport,
            updates,
            stdout: stdout_capture,
            stderr: stderr_capture,
            stdout_closed: AsyncMutex::new(Some(stdout_closed)),
            stderr_closed: AsyncMutex::new(Some(stderr_closed)),
            close_gate: AsyncMutex::new(()),
            closed: AtomicBool::new(false),
        }),
    })
}

fn cancelled_permission_handler() -> AcpPermissionHandler {
    Arc::new(|_params| {
        Box::pin(async { Ok(serde_json::json!({"outcome":{"outcome":"cancelled"}})) })
    })
}

fn publish_update(state: &Mutex<UpdateState>, update: &Value) {
    let waiters = {
        let mut state = state.lock();
        state.updates.push(update.clone());
        std::mem::take(&mut state.waiters)
    };
    let mut pending = Vec::with_capacity(waiters.len());
    for waiter in waiters.into_iter().rev() {
        match (waiter.predicate)(update) {
            Ok(true) => {
                let _ = waiter.sender.send(Ok(update.clone()));
            }
            Ok(false) => pending.push(waiter),
            Err(error) => {
                let _ = waiter.sender.send(Err(error));
            }
        }
    }
    pending.reverse();
    let mut state = state.lock();
    if state.stream_closed {
        for waiter in pending {
            let _ = waiter
                .sender
                .send(Err(anyhow::anyhow!(UPDATE_STREAM_CLOSED)));
        }
    } else {
        pending.append(&mut state.waiters);
        state.waiters = pending;
    }
}

fn close_update_stream(state: &Mutex<UpdateState>) {
    let mut state = state.lock();
    if state.stream_closed {
        return;
    }
    state.stream_closed = true;
    for waiter in state.waiters.drain(..) {
        let _ = waiter
            .sender
            .send(Err(anyhow::anyhow!(UPDATE_STREAM_CLOSED)));
    }
}

fn capture_stderr(
    mut stderr: tokio::process::ChildStderr,
    capture: Arc<Mutex<Vec<u8>>>,
    closed: oneshot::Sender<()>,
) {
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes).await;
        capture.lock().extend(bytes);
        let _ = closed.send(());
    });
}

async fn exit_marker_within_grace(child: &mut Child) -> io::Result<bool> {
    if child.try_wait()?.is_some() {
        return Ok(true);
    }
    exit_future_within_grace(async { child.wait().await.map(|_| ()) }).await
}

async fn exit_future_within_grace<F>(exited: F) -> io::Result<bool>
where
    F: Future<Output = io::Result<()>>,
{
    match tokio::time::timeout(EXIT_MARKER_GRACE, exited).await {
        Ok(result) => result.map(|()| true),
        Err(_) => Ok(false),
    }
}

struct CapturingReader<R> {
    inner: R,
    capture: Arc<Mutex<Vec<u8>>>,
    closed: Option<oneshot::Sender<()>>,
}

impl<R> CapturingReader<R> {
    fn new(inner: R, capture: Arc<Mutex<Vec<u8>>>, closed: oneshot::Sender<()>) -> Self {
        Self {
            inner,
            capture,
            closed: Some(closed),
        }
    }

    fn mark_closed(&mut self) {
        if let Some(closed) = self.closed.take() {
            let _ = closed.send(());
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CapturingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        match Pin::new(&mut this.inner).poll_read(context, buffer) {
            Poll::Ready(Ok(())) => {
                let captured = &buffer.filled()[before..];
                if captured.is_empty() {
                    this.mark_closed();
                } else {
                    this.capture.lock().extend_from_slice(captured);
                }
                Poll::Ready(Ok(()))
            }
            result => result,
        }
    }
}

impl<R> Drop for CapturingReader<R> {
    fn drop(&mut self) {
        self.mark_closed();
    }
}

#[cfg(unix)]
fn request_signal(child: &mut Child, signal: AcpTestSignal) -> io::Result<()> {
    use nix::{
        errno::Errno,
        sys::signal::{Signal, kill},
        unistd::Pid,
    };

    let signal = match signal {
        AcpTestSignal::Hangup => Signal::SIGHUP,
        AcpTestSignal::Interrupt => Signal::SIGINT,
        AcpTestSignal::Terminate => Signal::SIGTERM,
        AcpTestSignal::Kill => Signal::SIGKILL,
    };
    let pid = child
        .id()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "child process has no pid"))?;
    kill(
        Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX)),
        signal,
    )
    .map_err(|error| {
        io::Error::from_raw_os_error(match error {
            Errno::UnknownErrno => libc_fallback_error_code(),
            error => error as i32,
        })
    })
}

#[cfg(unix)]
const fn libc_fallback_error_code() -> i32 {
    22
}

#[cfg(windows)]
fn request_signal(child: &mut Child, _signal: AcpTestSignal) -> io::Result<()> {
    child.start_kill()
}

#[cfg(test)]
mod tests {
    use std::{future, io, time::Duration};

    use super::exit_future_within_grace;

    #[tokio::test]
    async fn exit_marker_grace_accepts_a_late_marker_and_rejects_no_marker() {
        assert!(
            exit_future_within_grace(async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(())
            })
            .await
            .unwrap()
        );
        assert!(
            !exit_future_within_grace(future::pending::<io::Result<()>>())
                .await
                .unwrap()
        );
    }
}
