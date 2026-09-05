//! Persistent shell session over the subprocess terminal primitive.

use std::{
    fmt,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::Shared};
use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessOutcome, SubprocessTerminalForeground, SubprocessTerminalHandleRef,
    SubprocessTerminalSignal,
};
use seekdeep_terminal::{
    TerminalBackendSession, TerminalError, TerminalErrorCode, TerminalFailure, TerminalReadRequest,
    TerminalReadResult, TerminalResult, TerminalSendOperation, TerminalSendOperationRef,
    TerminalSendRead, TerminalSendRequest, TerminalSendResult, TerminalSessionStatus,
    TerminalSignal, TerminalSignalResult, TerminalWaitReason, abort_failure,
};
use thiserror::Error;
use tokio::{io::AsyncReadExt as _, time::Instant};

use crate::{
    config::ResolvedTerminalBashConfig,
    sanitize::{CONTROLLED_PROMPT, TerminalSanitizer},
};

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
    fn complete(&self, value: T) {
        let mut stored = self.value.lock();
        if stored.is_none() {
            *stored = Some(value);
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

fn utf8_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut bytes = 0;
    let mut start = text.len();
    for (index, character) in text.char_indices().rev() {
        let next = character.len_utf8();
        if bytes + next > max_bytes {
            break;
        }
        bytes += next;
        start = index;
    }
    (text[start..].to_owned(), true)
}

#[derive(Debug)]
struct BoundedTextBuffer {
    value: String,
    dropped: bool,
    max_bytes: usize,
    max_lines: Option<usize>,
}

impl BoundedTextBuffer {
    fn new(max_bytes: usize, max_lines: Option<usize>) -> Self {
        Self {
            value: String::new(),
            dropped: false,
            max_bytes,
            max_lines,
        }
    }

    fn append(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.value.push_str(text);
        if let Some(max_lines) = self.max_lines {
            let line_count = self.value.split('\n').count();
            if line_count > max_lines {
                let keep_from = self
                    .value
                    .match_indices('\n')
                    .nth(line_count - max_lines - 1)
                    .map_or(0, |(index, _)| index + 1);
                self.value.drain(..keep_from);
                self.dropped = true;
            }
        }
        let (tail, truncated) = utf8_tail(&self.value, self.max_bytes);
        self.value = tail;
        self.dropped |= truncated;
    }

    fn consume(&mut self) -> TerminalSendRead {
        TerminalSendRead {
            delta: std::mem::take(&mut self.value),
            truncated: std::mem::take(&mut self.dropped),
        }
    }

    fn snapshot(&self) -> (&str, bool) {
        (&self.value, self.dropped)
    }
}

#[derive(Debug)]
struct SendState {
    finished: bool,
    cancellation_requested: bool,
    initial_foreground_left_wait: bool,
    initial_foreground_pgid: Option<seekdeep_subprocess::ProcessGroupId>,
}

#[derive(Debug)]
struct LocalSendOperation {
    id: u64,
    started_at: Instant,
    session: Weak<LocalPtySession>,
    output: Mutex<BoundedTextBuffer>,
    state: Mutex<SendState>,
    completion: Arc<Completion<TerminalResult<TerminalSendResult>>>,
}

impl LocalSendOperation {
    fn new(id: u64, max_bytes: usize, session: Weak<LocalPtySession>) -> Arc<Self> {
        Arc::new(Self {
            id,
            started_at: Instant::now(),
            session,
            output: Mutex::new(BoundedTextBuffer::new(max_bytes, None)),
            state: Mutex::new(SendState {
                finished: false,
                cancellation_requested: false,
                initial_foreground_left_wait: true,
                initial_foreground_pgid: None,
            }),
            completion: Arc::new(Completion::default()),
        })
    }

    fn is_settled(&self) -> bool {
        self.state.lock().finished
    }

    fn cancel_requested(&self) -> bool {
        self.state.lock().cancellation_requested
    }

    fn append(&self, text: &str) {
        if !self.state.lock().finished {
            self.output.lock().append(text);
        }
    }

    fn settle(
        &self,
        wait_reason: TerminalWaitReason,
        session_status: TerminalSessionStatus,
        inherited_truncation: bool,
    ) {
        {
            let mut state = self.state.lock();
            if state.finished {
                return;
            }
            state.finished = true;
        }
        let output = self.output.lock();
        let (viewport, truncated) = output.snapshot();
        self.completion.complete(Ok(TerminalSendResult {
            viewport: viewport.to_owned(),
            wait_reason,
            session_status,
            truncated: truncated || inherited_truncation,
        }));
    }

    fn fail(&self, error: TerminalFailure) {
        {
            let mut state = self.state.lock();
            if state.finished {
                return;
            }
            state.finished = true;
        }
        self.completion.complete(Err(error));
    }

    fn set_initial_foreground(&self, foreground: Option<&SubprocessTerminalForeground>) {
        let mut state = self.state.lock();
        state.initial_foreground_pgid = foreground.map(|value| value.process_group_id);
        state.initial_foreground_left_wait = foreground.is_none_or(|value| !value.input_waiting);
    }

    fn accepts_stdin_wait(
        &self,
        process_group_id: seekdeep_subprocess::ProcessGroupId,
        waiting: bool,
    ) -> bool {
        let mut state = self.state.lock();
        if Some(process_group_id) != state.initial_foreground_pgid {
            return waiting;
        }
        if !waiting {
            state.initial_foreground_left_wait = true;
        }
        waiting && state.initial_foreground_left_wait
    }
}

impl TerminalSendOperation for LocalSendOperation {
    fn done(&self) -> futures::future::BoxFuture<'static, TerminalResult<TerminalSendResult>> {
        let completion = self.completion.clone();
        async move { completion.wait().await }.boxed()
    }

    fn read_output(&self) -> TerminalSendRead {
        self.output.lock().consume()
    }

    fn cancel(&self) -> bool {
        {
            let mut state = self.state.lock();
            if state.finished {
                return false;
            }
            state.cancellation_requested = true;
        }
        if let Some(session) = self.session.upgrade() {
            session.interrupt(self.id);
        }
        true
    }
}

