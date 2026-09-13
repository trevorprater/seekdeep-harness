//! Local PTY allocation and descendant-safe terminal-session ownership.

use std::{
    collections::BTreeSet,
    io::{Read as _, Write as _},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};
use seekdeep_subprocess::{
    ProcessGroupId, ProcessId, ProcessSignal, SubprocessOutcome, SubprocessOutput,
    SubprocessTerminalForeground, SubprocessTerminalHandle, SubprocessTerminalSignal,
    SubprocessTerminalSpawnSpec,
};
use tokio::io::{AsyncRead, ReadBuf};

use crate::{
    process_inspector::{ProcessIdentity, ProcessInspector, signal_unchecked_process},
    spawn::child_env,
};

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

struct ChannelReader {
    receiver: tokio::sync::mpsc::UnboundedReceiver<ChannelMessage>,
    current: Vec<u8>,
    offset: usize,
    ended: bool,
}

enum ChannelMessage {
    Data(Vec<u8>),
    End,
}

#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn decode(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);
        let split = complete_utf8_prefix(&bytes);
        self.pending.extend_from_slice(&bytes[split..]);
        String::from_utf8_lossy(&bytes[..split])
            .into_owned()
            .into_bytes()
    }

    fn finish(&mut self) -> Vec<u8> {
        String::from_utf8_lossy(&std::mem::take(&mut self.pending))
            .into_owned()
            .into_bytes()
    }
}

fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let mut cursor = 0;
    while cursor < bytes.len() {
        match std::str::from_utf8(&bytes[cursor..]) {
            Ok(_) => return bytes.len(),
            Err(error) => {
                let error_at = cursor + error.valid_up_to();
                let Some(error_length) = error.error_len() else {
                    return error_at;
                };
                cursor = error_at.saturating_add(error_length);
            }
        }
    }
    bytes.len()
}

impl std::fmt::Debug for ChannelReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalOutput")
            .field("buffered", &self.current.len().saturating_sub(self.offset))
            .finish_non_exhaustive()
    }
}

impl AsyncRead for ChannelReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        destination: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if self.ended {
                return Poll::Ready(Ok(()));
            }
            if self.offset < self.current.len() {
                let count = destination
                    .remaining()
                    .min(self.current.len().saturating_sub(self.offset));
                destination.put_slice(&self.current[self.offset..self.offset + count]);
                self.offset += count;
                return Poll::Ready(Ok(()));
            }
            match self.receiver.poll_recv(context) {
                Poll::Ready(Some(ChannelMessage::Data(chunk))) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Poll::Ready(Some(ChannelMessage::End) | None) => {
                    self.ended = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug, Default)]
struct CleanupControl {
    running: bool,
    complete: bool,
    generation: u64,
    last: Option<(u64, Result<(), Arc<str>>)>,
}

#[derive(Debug)]
struct OperationControl {
    accepting: AtomicBool,
    active: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl OperationControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            accepting: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            notify: tokio::sync::Notify::new(),
        })
    }

    fn enter(self: &Arc<Self>) -> anyhow::Result<OperationGuard> {
        anyhow::ensure!(
            self.accepting.load(Ordering::Acquire),
            "terminal session has terminated"
        );
        self.active.fetch_add(1, Ordering::AcqRel);
        if !self.accepting.load(Ordering::Acquire) {
            self.leave();
            anyhow::bail!("terminal session has terminated");
        }
        Ok(OperationGuard(self.clone()))
    }

    fn leave(&self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.notify.notify_waiters();
        }
    }

    async fn close_and_wait(&self) {
        self.accepting.store(false, Ordering::Release);
        loop {
            let notified = self.notify.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct OperationGuard(Arc<OperationControl>);

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// Concrete terminal handle backed by a native PTY.
pub struct LocalTerminalHandle {
    pid: ProcessId,
    output: SubprocessOutput,
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    inspector: Arc<dyn ProcessInspector>,
    grace: Duration,
    done: Arc<DoneSlot>,
    exited: Arc<AtomicBool>,
    tracked_descendants: Mutex<Vec<ProcessIdentity>>,
    root_identity: Option<ProcessIdentity>,
    cleanup: tokio::sync::Mutex<CleanupControl>,
    cleanup_notify: tokio::sync::Notify,
    operations: Arc<OperationControl>,
}

impl std::fmt::Debug for LocalTerminalHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalTerminalHandle")
            .field("pid", &self.pid)
            .field("exited", &self.exited.load(Ordering::Acquire))
            .field("root_identity", &self.root_identity)
            .finish_non_exhaustive()
    }
}

