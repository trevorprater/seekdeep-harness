//! Readiness, cancellation, output, and teardown parity for the local PTY session.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    ProcessGroupId, ProcessId, ProcessSignal, SubprocessOutcome, SubprocessOutput,
    SubprocessTerminalForeground, SubprocessTerminalHandle, SubprocessTerminalHandleRef,
    SubprocessTerminalSignal,
};
use seekdeep_terminal::{
    TerminalBackendSession, TerminalError, TerminalErrorCode, TerminalReadRequest,
    TerminalSendRequest, TerminalSessionStatus, TerminalSignal, TerminalWaitReason,
};
use seekdeep_terminal_bash::{LocalPtySession, ResolvedTerminalBashConfig, TerminalBashConfig};
use tokio::io::AsyncWriteExt as _;

#[derive(Debug)]
struct Completion<T> {
    value: Mutex<Option<T>>,
    notify: tokio::sync::Notify,
}

impl<T> Default for Completion<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl<T: Clone> Completion<T> {
    fn resolve(&self, value: T) {
        let mut current = self.value.lock();
        if current.is_none() {
            *current = Some(value);
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> T {
        loop {
            let notified = self.notify.notified();
            if let Some(value) = self.value.lock().clone() {
                return value;
            }
            notified.await;
        }
    }
}

type InspectResult = Result<Option<SubprocessTerminalForeground>, String>;
type InspectGate = Arc<Completion<InspectResult>>;
type WriteGate = Arc<Completion<Result<(), String>>>;
type SignalGate = Arc<Completion<Result<ProcessGroupId, String>>>;

struct FakeTerminal {
    output: SubprocessOutput,
    writer: tokio::sync::Mutex<Option<tokio::io::DuplexStream>>,
    outcome: Arc<Completion<Result<SubprocessOutcome, String>>>,
    writes: Mutex<Vec<String>>,
    signals: Mutex<Vec<SubprocessTerminalSignal>>,
    terminate_calls: AtomicUsize,
    inspect_calls: AtomicUsize,
    foreground: Mutex<Option<SubprocessTerminalForeground>>,
    inspect_gate: Mutex<Option<InspectGate>>,
    write_gate: Mutex<Option<WriteGate>>,
    signal_gate: Mutex<Option<SignalGate>>,
    write_error: Mutex<Option<String>>,
    inspect_error: Mutex<Option<String>>,
    signal_error: Mutex<Option<String>>,
    terminate_error: Mutex<Option<String>>,
    auto_exit_on_terminate: AtomicBool,
    terminated: AtomicBool,
}

impl fmt::Debug for FakeTerminal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeTerminal")
            .field("writes", &self.writes)
            .field("signals", &self.signals)
            .finish_non_exhaustive()
    }
}

impl FakeTerminal {
    fn new() -> Arc<Self> {
        let (writer, reader) = tokio::io::duplex(65_536);
        Arc::new(Self {
            output: Arc::new(tokio::sync::Mutex::new(Box::pin(reader))),
            writer: tokio::sync::Mutex::new(Some(writer)),
            outcome: Arc::new(Completion::default()),
            writes: Mutex::new(Vec::new()),
            signals: Mutex::new(Vec::new()),
            terminate_calls: AtomicUsize::new(0),
            inspect_calls: AtomicUsize::new(0),
            foreground: Mutex::new(Some(SubprocessTerminalForeground {
                process_group_id: ProcessGroupId::new(456),
                input_waiting: false,
            })),
            inspect_gate: Mutex::new(None),
            write_gate: Mutex::new(None),
            signal_gate: Mutex::new(None),
            write_error: Mutex::new(None),
            inspect_error: Mutex::new(None),
            signal_error: Mutex::new(None),
            terminate_error: Mutex::new(None),
            auto_exit_on_terminate: AtomicBool::new(true),
            terminated: AtomicBool::new(false),
        })
    }

    async fn emit_bytes(&self, bytes: &[u8]) {
        self.writer
            .lock()
            .await
            .as_mut()
            .expect("live output")
            .write_all(bytes)
            .await
            .expect("emit output");
        drive().await;
    }

    async fn emit_data(&self, text: &str) {
        self.emit_bytes(text.as_bytes()).await;
    }

    async fn emit_exit(&self, exit_code: i32, signal: Option<&str>) {
        self.writer.lock().await.take();
        self.outcome.resolve(Ok(SubprocessOutcome {
            exit_code: signal.is_none().then_some(exit_code),
            signal: signal.map(ProcessSignal::new),
        }));
        drive().await;
    }