type WriteFuture = Shared<futures::future::BoxFuture<'static, TerminalResult<()>>>;

#[allow(clippy::struct_excessive_bools)] // Independent source readiness evidence is intentional.
struct SessionState {
    motd: String,
    sanitizer: TerminalSanitizer,
    scrollback: BoundedTextBuffer,
    status: TerminalSessionStatus,
    active: Option<Arc<LocalSendOperation>>,
    active_timer: Option<(u64, tokio::task::AbortHandle)>,
    active_deadline_timer: Option<tokio::task::AbortHandle>,
    active_abort: Option<tokio::task::AbortHandle>,
    interrupting: Option<u64>,
    active_write: Option<(u64, WriteFuture)>,
    polling_ready: Option<u64>,
    polling: bool,
    prompt_seen: bool,
    prompt_text_seen: bool,
    prompt_tail: String,
    shell_pgid: Option<seekdeep_subprocess::ProcessGroupId>,
    initializing: bool,
    last_output_at: Instant,
    closing: bool,
    close_fence: Option<Arc<CloseFence>>,
    transport_failure: Option<TerminalFailure>,
}

type CloseFuture = Shared<futures::future::BoxFuture<'static, TerminalResult<()>>>;

struct CloseFence(CloseFuture);

impl CloseFence {
    async fn wait(&self) -> TerminalResult<()> {
        self.0.clone().await
    }
}

/// Backend session wrapping one provider-owned terminal process.
pub struct LocalPtySession {
    weak_self: Weak<Self>,
    terminal: SubprocessTerminalHandleRef,
    config: ResolvedTerminalBashConfig,
    next_send_id: AtomicU64,
    next_timer_id: AtomicU64,
    state: Mutex<SessionState>,
    output_ended: Arc<Completion<()>>,
    terminal_completion: Arc<Completion<()>>,
    watchers_started: AtomicBool,
}

impl fmt::Debug for LocalPtySession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("LocalPtySession")
            .field("pid", &self.terminal.pid())
            .field("status", &state.status)
            .field("active", &state.active.as_ref().map(|active| active.id))
            .field("closing", &state.closing)
            .finish_non_exhaustive()
    }
}