impl LocalTerminalHandle {
    /// Allocates one real native PTY and starts its output and exit observers.
    ///
    /// # Errors
    ///
    /// Returns invalid dimensions or native PTY allocation/spawn failures.
    #[allow(clippy::too_many_lines)]
    pub fn spawn(
        spec: &SubprocessTerminalSpawnSpec,
        inspector: Arc<dyn ProcessInspector>,
        grace: Duration,
    ) -> anyhow::Result<Arc<Self>> {
        let (rows, cols) = terminal_dimensions(spec.rows, spec.cols);
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut command = CommandBuilder::new(&spec.argv[0]);
        command.args(&spec.argv[1..]);
        command.cwd(&spec.cwd);
        command.env_clear();
        let explicit = spec.env.as_ref().map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect()
        });
        for (key, value) in child_env(explicit.as_ref()) {
            command.env(key, value);
        }
        command.env("PWD", &spec.cwd);
        command.env("TERM", "dumb");
        let mut child = pair.slave.spawn_command(command)?;
        let pid =
            ProcessId::new(i64::from(child.process_id().ok_or_else(|| {
                anyhow::anyhow!("terminal process did not report a pid")
            })?));
        let killer = Arc::new(Mutex::new(child.clone_killer()));
        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        drop(pair.slave);
        drop(pair.master);

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let end_sender = sender.clone();
        let (reader_done, reader_done_receiver) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let mut buffer = vec![0_u8; 16 * 1024];
            let mut decoder = Utf8StreamDecoder::default();
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let bytes = decoder.decode(&buffer[..read]);
                        if !bytes.is_empty() && sender.send(ChannelMessage::Data(bytes)).is_err() {
                            break;
                        }
                    }
                }
            }
            let final_bytes = decoder.finish();
            if !final_bytes.is_empty() {
                let _ = sender.send(ChannelMessage::Data(final_bytes));
            }
            drop(sender);
            let _ = reader_done.send(());
        });
        let output: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(ChannelReader {
            receiver,
            current: Vec::new(),
            offset: 0,
            ended: false,
        });
        let output = SubprocessOutput::new(output);
        let done = Arc::new(DoneSlot::default());
        let exited = Arc::new(AtomicBool::new(false));
        let wait_exited = exited.clone();
        let (outcome_sender, outcome_receiver) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let outcome = child.wait();
            wait_exited.store(true, Ordering::Release);
            let outcome = match outcome {
                Ok(status) => StoredDone::Success(SubprocessOutcome {
                    exit_code: status
                        .signal()
                        .is_none()
                        .then(|| i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
                    signal: status.signal().and_then(normalize_terminal_signal),
                }),
                Err(error) => StoredDone::Failure(Arc::from(error.to_string())),
            };
            let _ = outcome_sender.send(outcome);
        });
        let wait_done = done.clone();
        tokio::spawn(async move {
            let outcome = outcome_receiver.await.unwrap_or_else(|_| {
                StoredDone::Failure(Arc::from("terminal exit observer stopped"))
            });
            let _ = tokio::time::timeout(Duration::from_millis(200), reader_done_receiver).await;
            let _ = end_sender.send(ChannelMessage::End);
            wait_done.complete(outcome);
        });

        let root_identity = inspector
            .try_process_tree(pid)?
            .into_iter()
            .find(|member| member.pid == pid);
        Ok(Arc::new(Self {
            pid,
            output,
            writer,
            killer,
            inspector,
            grace,
            done,
            exited,
            tracked_descendants: Mutex::new(Vec::new()),
            root_identity,
            cleanup: tokio::sync::Mutex::new(CleanupControl::default()),
            cleanup_notify: tokio::sync::Notify::new(),
            operations: OperationControl::new(),
        }))
    }

    /// Force-terminates the observable session synchronously for host exit.
    pub fn terminate_for_host_exit(&self) {
        self.force_stop_descendants();
        self.force_stop_shell();
        self.force_stop_descendants();
    }

    fn force_stop_shell(&self) {
        if self.exited.load(Ordering::Acquire) {
            return;
        }
        if let Some(root) = self.root_identity.as_ref() {
            let _ = self.inspector.signal_process(root, true);
        } else {
            let _ = self.killer.lock().kill();
        }
    }

    fn survivors(&self, members: &[ProcessIdentity]) -> anyhow::Result<Vec<ProcessIdentity>> {
        let mut survivors = Vec::new();
        for member in members {
            if self.inspector.try_is_alive(member)? {
                survivors.push(member.clone());
            }
        }
        Ok(survivors)
    }

    fn descendants(&self) -> anyhow::Result<Vec<ProcessIdentity>> {
        let tree = self.inspector.try_process_tree(self.pid)?;
        let root = tree.iter().find(|member| member.pid == self.pid);
        let root_verified = self.root_identity.as_ref().is_some_and(|identity| {
            root.is_some_and(|current| current.started == identity.started)
        });
        let mut groups = vec![self.tracked_descendants.lock().clone()];
        if root_verified {
            groups.push(tree);
            groups.push(self.inspector.try_process_session(self.pid)?);
        }
        let members = union_members(groups)
            .into_iter()
            .filter(|member| member.pid != self.pid)
            .collect::<Vec<_>>();
        let survivors = self.survivors(&members)?;
        self.tracked_descendants.lock().clone_from(&survivors);
        Ok(survivors)
    }

    async fn wait_for_members(
        &self,
        members: &[ProcessIdentity],
    ) -> anyhow::Result<Vec<ProcessIdentity>> {
        let deadline = tokio::time::Instant::now() + self.grace;
        let mut survivors = self.survivors(members)?;
        while !survivors.is_empty() && tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep(remaining.min(Duration::from_millis(25))).await;
            survivors = self.survivors(members)?;
        }
        Ok(survivors)
    }

    fn signal_members(&self, members: &[ProcessIdentity], force: bool) {
        for member in members {
            let _ = self.inspector.signal_process(member, force);
        }
    }

    fn force_stop_descendants(&self) {
        let members = self
            .descendants()
            .unwrap_or_else(|_| self.tracked_descendants.lock().clone());
        self.signal_members(&members, true);
    }

    async fn stop_descendants(&self) -> anyhow::Result<Vec<ProcessIdentity>> {
        let captured = self.descendants()?;
        self.signal_members(&captured, false);
        let captured_survivors = self.wait_for_members(&captured).await?;
        let members = union_members([captured_survivors, self.descendants()?]);
        self.signal_members(&members, true);
        let survivors = self.wait_for_members(&members).await?;
        self.survivors(&union_members([survivors, self.descendants()?]))
    }

    async fn signal_shell_and_wait(&self, force: bool) {
        if force {
            let _ = self.killer.lock().kill();
        } else {
            let _ = signal_unchecked_process(self.pid, false);
        }
        let _ = tokio::time::timeout(self.grace, self.done.wait()).await;
    }

    async fn stop_shell(&self) -> anyhow::Result<()> {
        if !self.exited.load(Ordering::Acquire) {
            self.signal_shell_and_wait(false).await;
        }
        if !self.exited.load(Ordering::Acquire) {
            self.signal_shell_and_wait(true).await;
        }
        anyhow::ensure!(
            self.exited.load(Ordering::Acquire),
            "terminal cleanup failed; surviving pid: {}",
            self.pid.as_i64()
        );
        Ok(())
    }

    async fn close_once(&self) -> anyhow::Result<()> {
        let mut survivors = self.stop_descendants().await?;
        ensure_no_survivors(&survivors)?;
        self.stop_shell().await?;
        survivors = self.stop_descendants().await?;
        ensure_no_survivors(&survivors)
    }

    async fn terminate_joined(&self) -> anyhow::Result<()> {
        let (owner, generation) = loop {
            let notified = self.cleanup_notify.notified();
            let mut control = self.cleanup.lock().await;
            if control.complete {
                return Ok(());
            }
            if control.running {
                let generation = control.generation;
                drop(control);
                notified.await;
                let control = self.cleanup.lock().await;
                if let Some((finished, outcome)) = &control.last
                    && *finished == generation
                {
                    return outcome
                        .clone()
                        .map_err(|message| anyhow::anyhow!(message.to_string()));
                }
                continue;
            }
            control.running = true;
            control.generation = control.generation.saturating_add(1);
            break (true, control.generation);
        };
        debug_assert!(owner);
        let outcome = self.close_once().await;
        if outcome.is_ok() {
            self.operations.close_and_wait().await;
        }
        let stored = match outcome.as_ref() {
            Ok(()) => Ok(()),
            Err(error) => Err(Arc::<str>::from(format!("{error:#}"))),
        };
        let mut control = self.cleanup.lock().await;
        control.running = false;
        control.complete = outcome.is_ok();
        control.last = Some((generation, stored));
        drop(control);
        self.cleanup_notify.notify_waiters();
        outcome
    }
}