    async fn emit_failure(&self, message: &str) {
        self.writer.lock().await.take();
        self.outcome.resolve(Err(message.to_owned()));
        drive().await;
    }

    fn set_foreground(&self, pgid: Option<i64>, waiting: bool) {
        *self.foreground.lock() = pgid.map(|pgid| SubprocessTerminalForeground {
            process_group_id: ProcessGroupId::new(pgid),
            input_waiting: waiting,
        });
    }
}

#[async_trait]
impl SubprocessTerminalHandle for FakeTerminal {
    fn pid(&self) -> ProcessId {
        ProcessId::new(123)
    }

    fn output(&self) -> SubprocessOutput {
        self.output.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        self.outcome
            .wait()
            .await
            .map_err(|message| anyhow::anyhow!(message))
    }

    async fn write(&self, data: &str) -> anyhow::Result<()> {
        let gate = self.write_gate.lock().take();
        if let Some(gate) = gate {
            gate.wait()
                .await
                .map_err(|message| anyhow::anyhow!(message))?;
        }
        if let Some(error) = self.write_error.lock().clone() {
            anyhow::bail!(error);
        }
        self.writes.lock().push(data.to_owned());
        Ok(())
    }

    async fn inspect_foreground(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        self.inspect_calls.fetch_add(1, Ordering::AcqRel);
        let gate = self.inspect_gate.lock().take();
        if let Some(gate) = gate {
            return gate
                .wait()
                .await
                .map_err(|message| anyhow::anyhow!(message));
        }
        if let Some(error) = self.inspect_error.lock().clone() {
            anyhow::bail!(error);
        }
        Ok(self.foreground.lock().clone())
    }

    async fn signal_foreground(
        &self,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<ProcessGroupId> {
        let gate = self.signal_gate.lock().take();
        if let Some(gate) = gate {
            let target = gate
                .wait()
                .await
                .map_err(|message| anyhow::anyhow!(message))?;
            self.signals.lock().push(signal);
            return Ok(target);
        }
        if let Some(error) = self.signal_error.lock().clone() {
            anyhow::bail!(error);
        }
        let target = self
            .foreground
            .lock()
            .as_ref()
            .map(|foreground| foreground.process_group_id)
            .ok_or_else(|| anyhow::anyhow!("cannot resolve foreground process group"))?;
        self.signals.lock().push(signal);
        Ok(target)
    }

    async fn terminate(&self) -> anyhow::Result<()> {
        self.terminate_calls.fetch_add(1, Ordering::AcqRel);
        if let Some(error) = self.terminate_error.lock().clone() {
            anyhow::bail!(error);
        }
        if !self.terminated.swap(true, Ordering::AcqRel)
            && self.auto_exit_on_terminate.load(Ordering::Acquire)
        {
            self.emit_exit(0, Some("SIGTERM")).await;
        }
        Ok(())
    }
}

fn config() -> ResolvedTerminalBashConfig {
    TerminalBashConfig {
        shell_args: Vec::new(),
        rows: 24.0,
        cols: 80.0,
        scrollback_lines: 10.0,
        scrollback_max_bytes: 128.0,
        max_read_bytes: 64.0,
        poll_interval_ms: 10.0,
        exact_probe_after_ms: 20.0,
        idle_silence_ms: 50.0,
        handoff_grace_ms: 10.0,
        timeout_ms: 100.0,
        dispose_grace_ms: 20.0,
        ..TerminalBashConfig::default()
    }
    .resolve()
    .expect("config")
}

fn make_session(terminal: &Arc<FakeTerminal>) -> Arc<LocalPtySession> {
    let handle: SubprocessTerminalHandleRef = terminal.clone();
    LocalPtySession::new(handle, config())
}

fn send(text: &str, submit: bool) -> TerminalSendRequest {
    TerminalSendRequest {
        text: text.to_owned(),
        submit,
        signal: None,
    }
}

async fn drive() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn advance(milliseconds: u64) {
    tokio::time::advance(Duration::from_millis(milliseconds)).await;
    drive().await;
}

async fn initialize(session: &Arc<LocalPtySession>, terminal: &Arc<FakeTerminal>) {
    let initializing = {
        let session = session.clone();
        tokio::spawn(async move { session.initialize(None).await })
    };
    drive().await;
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    advance(10).await;
    initializing
        .await
        .expect("initialization task")
        .expect("initialize");
}

#[tokio::test(start_paused = true)]
async fn captures_prompt_motd_writes_submit_and_settles_exact_stdin_waits() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    assert_eq!(session.motd(), "seekdeep> ");