impl LocalPtySession {
    /// Creates one session and starts its sole output and completion observers.
    #[must_use]
    pub fn new(
        terminal: SubprocessTerminalHandleRef,
        config: ResolvedTerminalBashConfig,
    ) -> Arc<Self> {
        let max_read_bytes = config.max_read_bytes;
        let scrollback_max_bytes = config.scrollback_max_bytes;
        let scrollback_lines = config.scrollback_lines;
        let session = Arc::new_cyclic(|weak_self| Self {
            weak_self: weak_self.clone(),
            terminal,
            config,
            next_send_id: AtomicU64::new(0),
            next_timer_id: AtomicU64::new(0),
            state: Mutex::new(SessionState {
                motd: String::new(),
                sanitizer: TerminalSanitizer::new(max_read_bytes),
                scrollback: BoundedTextBuffer::new(scrollback_max_bytes, Some(scrollback_lines)),
                status: TerminalSessionStatus::Running,
                active: None,
                active_timer: None,
                active_deadline_timer: None,
                active_abort: None,
                interrupting: None,
                active_write: None,
                polling_ready: None,
                polling: false,
                prompt_seen: false,
                prompt_text_seen: false,
                prompt_tail: String::new(),
                shell_pgid: None,
                initializing: false,
                last_output_at: Instant::now(),
                closing: false,
                close_fence: None,
                transport_failure: None,
            }),
            output_ended: Arc::new(Completion::default()),
            terminal_completion: Arc::new(Completion::default()),
            watchers_started: AtomicBool::new(false),
        });
        session.start_watchers();
        session
    }