fn terminal_dimensions(rows: u32, cols: u32) -> (u16, u16) {
    // node-pty applies its defaults to zero and then assigns Int32 values to
    // the platform's u16 winsize fields, retaining the low sixteen bits.
    #[allow(clippy::cast_possible_truncation)]
    let rows = if rows == 0 { 24 } else { rows as u16 };
    #[allow(clippy::cast_possible_truncation)]
    let cols = if cols == 0 { 80 } else { cols as u16 };
    (rows, cols)
}

#[async_trait::async_trait]
impl SubprocessTerminalHandle for LocalTerminalHandle {
    fn pid(&self) -> ProcessId {
        self.pid
    }

    fn output(&self) -> SubprocessOutput {
        self.output.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        self.done.wait().await
    }

    async fn write(&self, data: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.exited.load(Ordering::Acquire),
            "terminal process has exited"
        );
        let _operation = self.operations.enter()?;
        let writer = self.writer.clone();
        let data = data.as_bytes().to_vec();
        tokio::task::spawn_blocking(move || writer.lock().write_all(&data)).await??;
        Ok(())
    }

    async fn inspect_foreground(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        let _operation = self.operations.enter()?;
        self.inspect_foreground_inner()
    }

    async fn signal_foreground(
        &self,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<ProcessGroupId> {
        let _operation = self.operations.enter()?;
        let foreground = self.inspect_foreground_inner()?.ok_or_else(|| {
            anyhow::anyhow!(
                "cannot resolve foreground process group for terminal {}",
                self.pid.as_i64()
            )
        })?;
        anyhow::ensure!(
            signal != SubprocessTerminalSignal::Sigkill
                || foreground.process_group_id.as_i64() != self.pid.as_i64(),
            "refusing to SIGKILL the terminal shell; terminate the terminal session instead"
        );
        self.inspector
            .signal_group(foreground.process_group_id, signal)?;
        Ok(foreground.process_group_id)
    }

    async fn terminate(&self) -> anyhow::Result<()> {
        self.terminate_joined().await
    }
}