    let operation = session.start_send(send("python3", true)).expect("send");
    drive().await;
    assert_eq!(terminal.writes.lock().as_slice(), ["python3\r"]);
    terminal.set_foreground(Some(789), true);
    terminal.emit_data("Python\r\n>>> ").await;
    advance(20).await;
    let result = operation.done().await.expect("settled");
    assert_eq!(result.wait_reason, TerminalWaitReason::StdinRead);
    assert_eq!(result.viewport, "Python\n>>> ");
    assert_eq!(result.session_status, TerminalSessionStatus::Running);
    assert!(!operation.cancel());
}

#[tokio::test(start_paused = true)]
async fn does_not_reuse_a_prewrite_stdin_wait_as_postwrite_readiness() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    terminal.set_foreground(Some(456), true);
    let operation = session.start_send(send("echo ready", true)).expect("send");
    advance(20).await;
    assert!(operation.done().now_or_never().is_none());
    terminal.set_foreground(Some(456), false);
    advance(10).await;
    terminal.set_foreground(Some(456), true);
    advance(10).await;
    assert_eq!(
        operation.done().await.expect("settled").wait_reason,
        TerminalWaitReason::StdinRead
    );
}

#[tokio::test(start_paused = true)]
async fn distinguishes_inferred_idle_timeout_and_signal_exit() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    terminal.set_foreground(None, false);
    let inferred = session.start_send(send("sleep", false)).expect("send");
    terminal.emit_data("working").await;
    assert_eq!(inferred.read_output().delta, "working");
    advance(60).await;
    assert_eq!(
        inferred.done().await.expect("idle").wait_reason,
        TerminalWaitReason::InferredIdle
    );

    let timeout = session.start_send(send("blocked", false)).expect("send");
    for _ in 0..3 {
        advance(30).await;
        terminal.emit_data(".").await;
    }
    advance(10).await;
    assert_eq!(
        timeout.done().await.expect("timeout").wait_reason,
        TerminalWaitReason::Timeout
    );

    let exiting = session.start_send(send("exit", true)).expect("send");
    terminal.emit_exit(7, Some("SIGKILL")).await;
    let result = exiting.done().await.expect("exit");
    assert_eq!(result.wait_reason, TerminalWaitReason::SessionExit);
    assert_eq!(
        result.session_status,
        TerminalSessionStatus::Exited {
            exit_code: None,
            signal: Some(ProcessSignal::new("SIGKILL")),
        }
    );
    assert!(
        session
            .start_send(send("", false))
            .expect_err("exited")
            .to_string()
            .contains("has exited")
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_signals_foreground_observes_abort_and_contains_write_failure() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let signal = AbortSignal::default();
    let operation = session
        .start_send(TerminalSendRequest {
            text: "sleep".to_owned(),
            submit: true,
            signal: Some(signal.clone()),
        })
        .expect("send");
    assert_eq!(
        session
            .start_send(send("again", true))
            .expect_err("active")
            .downcast_ref::<TerminalError>()
            .expect("typed")
            .code(),
        TerminalErrorCode::SendActive
    );
    signal.abort();
    drive().await;
    assert!(
        terminal
            .signals
            .lock()
            .contains(&SubprocessTerminalSignal::Sigint)
    );
    terminal.emit_data("\x1b]133;D;130\x07seekdeep> ").await;
    advance(10).await;
    operation.done().await.expect("cancel settles");

    let aborted = AbortSignal::default();
    aborted.abort();
    assert_eq!(
        session
            .start_send(TerminalSendRequest {
                text: String::new(),
                submit: false,
                signal: Some(aborted),
            })
            .expect_err("pre-aborted")
            .to_string(),
        "PTY send aborted before write"
    );

    *terminal.write_error.lock() = Some("write failed".to_owned());
    let failed = session.start_send(send("x", false)).expect("operation");
    assert_eq!(
        failed.done().await.expect_err("write error").to_string(),
        "write failed"
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_prewrite_inspection_never_writes() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let inspection = Arc::new(Completion::default());
    *terminal.inspect_gate.lock() = Some(inspection.clone());
    let signal = AbortSignal::default();
    let operation = session
        .start_send(TerminalSendRequest {
            text: "must not execute".to_owned(),
            submit: true,
            signal: Some(signal.clone()),
        })
        .expect("send");
    drive().await;
    signal.abort();
    inspection.resolve(Ok(Some(SubprocessTerminalForeground {
        process_group_id: ProcessGroupId::new(456),
        input_waiting: false,
    })));
    drive().await;
    assert!(terminal.writes.lock().is_empty());
    assert!(
        terminal
            .signals
            .lock()
            .contains(&SubprocessTerminalSignal::Sigint)
    );
    terminal.emit_data("\x1b]133;D;130\x07seekdeep> ").await;
    advance(10).await;
    operation.done().await.expect("settled");
}

#[tokio::test(start_paused = true)]
async fn signal_waits_for_inflight_write_and_timeout_retains_ownership_until_drain() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let write_gate = Arc::new(Completion::default());
    *terminal.write_gate.lock() = Some(write_gate.clone());
    let operation = session.start_send(send("slow write", true)).expect("send");
    drive().await;
    assert!(operation.cancel());
    drive().await;
    assert!(terminal.signals.lock().is_empty());
    advance(100).await;
    assert_eq!(
        operation.done().await.expect("timeout").wait_reason,
        TerminalWaitReason::Timeout
    );
    assert!(
        session
            .start_send(send("successor", true))
            .expect_err("write drain")
            .to_string()
            .contains("draining provider write")
    );
    write_gate.resolve(Ok(()));
    drive().await;
    assert_eq!(
        terminal.signals.lock().as_slice(),
        [SubprocessTerminalSignal::Sigint]
    );
    let successor = session
        .start_send(send("successor", false))
        .expect("successor after drain");
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    advance(10).await;
    successor.done().await.expect("successor settles");
}