    fn start_watchers(self: &Arc<Self>) {
        if self.watchers_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let output = self.terminal.output();
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut decoder = Utf8StreamDecoder::default();
            let mut buffer = [0_u8; 8_192];
            loop {
                let read = {
                    let mut output = output.lock().await;
                    output.read(&mut buffer).await
                };
                match read {
                    Ok(0) => {
                        if let Some(session) = weak.upgrade() {
                            let tail = decoder.finish();
                            session.on_data(&tail);
                            let flushed = session.state.lock().sanitizer.flush();
                            session.append_output(&flushed);
                            session.output_ended.complete(());
                        }
                        break;
                    }
                    Ok(count) => {
                        if let Some(session) = weak.upgrade() {
                            session.on_data(&decoder.decode(&buffer[..count]));
                        } else {
                            break;
                        }
                    }
                    Err(error) => {
                        if let Some(session) = weak.upgrade() {
                            session
                                .on_transport_failure(TerminalFailure::message(error.to_string()));
                            session.output_ended.complete(());
                        }
                        break;
                    }
                }
            }
        });

        let terminal = self.terminal.clone();
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            if let Some(session) = weak.upgrade() {
                match terminal.done().await {
                    Ok(outcome) => session.on_exit(outcome).await,
                    Err(error) => {
                        session.on_transport_failure(TerminalFailure::message(error.to_string()));
                    }
                }
                session.terminal_completion.complete(());
            }
        });
    }

    /// Captures startup output through the ordinary readiness contract.
    ///
    /// # Errors
    ///
    /// Rejects cancellation, startup exit, timeout, or terminal failures.
    pub async fn initialize(&self, signal: Option<AbortSignal>) -> TerminalResult<()> {
        self.state.lock().initializing = true;
        let operation = self.start_send_internal(TerminalSendRequest {
            text: String::new(),
            submit: false,
            signal: signal.clone(),
        });
        let result = match operation {
            Ok(operation) => operation
                .done()
                .await
                .and_then(|result| match result.wait_reason {
                    TerminalWaitReason::SessionExit => {
                        Err(TerminalFailure::message("PTY shell exited during startup"))
                    }
                    TerminalWaitReason::Timeout => Err(TerminalFailure::message(
                        "PTY shell did not reach readiness before startup timeout",
                    )),
                    TerminalWaitReason::StdinRead | TerminalWaitReason::InferredIdle => {
                        self.state.lock().motd = result.viewport;
                        Ok(())
                    }
                }),
            Err(error) => Err(error),
        };
        self.state.lock().initializing = false;
        if let Some(signal) = signal
            && signal.is_aborted()
        {
            return Err(abort_failure(&signal));
        }
        result
    }

    fn start_send_internal(
        &self,
        request: TerminalSendRequest,
    ) -> TerminalResult<TerminalSendOperationRef> {
        let id = self.next_send_id.fetch_add(1, Ordering::AcqRel) + 1;
        let operation =
            LocalSendOperation::new(id, self.config.max_read_bytes, self.weak_self.clone());
        {
            let mut state = self.state.lock();
            if state.closing {
                return Err(TerminalFailure::message("PTY session is closing"));
            }
            if matches!(state.status, TerminalSessionStatus::Exited { .. }) {
                return Err(TerminalFailure::message("PTY session has exited"));
            }
            if state.active.is_some() {
                let draining = if state.active_write.is_some() {
                    " or draining provider write"
                } else if state.interrupting.is_some() {
                    " or draining foreground interrupt"
                } else {
                    ""
                };
                return Err(TerminalFailure::new(TerminalError::new(
                    format!("PTY session already has an active send{draining}"),
                    TerminalErrorCode::SendActive,
                )));
            }
            if request.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                return Err(TerminalFailure::message("PTY send aborted before write"));
            }
            state.active = Some(operation.clone());
            Self::reset_readiness_evidence(&mut state);
        }

        if let Some(signal) = request.signal.clone() {
            let operation_for_abort = operation.clone();
            let task = tokio::spawn(async move {
                tokio::select! {
                    () = signal.cancelled() => { operation_for_abort.cancel(); }
                    _ = operation_for_abort.done() => {}
                }
            });
            self.install_active_abort(id, task.abort_handle());
        }
        let weak = self.weak_self.clone();
        let timeout = self.config.timeout_ms;
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout)).await;
            if let Some(session) = weak.upgrade() {
                session.deadline_fired(id);
            }
        });
        self.install_deadline(id, task.abort_handle());

        if let Some(session) = self.weak_self.upgrade() {
            let operation_for_send = operation.clone();
            tokio::spawn(async move {
                session.begin_send(operation_for_send, request).await;
            });
        }
        Ok(operation)
    }

    fn install_active_abort(&self, id: u64, handle: tokio::task::AbortHandle) {
        let mut state = self.state.lock();
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            state.active_abort = Some(handle);
        } else {
            handle.abort();
        }
    }

    fn install_deadline(&self, id: u64, handle: tokio::task::AbortHandle) {
        let mut state = self.state.lock();
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            state.active_deadline_timer = Some(handle);
        } else {
            handle.abort();
        }
    }

    #[allow(clippy::too_many_lines)] // One ordered owner mirrors the source send lifecycle.
    async fn begin_send(
        self: Arc<Self>,
        operation: Arc<LocalSendOperation>,
        request: TerminalSendRequest,
    ) {
        let foreground = match self.terminal.inspect_foreground().await {
            Ok(foreground) => foreground,
            Err(error) => {
                let should_fail = {
                    let state = self.state.lock();
                    state
                        .active
                        .as_ref()
                        .is_some_and(|active| active.id == operation.id)
                        && !state.closing
                        && state.interrupting != Some(operation.id)
                };
                if should_fail {
                    self.fail_active(TerminalFailure::message(error.to_string()));
                }
                return;
            }
        };
        {
            let state = self.state.lock();
            if state
                .active
                .as_ref()
                .is_none_or(|active| active.id != operation.id)
                || state.closing
                || state.interrupting == Some(operation.id)
            {
                return;
            }
        }
        operation.set_initial_foreground(foreground.as_ref());
        let input = format!("{}{}", request.text, if request.submit { "\r" } else { "" });
        if !input.is_empty() && !operation.cancel_requested() {
            {
                let mut state = self.state.lock();
                Self::reset_readiness_evidence(&mut state);
            }
            let terminal = self.terminal.clone();
            let write_input = input.clone();
            let write: WriteFuture = async move {
                terminal
                    .write(&write_input)
                    .await
                    .map_err(|error| TerminalFailure::message(error.to_string()))
            }
            .boxed()
            .shared();
            {
                let mut state = self.state.lock();
                state.active_write = Some((operation.id, write.clone()));
            }
            let result = write.await;
            {
                let mut state = self.state.lock();
                if state
                    .active_write
                    .as_ref()
                    .is_some_and(|(id, _)| *id == operation.id)
                {
                    state.active_write = None;
                }
            }
            if let Err(error) = result {
                let should_handle = {
                    let state = self.state.lock();
                    state
                        .active
                        .as_ref()
                        .is_some_and(|active| active.id == operation.id)
                        && !state.closing
                };
                if should_handle {
                    if operation.is_settled() {
                        self.clear_active();
                    } else {
                        self.fail_active(error);
                    }
                }
                return;
            }
        }
        if operation.cancel_requested() {
            return;
        }
        let action = {
            let mut state = self.state.lock();
            if state
                .active
                .as_ref()
                .is_none_or(|active| active.id != operation.id)
            {
                0
            } else if operation.is_settled() {
                1
            } else if !state.closing {
                state.polling_ready = Some(operation.id);
                2
            } else {
                0
            }
        };
        match action {
            1 => self.clear_active(),
            2 => self.schedule_poll(operation.id, self.config.poll_interval_ms),
            _ => {}
        }
    }

    fn reset_readiness_evidence(state: &mut SessionState) {
        state.last_output_at = Instant::now();
        state.prompt_seen = false;
        state.prompt_text_seen = false;
        state.prompt_tail.clear();
    }

    fn deadline_fired(&self, operation_id: u64) {
        let retain = {
            let state = self.state.lock();
            if state
                .active
                .as_ref()
                .is_none_or(|active| active.id != operation_id)
            {
                return;
            }
            state
                .active_write
                .as_ref()
                .is_some_and(|(id, _)| *id == operation_id)
                || state.interrupting == Some(operation_id)
        };
        self.settle_active(TerminalWaitReason::Timeout, retain);
    }

    fn schedule_poll(&self, operation_id: u64, delay_ms: u64) {
        let mut state = self.state.lock();
        if state
            .active
            .as_ref()
            .is_none_or(|active| active.id != operation_id)
            || state.interrupting == Some(operation_id)
            || state.polling
        {
            return;
        }
        if let Some((_, timer)) = state.active_timer.take() {
            timer.abort();
        }
        let timer_id = self.next_timer_id.fetch_add(1, Ordering::AcqRel) + 1;
        let weak = self.weak_self.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            if let Some(session) = weak.upgrade()
                && session.take_readiness_timer(timer_id)
            {
                session.poll_readiness(operation_id).await;
            }
        });
        state.active_timer = Some((timer_id, task.abort_handle()));
    }

    fn take_readiness_timer(&self, timer_id: u64) -> bool {
        let mut state = self.state.lock();
        if state
            .active_timer
            .as_ref()
            .is_some_and(|(current, _)| *current == timer_id)
        {
            state.active_timer = None;
            true
        } else {
            false
        }
    }

    async fn poll_readiness(self: Arc<Self>, operation_id: u64) {
        let operation = {
            let mut state = self.state.lock();
            let Some(operation) = state.active.clone() else {
                return;
            };
            if operation.id != operation_id || state.polling {
                return;
            }
            state.polling = true;
            operation
        };
        let inspection = self.terminal.inspect_foreground().await;
        let mut settle = None;
        let mut failure = None;
        match inspection {
            Ok(foreground) => {
                let mut state = self.state.lock();
                if state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == operation_id)
                    && !state.closing
                    && state.interrupting != Some(operation_id)
                {
                    let idle_for = duration_millis(
                        Instant::now().saturating_duration_since(state.last_output_at),
                    );
                    if state.prompt_seen && state.shell_pgid.is_none() {
                        state.shell_pgid = foreground.as_ref().map(|value| value.process_group_id);
                    }
                    if state.prompt_seen
                        && state.prompt_text_seen
                        && idle_for >= self.config.poll_interval_ms
                        && foreground.as_ref().map(|value| value.process_group_id)
                            == state.shell_pgid
                    {
                        settle = Some(TerminalWaitReason::StdinRead);
                    } else {
                        let elapsed = duration_millis(
                            Instant::now().saturating_duration_since(operation.started_at),
                        );
                        let startup_has_output =
                            !state.initializing || !state.scrollback.snapshot().0.is_empty();
                        let accepts_stdin_wait = startup_has_output
                            && foreground.as_ref().is_some_and(|value| {
                                operation
                                    .accepts_stdin_wait(value.process_group_id, value.input_waiting)
                            });
                        if elapsed >= self.config.exact_probe_after_ms && accepts_stdin_wait {
                            settle = Some(TerminalWaitReason::StdinRead);
                        } else {
                            let handoff_grace = if state.prompt_seen {
                                self.config.handoff_grace_ms
                            } else {
                                0
                            };
                            if startup_has_output
                                && idle_for >= self.config.idle_silence_ms + handoff_grace
                            {
                                settle = Some(TerminalWaitReason::InferredIdle);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let state = self.state.lock();
                if state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == operation_id)
                    && !state.closing
                    && state.interrupting != Some(operation_id)
                {
                    failure = Some(TerminalFailure::message(error.to_string()));
                }
            }
        }
        if let Some(reason) = settle {
            self.settle_active(reason, false);
        } else if let Some(error) = failure {
            self.fail_active(error);
        }
        let reschedule = {
            let mut state = self.state.lock();
            state.polling = false;
            state
                .active
                .as_ref()
                .and_then(|active| (state.polling_ready == Some(active.id)).then_some(active.id))
        };
        if let Some(active_id) = reschedule {
            self.schedule_poll(active_id, self.config.poll_interval_ms);
        }
    }

    fn interrupt(&self, operation_id: u64) {
        {
            let mut state = self.state.lock();
            if state
                .active
                .as_ref()
                .is_none_or(|active| active.id != operation_id)
            {
                return;
            }
            state.interrupting = Some(operation_id);
            Self::stop_readiness_polling(&mut state);
        }
        if let Some(session) = self.weak_self.upgrade() {
            tokio::spawn(async move {
                session.interrupt_once(operation_id).await;
            });
        }
    }

    async fn interrupt_once(self: Arc<Self>, operation_id: u64) {
        let active_write = self
            .state
            .lock()
            .active_write
            .as_ref()
            .and_then(|(id, write)| (*id == operation_id).then(|| write.clone()));
        if let Some(write) = active_write
            && write.await.is_err()
        {
            let mut state = self.state.lock();
            if state.interrupting == Some(operation_id) {
                state.interrupting = None;
            }
            return;
        }
        let signal_result = self
            .terminal
            .signal_foreground(SubprocessTerminalSignal::Sigint)
            .await;
        {
            let mut state = self.state.lock();
            if state.interrupting == Some(operation_id) {
                state.interrupting = None;
            }
        }
        if let Err(error) = signal_result {
            let should_fail = {
                let state = self.state.lock();
                state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == operation_id)
                    && !state.closing
            };
            if should_fail {
                self.on_transport_failure(TerminalFailure::message(error.to_string()));
            }
            return;
        }
        let action = {
            let mut state = self.state.lock();
            let Some(operation) = state
                .active
                .as_ref()
                .filter(|active| active.id == operation_id)
                .cloned()
            else {
                return;
            };
            if operation.is_settled() {
                1
            } else if !state.closing {
                state.polling_ready = Some(operation_id);
                2
            } else {
                0
            }
        };
        match action {
            1 => self.clear_active(),
            2 => self.schedule_poll(operation_id, 0),
            _ => {}
        }
    }

    fn settle_active(&self, reason: TerminalWaitReason, retain_ownership: bool) {
        let (operation, status, truncated) = {
            let mut state = self.state.lock();
            let Some(operation) = state.active.clone() else {
                return;
            };
            let status = state.status.clone();
            let truncated = state.scrollback.snapshot().1;
            if retain_ownership {
                Self::stop_polling(&mut state);
                if let Some(abort) = state.active_abort.take() {
                    abort.abort();
                }
            } else {
                Self::clear_active_state(&mut state);
            }
            (operation, status, truncated)
        };
        operation.settle(reason, status, truncated);
    }

    fn clear_active(&self) {
        Self::clear_active_state(&mut self.state.lock());
    }

    fn clear_active_state(state: &mut SessionState) {
        let operation_id = state.active.as_ref().map(|active| active.id);
        Self::stop_polling(state);
        if let Some(abort) = state.active_abort.take() {
            abort.abort();
        }
        if state.interrupting == operation_id {
            state.interrupting = None;
        }
        state.polling_ready = None;
        state.active = None;
        state.active_write = None;
    }

    fn stop_polling(state: &mut SessionState) {
        Self::stop_readiness_polling(state);
        if let Some(timer) = state.active_deadline_timer.take() {
            timer.abort();
        }
    }

    fn stop_readiness_polling(state: &mut SessionState) {
        if let Some((_, timer)) = state.active_timer.take() {
            timer.abort();
        }
        state.polling_ready = None;
    }

    fn fail_active(&self, error: TerminalFailure) {
        let operation = {
            let mut state = self.state.lock();
            let operation = state.active.clone();
            Self::clear_active_state(&mut state);
            operation
        };
        if let Some(operation) = operation {
            operation.fail(error);
        }
    }

    fn on_data(&self, data: &str) {
        let (text, active) = {
            let mut state = self.state.lock();
            let sanitized = state.sanitizer.push(data);
            if !sanitized.text.is_empty() {
                state.last_output_at = Instant::now();
                state.scrollback.append(&sanitized.text);
            }
            if sanitized.prompt {
                state.prompt_seen = true;
                state.prompt_tail.clear();
                state.last_output_at = Instant::now();
            }
            if state.prompt_seen
                && let Some(prompt_tail) = sanitized.prompt_tail
            {
                let remaining = (CONTROLLED_PROMPT.chars().count() + 1)
                    .saturating_sub(state.prompt_tail.chars().count());
                state
                    .prompt_tail
                    .extend(prompt_tail.chars().take(remaining));
                if prompt_tail.chars().count() > remaining {
                    state.prompt_tail = format!("{CONTROLLED_PROMPT}\0");
                }
                state.prompt_text_seen = state.prompt_tail == CONTROLLED_PROMPT;
            }
            (sanitized.text, state.active.clone())
        };
        if let Some(active) = active {
            active.append(&text);
        }
    }

    fn append_output(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let active = {
            let mut state = self.state.lock();
            state.last_output_at = Instant::now();
            state.scrollback.append(text);
            state.active.clone()
        };
        if let Some(active) = active {
            active.append(text);
        }
    }

    async fn on_exit(&self, outcome: SubprocessOutcome) {
        self.output_ended.wait().await;
        {
            let mut state = self.state.lock();
            if state.transport_failure.is_some() {
                return;
            }
            state.status = TerminalSessionStatus::Exited {
                exit_code: outcome.exit_code,
                signal: outcome.signal,
            };
        }
        self.settle_active(TerminalWaitReason::SessionExit, false);
    }

    fn on_transport_failure(&self, error: TerminalFailure) {
        let failure = {
            let mut state = self.state.lock();
            if state.transport_failure.is_none() {
                state.transport_failure = Some(error);
            }
            state.status = TerminalSessionStatus::Exited {
                exit_code: None,
                signal: None,
            };
            state.transport_failure.clone().expect("stored failure")
        };
        self.fail_active(failure);
        let terminal = self.terminal.clone();
        tokio::spawn(async move {
            let _ = terminal.terminate().await;
        });
    }

    async fn close_once(self: Arc<Self>, reason: String) -> TerminalResult<()> {
        {
            let mut state = self.state.lock();
            Self::stop_polling(&mut state);
        }
        if let Err(error) = self.terminal.terminate().await {
            return Err(TerminalFailure::new(PtyCleanupError {
                reason,
                source: TerminalFailure::message(error.to_string()),
            }));
        }
        self.settle_active(TerminalWaitReason::SessionExit, false);
        self.terminal_completion.wait().await;
        if let Some(failure) = self.state.lock().transport_failure.clone() {
            return Err(failure);
        }
        Ok(())
    }

    fn read_internal(&self, request: &TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        let offset = validate_read_number(
            request.offset.unwrap_or(0.0),
            true,
            "PTY read offset must be a non-negative safe integer",
        )?;
        let count = validate_read_number(
            request.count.unwrap_or(500.0),
            false,
            "PTY read count must be a positive safe integer",
        )?;
        let state = self.state.lock();
        let (text, snapshot_truncated) = state.scrollback.snapshot();
        let lines = text.split('\n').collect::<Vec<_>>();
        let total_lines = if text.is_empty() { 0 } else { lines.len() };
        if offset >= total_lines {
            return Ok(TerminalReadResult {
                text: String::new(),
                total_lines: total_lines as u64,
                line_begin: offset as u64,
                line_end: offset as u64,
                truncated: snapshot_truncated,
            });
        }
        let end = total_lines - offset;
        let start = end.saturating_sub(count);
        let requested = lines[start..end].join("\n");
        let (bounded, bounded_truncated) = utf8_tail(&requested, self.config.max_read_bytes);
        let returned_lines = if bounded.is_empty() {
            0
        } else {
            bounded.split('\n').count()
        };
        Ok(TerminalReadResult {
            text: bounded,
            total_lines: total_lines as u64,
            line_begin: offset as u64,
            line_end: (offset + returned_lines) as u64,
            truncated: snapshot_truncated || bounded_truncated,
        })
    }
}