impl LocalTerminalHandle {
    fn inspect_foreground_inner(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        self.descendants()?;
        let Some(process_group_id) = self.inspector.try_foreground_pgid(self.pid)? else {
            return Ok(None);
        };
        Ok(Some(SubprocessTerminalForeground {
            process_group_id,
            input_waiting: self.inspector.try_is_stdin_waiting(process_group_id)?,
        }))
    }
}

fn union_members(groups: impl IntoIterator<Item = Vec<ProcessIdentity>>) -> Vec<ProcessIdentity> {
    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    for group in groups {
        for member in group {
            if seen.insert((member.pid, member.started.clone())) {
                members.push(member);
            }
        }
    }
    members
}

fn ensure_no_survivors(survivors: &[ProcessIdentity]) -> anyhow::Result<()> {
    anyhow::ensure!(
        survivors.is_empty(),
        "terminal cleanup failed; surviving pids: {}",
        survivors
            .iter()
            .map(|member| member.pid.as_i64().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

fn normalize_terminal_signal(signal: &str) -> Option<ProcessSignal> {
    let lowercase = signal.to_ascii_lowercase();
    let name = if lowercase.starts_with("hangup") {
        Some("SIGHUP")
    } else if lowercase.starts_with("interrupt") {
        Some("SIGINT")
    } else if lowercase.starts_with("quit") {
        Some("SIGQUIT")
    } else if lowercase.starts_with("illegal instruction") {
        Some("SIGILL")
    } else if lowercase.starts_with("trace") {
        Some("SIGTRAP")
    } else if lowercase.starts_with("abort") {
        Some("SIGABRT")
    } else if lowercase.starts_with("bus error") {
        Some("SIGBUS")
    } else if lowercase.starts_with("floating point") {
        Some("SIGFPE")
    } else if lowercase.starts_with("killed") {
        Some("SIGKILL")
    } else if lowercase.starts_with("segmentation") {
        Some("SIGSEGV")
    } else if lowercase.starts_with("terminated") {
        Some("SIGTERM")
    } else if lowercase.starts_with("broken pipe") {
        Some("SIGPIPE")
    } else if lowercase.starts_with("alarm") {
        Some("SIGALRM")
    } else if lowercase.starts_with("user defined signal 1") {
        Some("SIGUSR1")
    } else if lowercase.starts_with("user defined signal 2") {
        Some("SIGUSR2")
    } else if lowercase.starts_with("stopped (signal)") {
        Some("SIGSTOP")
    } else if lowercase.starts_with("stopped (tty input)") {
        Some("SIGTTIN")
    } else if lowercase.starts_with("stopped (tty output)") {
        Some("SIGTTOU")
    } else if lowercase.starts_with("suspended") {
        Some("SIGTSTP")
    } else if lowercase.starts_with("continued") {
        Some("SIGCONT")
    } else if lowercase.starts_with("child exited") {
        Some("SIGCHLD")
    } else if lowercase.starts_with("window size") {
        Some("SIGWINCH")
    } else {
        None
    };
    name.map(ProcessSignal::new)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        io,
        path::PathBuf,
    };

    use seekdeep_subprocess::{SubprocessTerminalHandle as _, SubprocessTerminalSpawnSpec};
    use tokio::io::AsyncReadExt as _;

    use super::*;
    use crate::process_inspector::{ProcessStartIdentity, create_process_inspector};

    #[derive(Debug, Default)]
    struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct BlockingWriterState {
        started: bool,
        released: bool,
    }

    #[derive(Debug)]
    struct BlockingWriter {
        shared: Arc<(std::sync::Mutex<BlockingWriterState>, std::sync::Condvar)>,
    }

    impl std::io::Write for BlockingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let (state, changed) = &*self.shared;
            let mut state = state.lock().unwrap();
            state.started = true;
            changed.notify_all();
            while !state.released {
                state = changed.wait(state).unwrap();
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FakeKiller(Arc<Mutex<usize>>);

    impl ChildKiller for FakeKiller {
        fn kill(&mut self) -> io::Result<()> {
            *self.0.lock() += 1;
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self(self.0.clone()))
        }
    }

    #[derive(Debug)]
    struct FakeInspector {
        foreground: Mutex<Option<ProcessGroupId>>,
        waiting: AtomicBool,
        tree: Mutex<Vec<ProcessIdentity>>,
        tree_sequence: Mutex<VecDeque<Vec<ProcessIdentity>>>,
        session: Mutex<Vec<ProcessIdentity>>,
        alive: Mutex<BTreeSet<ProcessIdentity>>,
        groups: Mutex<Vec<(ProcessGroupId, SubprocessTerminalSignal)>>,
        processes: Mutex<Vec<(ProcessId, bool)>>,
        remove_on_signal: AtomicBool,
        fail_tree: AtomicBool,
        fail_process_signal: AtomicBool,
    }

    impl FakeInspector {
        fn new(root: ProcessIdentity) -> Arc<Self> {
            Arc::new(Self {
                foreground: Mutex::new(Some(ProcessGroupId::new(456))),
                waiting: AtomicBool::new(false),
                tree: Mutex::new(vec![root.clone()]),
                tree_sequence: Mutex::new(VecDeque::new()),
                session: Mutex::new(Vec::new()),
                alive: Mutex::new(BTreeSet::from([root])),
                groups: Mutex::new(Vec::new()),
                processes: Mutex::new(Vec::new()),
                remove_on_signal: AtomicBool::new(true),
                fail_tree: AtomicBool::new(false),
                fail_process_signal: AtomicBool::new(false),
            })
        }
    }

    impl ProcessInspector for FakeInspector {
        fn foreground_pgid(&self, _shell_pid: ProcessId) -> Option<ProcessGroupId> {
            *self.foreground.lock()
        }

        fn is_stdin_waiting(&self, _pgid: ProcessGroupId) -> bool {
            self.waiting.load(Ordering::Acquire)
        }

        fn process_tree(&self, _root_pid: ProcessId) -> Vec<ProcessIdentity> {
            if let Some(tree) = self.tree_sequence.lock().pop_front() {
                return tree;
            }
            self.tree.lock().clone()
        }

        fn try_process_tree(&self, root_pid: ProcessId) -> anyhow::Result<Vec<ProcessIdentity>> {
            anyhow::ensure!(
                !self.fail_tree.load(Ordering::Acquire),
                "process table unavailable"
            );
            Ok(self.process_tree(root_pid))
        }

        fn process_session(&self, _session_id: ProcessId) -> Vec<ProcessIdentity> {
            self.session.lock().clone()
        }

        fn is_alive(&self, identity: &ProcessIdentity) -> bool {
            self.alive.lock().contains(identity)
        }

        fn signal_group(
            &self,
            pgid: ProcessGroupId,
            signal: SubprocessTerminalSignal,
        ) -> anyhow::Result<()> {
            self.groups.lock().push((pgid, signal));
            Ok(())
        }

        fn signal_process(&self, identity: &ProcessIdentity, force: bool) -> anyhow::Result<()> {
            anyhow::ensure!(
                !self.fail_process_signal.load(Ordering::Acquire),
                "process raced"
            );
            if self.is_alive(identity) {
                self.processes.lock().push((identity.pid, force));
                if self.remove_on_signal.load(Ordering::Acquire) {
                    self.alive.lock().remove(identity);
                }
            }
            Ok(())
        }
    }

    fn identity(pid: i64, started: &str) -> ProcessIdentity {
        ProcessIdentity {
            pid: ProcessId::new(pid),
            started: ProcessStartIdentity::new(started),
        }
    }

    fn fake_handle(inspector: Arc<FakeInspector>, grace: Duration) -> Arc<LocalTerminalHandle> {
        let pid = ProcessId::new(123);
        let root_identity = inspector
            .process_tree(pid)
            .into_iter()
            .find(|member| member.pid == pid);
        let output: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(tokio::io::empty());
        Arc::new(LocalTerminalHandle {
            pid,
            output: SubprocessOutput::new(output),
            writer: Arc::new(Mutex::new(Box::new(RecordingWriter::default()))),
            killer: Arc::new(Mutex::new(Box::new(FakeKiller::default()))),
            inspector,
            grace,
            done: Arc::new(DoneSlot::default()),
            exited: Arc::new(AtomicBool::new(false)),
            tracked_descendants: Mutex::new(Vec::new()),
            root_identity,
            cleanup: tokio::sync::Mutex::new(CleanupControl::default()),
            cleanup_notify: tokio::sync::Notify::new(),
            operations: OperationControl::new(),
        })
    }

    fn spec(script: &str) -> SubprocessTerminalSpawnSpec {
        SubprocessTerminalSpawnSpec {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: PathBuf::from("/tmp"),
            env: Some(BTreeMap::from([(
                "TERMINAL_EXPLICIT".to_owned(),
                "visible".to_owned(),
            )])),
            rows: 24,
            cols: 80,
            grace_ms: 200.0,
            signal: None,
        }
    }

    #[tokio::test]
    async fn real_pty_bridges_input_output_environment_and_exit() {
        let request = spec(
            "printf 'ready:%s:%s:%s\\n' \"$TERMINAL_EXPLICIT\" \"$TERM\" \"$PWD\"; IFS= read -r line; printf 'got:%s\\n' \"$line\"",
        );
        let handle = LocalTerminalHandle::spawn(
            &request,
            create_process_inspector().unwrap(),
            Duration::from_millis(200),
        )
        .unwrap();
        handle.write("hello\n").await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), handle.done())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        let output = handle.output();
        let mut output = output.lock().await;
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), output.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("ready:visible:dumb:/tmp"), "{text:?}");
        assert!(text.contains("got:hello"), "{text:?}");
        handle.terminate().await.unwrap();
    }

    #[tokio::test]
    async fn foreground_shell_cannot_be_sigkilled_through_group_primitive() {
        let request = spec("IFS= read -r line");
        let handle = LocalTerminalHandle::spawn(
            &request,
            create_process_inspector().unwrap(),
            Duration::from_millis(200),
        )
        .unwrap();
        let mut foreground = None;
        for _ in 0..50 {
            foreground = handle.inspect_foreground().await.unwrap();
            if foreground.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let foreground = foreground.expect("foreground process group");
        assert_eq!(foreground.process_group_id.as_i64(), handle.pid().as_i64());
        let error = handle
            .signal_foreground(SubprocessTerminalSignal::Sigkill)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("terminate the terminal session"));
        handle.terminate().await.unwrap();
    }

    #[test]
    fn terminal_signal_names_are_normalized_to_node_spellings() {
        assert_eq!(
            normalize_terminal_signal("Killed: 9").unwrap().as_str(),
            "SIGKILL"
        );
        assert_eq!(
            normalize_terminal_signal("Terminated").unwrap().as_str(),
            "SIGTERM"
        );
        assert_eq!(normalize_terminal_signal("Future signal"), None);
    }

    #[test]
    fn terminal_utf8_decoder_carries_split_sequences_and_flushes_incomplete_tail() {
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[b'a', 0xe2]), b"a");
        assert!(decoder.decode(&[0x82]).is_empty());
        assert_eq!(decoder.decode(&[0xac, b'b']), "€b".as_bytes());
        assert_eq!(decoder.decode(&[0xf0, 0x9f]), b"");
        assert_eq!(decoder.finish(), "�".as_bytes());
    }

    #[test]
    fn terminal_dimensions_match_node_pty_defaulting_and_native_narrowing() {
        assert_eq!(terminal_dimensions(0, 0), (24, 80));
        assert_eq!(terminal_dimensions(24, 80), (24, 80));
        assert_eq!(terminal_dimensions(65_537, 131_074), (1, 2));
    }

    #[tokio::test]
    async fn injected_foreground_control_and_post_exit_write_failure_match_source() {
        let root = identity(123, "shell");
        let inspector = FakeInspector::new(root);
        inspector.waiting.store(true, Ordering::Release);
        let handle = fake_handle(inspector.clone(), Duration::from_millis(1));
        assert_eq!(
            handle.inspect_foreground().await.unwrap(),
            Some(SubprocessTerminalForeground {
                process_group_id: ProcessGroupId::new(456),
                input_waiting: true,
            })
        );
        assert_eq!(
            handle
                .signal_foreground(SubprocessTerminalSignal::Sigint)
                .await
                .unwrap(),
            ProcessGroupId::new(456)
        );
        assert_eq!(
            *inspector.groups.lock(),
            vec![(ProcessGroupId::new(456), SubprocessTerminalSignal::Sigint)]
        );
        *inspector.foreground.lock() = Some(ProcessGroupId::new(123));
        assert!(
            handle
                .signal_foreground(SubprocessTerminalSignal::Sigkill)
                .await
                .unwrap_err()
                .to_string()
                .contains("terminate the terminal session")
        );
        *inspector.foreground.lock() = None;
        assert!(
            handle
                .signal_foreground(SubprocessTerminalSignal::Sigterm)
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot resolve")
        );
        handle.exited.store(true, Ordering::Release);
        assert_eq!(
            handle.write("late").await.unwrap_err().to_string(),
            "terminal process has exited"
        );
    }

    #[test]
    fn synchronous_host_exit_kills_descendants_then_exact_root() {
        let root = identity(123, "shell");
        let child = identity(124, "child");
        let inspector = FakeInspector::new(root.clone());
        *inspector.tree.lock() = vec![root, child.clone()];
        inspector.alive.lock().insert(child);
        let handle = fake_handle(inspector.clone(), Duration::from_millis(1));
        handle.terminate_for_host_exit();
        assert_eq!(
            *inspector.processes.lock(),
            vec![(ProcessId::new(124), true), (ProcessId::new(123), true)]
        );
    }

    #[tokio::test]
    async fn host_exit_uses_captured_identities_when_final_scan_fails() {
        let root = identity(123, "shell");
        let child = identity(124, "captured");
        let inspector = FakeInspector::new(root.clone());
        *inspector.tree.lock() = vec![root, child.clone()];
        inspector.alive.lock().insert(child);
        let handle = fake_handle(inspector.clone(), Duration::from_millis(1));
        handle.inspect_foreground().await.unwrap();
        inspector.fail_tree.store(true, Ordering::Release);
        inspector.fail_process_signal.store(true, Ordering::Release);
        handle.terminate_for_host_exit();
        assert!(inspector.processes.lock().is_empty());
    }

    #[test]
    fn recycled_root_never_donates_or_receives_signals() {
        let root = identity(123, "shell");
        let inspector = FakeInspector::new(root);
        let handle = fake_handle(inspector.clone(), Duration::from_millis(1));
        let recycled = identity(123, "recycled");
        let imposter = identity(999, "imposter-child");
        *inspector.tree.lock() = vec![recycled.clone(), imposter.clone()];
        *inspector.alive.lock() = BTreeSet::from([recycled, imposter]);
        handle.terminate_for_host_exit();
        assert!(inspector.processes.lock().is_empty());
    }

    #[tokio::test]
    async fn failed_descendant_cleanup_can_be_retried_after_the_survivor_leaves() {
        let root = identity(123, "shell");
        let child = identity(124, "child");
        let inspector = FakeInspector::new(root.clone());
        *inspector.tree.lock() = vec![root, child.clone()];
        inspector.alive.lock().insert(child.clone());
        inspector.remove_on_signal.store(false, Ordering::Release);
        let handle = fake_handle(inspector.clone(), Duration::from_millis(1));
        assert!(
            handle
                .terminate()
                .await
                .unwrap_err()
                .to_string()
                .contains("surviving pids: 124")
        );
        inspector.alive.lock().remove(&child);
        inspector.remove_on_signal.store(true, Ordering::Release);
        handle.exited.store(true, Ordering::Release);
        handle.done.complete(StoredDone::Success(SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        }));
        handle.terminate().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_rescans_for_descendants_forked_during_term() {
        let root = identity(123, "shell");
        let first = identity(124, "first");
        let late = identity(125, "late");
        let inspector = FakeInspector::new(root.clone());
        *inspector.tree_sequence.lock() = VecDeque::from([
            vec![root.clone()],
            vec![root.clone(), first.clone()],
            vec![root.clone(), late.clone()],
            vec![root.clone()],
        ]);
        *inspector.tree.lock() = vec![root];
        *inspector.alive.lock() = BTreeSet::from([first, late]);
        let handle = fake_handle(inspector.clone(), Duration::from_millis(1));
        handle.exited.store(true, Ordering::Release);
        handle.done.complete(StoredDone::Success(SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        }));
        handle.terminate().await.unwrap();
        assert_eq!(
            *inspector.processes.lock(),
            vec![(ProcessId::new(124), false), (ProcessId::new(125), true)]
        );
    }

    #[tokio::test]
    async fn signal_races_are_contained_while_survivors_are_reported() {
        let root = identity(123, "shell");
        let child = identity(124, "child");
        let inspector = FakeInspector::new(root.clone());
        *inspector.tree.lock() = vec![root, child.clone()];
        inspector.alive.lock().insert(child);
        inspector.fail_process_signal.store(true, Ordering::Release);
        let handle = fake_handle(inspector, Duration::from_millis(1));
        assert!(
            handle
                .terminate()
                .await
                .unwrap_err()
                .to_string()
                .contains("surviving pids: 124")
        );
    }

    #[tokio::test]
    async fn successful_termination_waits_for_an_in_flight_handle_operation() {
        let root = identity(123, "shell");
        let inspector = FakeInspector::new(root);
        let mut handle = fake_handle(inspector, Duration::from_millis(1));
        let shared = Arc::new((
            std::sync::Mutex::new(BlockingWriterState::default()),
            std::sync::Condvar::new(),
        ));
        Arc::get_mut(&mut handle).unwrap().writer =
            Arc::new(Mutex::new(Box::new(BlockingWriter {
                shared: shared.clone(),
            })));
        let writing = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.write("blocked").await })
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !shared.0.lock().unwrap().started {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        handle.exited.store(true, Ordering::Release);
        handle.done.complete(StoredDone::Success(SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        }));
        let terminating = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.terminate().await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!terminating.is_finished());
        {
            let mut state = shared.0.lock().unwrap();
            state.released = true;
            shared.1.notify_all();
        }
        writing.await.unwrap().unwrap();
        terminating.await.unwrap().unwrap();
    }
}