#[tokio::test(start_paused = true)]
async fn pagination_utf8_bounds_unknown_bytes_and_signal_delivery_are_preserved() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    terminal.emit_bytes(&[b'a', 0xff, b'b', b'\n']).await;
    terminal.emit_data("one\ntwo\nthree\nfour").await;
    let read = session
        .read(TerminalReadRequest {
            offset: Some(0.0),
            count: Some(2.0),
        })
        .expect("read");
    assert_eq!(read.text, "three\nfour");
    assert!(
        session
            .read(TerminalReadRequest {
                offset: Some(-1.0),
                count: None,
            })
            .expect_err("negative offset")
            .to_string()
            .contains("non-negative safe integer")
    );
    let all = session
        .read(TerminalReadRequest::default())
        .expect("all scrollback");
    assert!(all.text.contains('�'));
    let signal = session
        .signal(TerminalSignal::SIGTERM)
        .await
        .expect("signal");
    assert!(signal.is_delivered());
    assert_eq!(signal.target_pgid, ProcessGroupId::new(456));
}

#[tokio::test(start_paused = true)]
async fn close_is_idempotent_settles_active_send_and_retries_cleanup_failure() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    *terminal.terminate_error.lock() = Some("kill failed".to_owned());
    let first = session
        .close("test failure")
        .await
        .expect_err("cleanup failure");
    assert_eq!(first.to_string(), "PTY cleanup failed (test failure)");
    *terminal.terminate_error.lock() = None;
    session.close("retry").await.expect("retry close");
    assert!(
        session
            .signal(TerminalSignal::SIGINT)
            .await
            .expect_err("closing signal")
            .to_string()
            .contains("closing")
    );
    session.close("joined").await.expect("idempotent close");

    let active_terminal = FakeTerminal::new();
    let active_handle: SubprocessTerminalHandleRef = active_terminal.clone();
    let active_session = LocalPtySession::new(active_handle, config());
    initialize(&active_session, &active_terminal).await;
    let active = active_session
        .start_send(send("long", false))
        .expect("active");
    active_session.close("normal").await.expect("active close");
    assert_eq!(
        active.done().await.expect("active settles").wait_reason,
        TerminalWaitReason::SessionExit
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_transport_failure_is_contained_and_preserved_by_close() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let active = session.start_send(send("work", false)).expect("active");
    terminal.emit_failure("transport failed").await;
    assert_eq!(
        active
            .done()
            .await
            .expect_err("transport failure")
            .to_string(),
        "transport failed"
    );
    assert_eq!(
        session.status(),
        TerminalSessionStatus::Exited {
            exit_code: None,
            signal: None,
        }
    );
    assert_eq!(
        session
            .close("cleanup")
            .await
            .expect_err("first transport failure")
            .to_string(),
        "transport failed"
    );
}