#[async_trait]
impl TerminalBackendSession for LocalPtySession {
    fn motd(&self) -> String {
        self.state.lock().motd.clone()
    }

    fn pid(&self) -> Option<seekdeep_subprocess::ProcessId> {
        Some(self.terminal.pid())
    }

    fn start_send(&self, request: TerminalSendRequest) -> TerminalResult<TerminalSendOperationRef> {
        self.start_send_internal(request)
    }

    fn read(&self, request: TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        self.read_internal(&request)
    }

    async fn signal(&self, signal: TerminalSignal) -> TerminalResult<TerminalSignalResult> {
        if self.state.lock().closing {
            return Err(TerminalFailure::message("PTY session is closing"));
        }
        let signal = match signal {
            TerminalSignal::SIGINT => SubprocessTerminalSignal::Sigint,
            TerminalSignal::SIGTERM => SubprocessTerminalSignal::Sigterm,
            TerminalSignal::SIGKILL => SubprocessTerminalSignal::Sigkill,
            TerminalSignal::SIGTSTP => SubprocessTerminalSignal::Sigtstp,
            TerminalSignal::SIGHUP => SubprocessTerminalSignal::Sighup,
        };
        let target = self
            .terminal
            .signal_foreground(signal)
            .await
            .map_err(|error| TerminalFailure::message(error.to_string()))?;
        Ok(TerminalSignalResult::delivered(target))
    }

    fn status(&self) -> TerminalSessionStatus {
        self.state.lock().status.clone()
    }

    async fn close(&self, reason: &str) -> TerminalResult<()> {
        let fence = {
            let mut state = self.state.lock();
            state.closing = true;
            if let Some(fence) = &state.close_fence {
                fence.clone()
            } else {
                let Some(session) = self.weak_self.upgrade() else {
                    return Err(TerminalFailure::message("PTY session is unavailable"));
                };
                let reason = reason.to_owned();
                let future = async move { session.close_once(reason).await }
                    .boxed()
                    .shared();
                let fence = Arc::new(CloseFence(future));
                state.close_fence = Some(fence.clone());
                fence
            }
        };
        let result = fence.wait().await;
        if let Err(error) = result {
            let mut state = self.state.lock();
            if state
                .close_fence
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &fence))
            {
                state.close_fence = None;
            }
            drop(state);
            self.fail_active(error.clone());
            Err(error)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Error)]
#[error("PTY cleanup failed ({reason})")]
struct PtyCleanupError {
    reason: String,
    #[source]
    source: TerminalFailure,
}

fn validate_read_number(value: f64, zero_allowed: bool, message: &str) -> TerminalResult<usize> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !value.is_finite()
        || value.fract() != 0.0
        || value > MAX_SAFE_INTEGER
        || if zero_allowed {
            value < 0.0
        } else {
            value <= 0.0
        }
    {
        return Err(TerminalFailure::message(message));
    }
    usize::try_from(checked_read_integer(value)).map_err(|_| TerminalFailure::message(message))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn checked_read_integer(value: f64) -> u64 {
    // `validate_read_number` proves this is a non-negative safe integer.
    value as u64
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn decode(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);
        let split = complete_utf8_prefix(&bytes);
        self.pending.extend_from_slice(&bytes[split..]);
        String::from_utf8_lossy(&bytes[..split]).into_owned()
    }

    fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned()
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