#[tokio::test(start_paused = true)]
async fn zero_output_startup_needs_real_readiness_and_timeout_is_an_error() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    let initializing = {
        let session = session.clone();
        tokio::spawn(async move { session.initialize(None).await })
    };
    advance(60).await;
    assert!(!initializing.is_finished());
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    advance(10).await;
    initializing
        .await
        .expect("initialization task")
        .expect("readiness");

    let timeout_terminal = FakeTerminal::new();
    let timeout_session = make_session(&timeout_terminal);
    let timed_out = {
        let session = timeout_session.clone();
        tokio::spawn(async move { session.initialize(None).await })
    };
    advance(100).await;
    assert_eq!(
        timed_out
            .await
            .expect("timeout task")
            .expect_err("startup timeout")
            .to_string(),
        "PTY shell did not reach readiness before startup timeout"
    );
}

#[tokio::test(start_paused = true)]
async fn startup_preserves_abort_reason_when_foreground_cannot_resolve() {
    let terminal = FakeTerminal::new();
    terminal.set_foreground(None, false);
    let session = make_session(&terminal);
    let signal = AbortSignal::default();
    let reason = seekdeep_terminal::TerminalFailure::message("startup cancelled");
    let initializing = {
        let session = session.clone();
        let signal = signal.clone();
        tokio::spawn(async move { session.initialize(Some(signal)).await })
    };
    drive().await;
    signal.abort_with_typed_reason(
        Arc::new(reason.clone()),
        serde_json::json!(reason.to_string()),
    );
    let error = initializing
        .await
        .expect("initialization task")
        .expect_err("cancelled");
    assert!(error.ptr_eq(&reason));
}

#[tokio::test(start_paused = true)]
async fn startup_waits_for_split_printable_prompt_text() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    let initializing = {
        let session = session.clone();
        tokio::spawn(async move { session.initialize(None).await })
    };
    drive().await;
    terminal.emit_data("\x1b]133;D;0\x07").await;
    advance(20).await;
    assert!(!initializing.is_finished());
    terminal.emit_data("seekdeep> ").await;
    advance(10).await;
    initializing
        .await
        .expect("initialization task")
        .expect("split prompt");
    assert_eq!(session.motd(), "seekdeep> ");
}

#[tokio::test(start_paused = true)]
async fn prompt_marker_waits_for_shell_handoff_then_can_fall_back_to_idle() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let operation = session.start_send(send("run", true)).expect("send");
    drive().await;
    terminal.set_foreground(Some(789), false);
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    advance(50).await;
    assert!(operation.done().now_or_never().is_none());
    terminal.set_foreground(Some(456), false);
    advance(10).await;
    assert_eq!(
        operation.done().await.expect("handoff").wait_reason,
        TerminalWaitReason::StdinRead
    );

    let child = session.start_send(send("bash -i", true)).expect("child");
    drive().await;
    terminal.set_foreground(Some(789), false);
    terminal.emit_data("\x1b]133;D;0\x07child> ").await;
    advance(100).await;
    assert_eq!(
        child.done().await.expect("idle fallback").wait_reason,
        TerminalWaitReason::InferredIdle
    );
}

#[tokio::test(start_paused = true)]
async fn canceled_write_rejection_skips_signal_and_releases_the_slot() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let write_gate = Arc::new(Completion::default());
    *terminal.write_gate.lock() = Some(write_gate.clone());
    let operation = session
        .start_send(send("rejected write", true))
        .expect("send");
    drive().await;
    assert!(operation.cancel());
    write_gate.resolve(Err("write failed after cancellation".to_owned()));
    assert_eq!(
        operation
            .done()
            .await
            .expect_err("write rejection")
            .to_string(),
        "write failed after cancellation"
    );
    assert!(terminal.signals.lock().is_empty());
    let next = session.start_send(send("", false)).expect("next");
    advance(100).await;
    assert_eq!(
        next.done().await.expect("next idle").wait_reason,
        TerminalWaitReason::InferredIdle
    );
}

#[tokio::test(start_paused = true)]
async fn failed_cancellation_signal_is_a_transport_failure_after_write_drain() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let write_gate = Arc::new(Completion::default());
    *terminal.write_gate.lock() = Some(write_gate.clone());
    *terminal.signal_error.lock() = Some("interrupt failed".to_owned());
    let operation = session.start_send(send("slow write", true)).expect("send");
    drive().await;
    assert!(operation.cancel());
    advance(20).await;
    assert!(terminal.signals.lock().is_empty());
    assert!(
        session
            .start_send(send("must wait", true))
            .expect_err("active")
            .to_string()
            .contains("active send")
    );
    write_gate.resolve(Ok(()));
    assert_eq!(
        operation
            .done()
            .await
            .expect_err("interrupt failure")
            .to_string(),
        "interrupt failed"
    );
    assert_eq!(
        session.status(),
        TerminalSessionStatus::Exited {
            exit_code: None,
            signal: None,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_exit_flushes_incomplete_utf8_with_replacement() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    let operation = session.start_send(send("", false)).expect("send");
    terminal.emit_bytes(&[0xe2]).await;
    terminal.emit_exit(0, None).await;
    assert_eq!(operation.done().await.expect("exit").viewport, "�");
}

#[tokio::test(start_paused = true)]
async fn startup_exit_reports_status_and_unknown_signal_spelling_survives() {
    let startup_terminal = FakeTerminal::new();
    let startup = make_session(&startup_terminal);
    let initializing = {
        let startup = startup.clone();
        tokio::spawn(async move { startup.initialize(None).await })
    };
    drive().await;
    startup_terminal.emit_exit(1, None).await;
    assert_eq!(
        initializing
            .await
            .expect("initialization task")
            .expect_err("startup exit")
            .to_string(),
        "PTY shell exited during startup"
    );
    assert_eq!(
        startup.status(),
        TerminalSessionStatus::Exited {
            exit_code: Some(1),
            signal: None,
        }
    );

    let unknown_terminal = FakeTerminal::new();
    let unknown = make_session(&unknown_terminal);
    unknown_terminal.emit_exit(1, Some("SIG999")).await;
    assert_eq!(
        unknown.status(),
        TerminalSessionStatus::Exited {
            exit_code: None,
            signal: Some(ProcessSignal::new("SIG999")),
        }
    );
}

#[tokio::test(start_paused = true)]
async fn stale_readiness_probe_reschedules_a_successor_after_releasing_poll_ownership() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let old = session.start_send(send("", false)).expect("old send");
    drive().await;
    let stale_inspection = Arc::new(Completion::default());
    *terminal.inspect_gate.lock() = Some(stale_inspection.clone());
    advance(10).await;
    advance(90).await;
    assert_eq!(
        old.done().await.expect("old timeout").wait_reason,
        TerminalWaitReason::Timeout
    );

    let current = session.start_send(send("", false)).expect("current send");
    drive().await;
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    stale_inspection.resolve(Ok(Some(SubprocessTerminalForeground {
        process_group_id: ProcessGroupId::new(456),
        input_waiting: false,
    })));
    drive().await;
    advance(10).await;
    assert_eq!(
        current.done().await.expect("current readiness").wait_reason,
        TerminalWaitReason::StdinRead
    );
}

#[tokio::test(start_paused = true)]
async fn stale_probe_cannot_poll_successor_before_its_own_prewrite_phase_finishes() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let old = session.start_send(send("", false)).expect("old send");
    drive().await;
    let stale_inspection = Arc::new(Completion::default());
    *terminal.inspect_gate.lock() = Some(stale_inspection.clone());
    advance(10).await;
    advance(90).await;
    old.done().await.expect("old timeout");

    let successor_inspection = Arc::new(Completion::default());
    *terminal.inspect_gate.lock() = Some(successor_inspection.clone());
    let current = session
        .start_send(send("successor", true))
        .expect("successor");
    drive().await;
    stale_inspection.resolve(Ok(Some(SubprocessTerminalForeground {
        process_group_id: ProcessGroupId::new(456),
        input_waiting: false,
    })));
    drive().await;
    advance(10).await;
    assert!(terminal.writes.lock().is_empty());

    successor_inspection.resolve(Ok(Some(SubprocessTerminalForeground {
        process_group_id: ProcessGroupId::new(456),
        input_waiting: false,
    })));
    drive().await;
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    advance(10).await;
    assert_eq!(terminal.writes.lock().as_slice(), ["successor\r"]);
    assert_eq!(
        current.done().await.expect("current readiness").wait_reason,
        TerminalWaitReason::StdinRead
    );
}

#[tokio::test(start_paused = true)]
async fn readiness_failure_during_cancellation_cannot_release_signal_ownership() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let operation = session.start_send(send("first", true)).expect("send");
    drive().await;
    let readiness = Arc::new(Completion::default());
    *terminal.inspect_gate.lock() = Some(readiness.clone());
    advance(10).await;
    let signalling = Arc::new(Completion::default());
    *terminal.signal_gate.lock() = Some(signalling.clone());
    assert!(operation.cancel());
    readiness.resolve(Err("inspection failed during cancellation".to_owned()));
    drive().await;
    assert!(operation.done().now_or_never().is_none());
    assert!(
        session
            .start_send(send("successor", true))
            .expect_err("signal owns slot")
            .to_string()
            .contains("active send")
    );
    signalling.resolve(Ok(ProcessGroupId::new(456)));
    drive().await;
    session.close("test complete").await.expect("close");
    assert_eq!(
        operation.done().await.expect("close settles").wait_reason,
        TerminalWaitReason::SessionExit
    );
}

#[tokio::test(start_paused = true)]
async fn prompt_observed_during_prewrite_inspection_is_discarded() {
    let terminal = FakeTerminal::new();
    let session = make_session(&terminal);
    initialize(&session, &terminal).await;
    let inspection = Arc::new(Completion::default());
    *terminal.inspect_gate.lock() = Some(inspection.clone());
    let operation = session
        .start_send(send("long-running-command", true))
        .expect("send");
    drive().await;
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    inspection.resolve(Ok(Some(SubprocessTerminalForeground {
        process_group_id: ProcessGroupId::new(456),
        input_waiting: true,
    })));
    drive().await;
    advance(20).await;
    assert!(operation.done().now_or_never().is_none());
    terminal.emit_data("\x1b]133;D;0\x07seekdeep> ").await;
    advance(10).await;
    assert_eq!(
        operation.done().await.expect("new prompt").wait_reason,
        TerminalWaitReason::StdinRead
    );
}

#[tokio::test(start_paused = true)]
async fn line_and_utf8_caps_propagate_truncation_to_operation_and_reads() {
    let terminal = FakeTerminal::new();
    let mut bounded_config = config();
    bounded_config.scrollback_lines = 3;
    bounded_config.scrollback_max_bytes = 12;
    bounded_config.max_read_bytes = 6;
    let handle: SubprocessTerminalHandleRef = terminal.clone();
    let session = LocalPtySession::new(handle, bounded_config);
    initialize(&session, &terminal).await;
    terminal.set_foreground(None, false);
    let operation = session.start_send(send("", false)).expect("send");
    terminal.emit_data("一\n二\n三\n四").await;
    advance(60).await;
    assert!(operation.done().await.expect("idle").truncated);
    let page = session
        .read(TerminalReadRequest {
            offset: Some(0.0),
            count: Some(3.0),
        })
        .expect("page");
    assert!(page.text.len() <= 6);
    assert!(page.truncated);
    let beyond = session
        .read(TerminalReadRequest {
            offset: Some(999.0),
            count: None,
        })
        .expect("beyond");
    assert_eq!(
        (beyond.text, beyond.line_begin, beyond.line_end),
        (String::new(), 999, 999)
    );

    let tiny_terminal = FakeTerminal::new();
    let mut tiny_config = config();
    tiny_config.max_read_bytes = 1;
    let tiny_handle: SubprocessTerminalHandleRef = tiny_terminal.clone();
    let tiny = LocalPtySession::new(tiny_handle, tiny_config);
    initialize(&tiny, &tiny_terminal).await;
    tiny_terminal.set_foreground(None, false);
    let tiny_operation = tiny.start_send(send("", false)).expect("tiny send");
    tiny_terminal.emit_data("一").await;
    advance(60).await;
    tiny_operation.done().await.expect("tiny idle");
    assert_eq!(
        tiny.read(TerminalReadRequest {
            offset: Some(0.0),
            count: Some(1.0),
        })
        .expect("tiny read")
        .text,
        ""
    );
}

trait FutureNowOrNever: Sized {
    type Output;
    fn now_or_never(self) -> Option<Self::Output>;
}

impl<F> FutureNowOrNever for F
where
    F: std::future::Future,
{
    type Output = F::Output;

    fn now_or_never(self) -> Option<Self::Output> {
        futures::FutureExt::now_or_never(self)
    }
}
