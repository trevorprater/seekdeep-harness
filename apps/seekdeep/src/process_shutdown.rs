//! Bounded, escalating process shutdown for long-lived CLI surfaces.

use std::{
    fmt,
    future::{Future, IntoFuture},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use anyhow::Result;
use tokio::sync::{Notify, oneshot};

/// Maximum grace allowed for the application tree to dispose before forced exit.
pub const PROCESS_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;

/// Maximum grace allowed for the application tree to dispose before forced exit.
pub const PROCESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(PROCESS_SHUTDOWN_TIMEOUT_MS);

/// Exit code used when a supervisor requests ordinary shutdown with `SIGTERM`.
pub const SIGTERM_EXIT_CODE: i32 = 0;

/// Exit code used when a user interrupts the process with `SIGINT`.
pub const SIGINT_EXIT_CODE: i32 = 130;

type DisposeFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type Disposer = Box<dyn FnOnce() -> DisposeFuture + Send + 'static>;
type ExitHook = Arc<dyn Fn(i32) + Send + Sync + 'static>;
type Sleep = Arc<dyn Fn(Duration) -> SleepFuture + Send + Sync + 'static>;

/// A one-shot controller shared by normal completion and process signal handlers.
///
/// The first call starts disposal and fixes its exit code and completion mode.
/// Normal shutdown calls then coalesce. An interrupt while that operation exists
/// escalates immediately, including an interrupt after natural completion.
#[derive(Clone)]
pub struct ProcessShutdown {
    inner: Arc<Inner>,
}

impl fmt::Debug for ProcessShutdown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessShutdown")
            .field("timeout", &self.inner.timeout)
            .finish_non_exhaustive()
    }
}

/// A cloneable waiter for the controller's single disposal operation.
///
/// Dropping a waiter does not cancel disposal. Every waiter returned by one
/// controller observes the same operation, and can be awaited directly.
#[derive(Clone)]
#[must_use = "shutdown continues in the background; await this handle to observe disposal quiescence"]
pub struct ShutdownWait {
    pending: Arc<Pending>,
}

impl fmt::Debug for ShutdownWait {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShutdownWait")
            .field("finished", &self.pending.finished.load(Ordering::Acquire))
            .finish()
    }
}

impl ShutdownWait {
    /// Return whether two handles join the exact same shutdown operation.
    #[cfg(test)]
    pub(crate) fn same_operation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pending, &other.pending)
    }
}

impl IntoFuture for ShutdownWait {
    type Output = ();
    type IntoFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            self.pending.wait().await;
        })
    }
}

struct Inner {
    disposer: Mutex<Option<Disposer>>,
    force_exit: ExitHook,
    complete: ExitHook,
    sleep: Sleep,
    timeout: Duration,
    transition: Mutex<()>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pending: Option<Arc<Pending>>,
    timer_cancel: Option<oneshot::Sender<()>>,
    completed: bool,
    force_exited: bool,
}

struct Pending {
    finished: AtomicBool,
    notify: Notify,
}

struct CaughtDisposal {
    future: DisposeFuture,
}

enum DisposalOutcome {
    Returned(Result<()>),
    Panicked,
}

impl CaughtDisposal {
    fn new(disposer: Disposer) -> Self {
        Self {
            future: Box::pin(run_disposer(disposer)),
        }
    }
}

impl Future for CaughtDisposal {
    type Output = DisposalOutcome;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match catch_unwind(AssertUnwindSafe(|| self.future.as_mut().poll(context))) {
            Ok(Poll::Ready(result)) => Poll::Ready(DisposalOutcome::Returned(result)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(DisposalOutcome::Panicked),
        }
    }
}

struct FinishPending(Arc<Pending>);

impl Drop for FinishPending {
    fn drop(&mut self) {
        self.0.finish();
    }
}

impl Pending {
    fn new() -> Self {
        Self {
            finished: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn finish(&self) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

impl ProcessShutdown {
    /// Create a controller using the five-second production grace period.
    ///
    /// `force_exit` is the immediate-exit seam (normally
    /// `std::process::exit`). `complete` records a natural completion code for
    /// the Tokio main task to return after remaining handles drain.
    ///
    /// Starting the controller requires an active Tokio runtime.
    pub fn new<D, Disposal, ForceExit, ForceExitOutput, Complete>(
        dispose: D,
        force_exit: ForceExit,
        complete: Complete,
    ) -> Self
    where
        D: FnOnce() -> Disposal + Send + 'static,
        Disposal: Future<Output = Result<()>> + Send + 'static,
        ForceExit: Fn(i32) -> ForceExitOutput + Send + Sync + 'static,
        Complete: Fn(i32) + Send + Sync + 'static,
    {
        Self::with_timeout(dispose, force_exit, complete, PROCESS_SHUTDOWN_TIMEOUT)
    }

    /// Create a controller with a caller-supplied grace period.
    ///
    /// Starting the controller requires an active Tokio runtime.
    pub fn with_timeout<D, Disposal, ForceExit, ForceExitOutput, Complete>(
        dispose: D,
        force_exit: ForceExit,
        complete: Complete,
        timeout: Duration,
    ) -> Self
    where
        D: FnOnce() -> Disposal + Send + 'static,
        Disposal: Future<Output = Result<()>> + Send + 'static,
        ForceExit: Fn(i32) -> ForceExitOutput + Send + Sync + 'static,
        Complete: Fn(i32) + Send + Sync + 'static,
    {
        Self::with_clock(dispose, force_exit, complete, timeout, tokio::time::sleep)
    }

    /// Create a controller with caller-supplied grace and clock seams.
    ///
    /// The clock seam receives the grace duration exactly once when shutdown
    /// starts and returns a future that resolves at the forced-exit deadline.
    /// Starting the controller requires an active Tokio runtime.
    pub fn with_clock<D, Disposal, ForceExit, ForceExitOutput, Complete, Clock, SleepFutureType>(
        dispose: D,
        force_exit: ForceExit,
        complete: Complete,
        timeout: Duration,
        sleep: Clock,
    ) -> Self
    where
        D: FnOnce() -> Disposal + Send + 'static,
        Disposal: Future<Output = Result<()>> + Send + 'static,
        ForceExit: Fn(i32) -> ForceExitOutput + Send + Sync + 'static,
        Complete: Fn(i32) + Send + Sync + 'static,
        Clock: Fn(Duration) -> SleepFutureType + Send + Sync + 'static,
        SleepFutureType: Future<Output = ()> + Send + 'static,
    {
        Self {
            inner: Arc::new(Inner {
                disposer: Mutex::new(Some(Box::new(move || Box::pin(dispose())))),
                force_exit: Arc::new(move |code| drop(force_exit(code))),
                complete: Arc::new(complete),
                sleep: Arc::new(move |duration| Box::pin(sleep(duration))),
                timeout,
                transition: Mutex::new(()),
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Start or join graceful disposal before allowing natural completion.
    ///
    /// The first normal code wins. Repeated normal calls do not escalate.
    ///
    /// # Panics
    ///
    /// Panics when the first call is made without an active Tokio runtime and
    /// time driver. Calls that merely join an existing operation do not spawn.
    pub fn shutdown(&self, code: i32) -> ShutdownWait {
        self.start(code, false).wait
    }

    /// Start signal-driven disposal, or force exit if shutdown already exists.
    ///
    /// A first interrupt drains for the configured grace period and then forces
    /// exit even when disposal succeeds. Any later interrupt forces immediately
    /// with that interrupt's code.
    ///
    /// # Panics
    ///
    /// Panics when the first call is made without an active Tokio runtime and
    /// time driver. Escalating an existing operation does not spawn.
    pub fn interrupt(&self, code: i32) {
        let start = self.start(code, true);
        if !start.started {
            self.inner.force_exit_once(code);
        }
    }

    /// Handle the source-compatible `SIGTERM` policy (successful exit code 0).
    pub fn interrupt_sigterm(&self) {
        self.interrupt(SIGTERM_EXIT_CODE);
    }

    /// Handle the source-compatible `SIGINT` policy (exit code 130).
    pub fn interrupt_sigint(&self) {
        self.interrupt(SIGINT_EXIT_CODE);
    }

    fn start(&self, code: i32, force_after_dispose: bool) -> Start {
        let (wait, mut cancel_receiver, disposer) = {
            let mut state = lock(&self.inner.state);
            if let Some(pending) = &state.pending {
                return Start {
                    wait: ShutdownWait {
                        pending: Arc::clone(pending),
                    },
                    started: false,
                };
            }

            let pending = Arc::new(Pending::new());
            let (timer_cancel, cancel_receiver) = oneshot::channel();
            state.pending = Some(Arc::clone(&pending));
            state.timer_cancel = Some(timer_cancel);

            let disposer = lock(&self.inner.disposer)
                .take()
                .expect("the one-shot disposer must exist before shutdown starts");
            (ShutdownWait { pending }, cancel_receiver, disposer)
        };

        let completion_inner = Arc::clone(&self.inner);
        let sleep = (completion_inner.sleep)(completion_inner.timeout);
        let disposal = CaughtDisposal::new(disposer);
        let pending = Arc::clone(&wait.pending);
        drop(tokio::spawn(async move {
            let _finish_pending = FinishPending(pending);
            tokio::pin!(disposal);
            let result = tokio::select! {
                biased;
                result = &mut disposal => result,
                _ = &mut cancel_receiver => disposal.await,
                () = sleep => {
                    completion_inner.timeout_exit_once(code);
                    disposal.await
                }
            };

            match result {
                DisposalOutcome::Returned(Ok(())) if force_after_dispose => {
                    completion_inner.force_exit_once(code);
                }
                DisposalOutcome::Returned(Ok(())) => completion_inner.complete_once(code),
                DisposalOutcome::Returned(Err(_)) | DisposalOutcome::Panicked => {
                    completion_inner.force_exit_once(code);
                }
            }
        }));

        Start {
            wait,
            started: true,
        }
    }
}

struct Start {
    wait: ShutdownWait,
    started: bool,
}

impl Inner {
    fn timeout_exit_once(&self, code: i32) {
        let _transition = lock(&self.transition);
        let timer_cancel = {
            let mut state = lock(&self.state);
            if state.force_exited || state.completed {
                return;
            }
            state.force_exited = true;
            state.timer_cancel.take()
        };

        if let Some(timer_cancel) = timer_cancel {
            let _cancelled = timer_cancel.send(());
        }
        (self.force_exit)(code);
    }

    fn force_exit_once(&self, code: i32) {
        let _transition = lock(&self.transition);
        let timer_cancel = {
            let mut state = lock(&self.state);
            if state.force_exited {
                return;
            }
            state.force_exited = true;
            state.timer_cancel.take()
        };

        if let Some(timer_cancel) = timer_cancel {
            let _cancelled = timer_cancel.send(());
        }
        (self.force_exit)(code);
    }

    fn complete_once(&self, code: i32) {
        let _transition = lock(&self.transition);
        let timer_cancel = {
            let mut state = lock(&self.state);
            if state.completed || state.force_exited {
                return;
            }
            state.completed = true;
            state.timer_cancel.take()
        };

        if let Some(timer_cancel) = timer_cancel {
            let _cancelled = timer_cancel.send(());
        }
        (self.complete)(code);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn run_disposer(disposer: Disposer) -> Result<()> {
    disposer().await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Clone, Default)]
    struct ExitRecorder {
        forced: Arc<Mutex<Vec<i32>>>,
        completed: Arc<Mutex<Vec<i32>>>,
    }

    impl ExitRecorder {
        fn controller<D, Disposal>(&self, dispose: D) -> ProcessShutdown
        where
            D: FnOnce() -> Disposal + Send + 'static,
            Disposal: Future<Output = Result<()>> + Send + 'static,
        {
            let forced = self.clone();
            let completed = self.clone();
            ProcessShutdown::new(
                dispose,
                move |code| forced.record_force(code),
                move |code| completed.record_complete(code),
            )
        }

        fn record_force(&self, code: i32) {
            lock(&self.forced).push(code);
        }

        fn record_complete(&self, code: i32) {
            lock(&self.completed).push(code);
        }

        fn forced(&self) -> Vec<i32> {
            lock(&self.forced).clone()
        }

        fn completed(&self) -> Vec<i32> {
            lock(&self.completed).clone()
        }
    }

    #[derive(Clone, Default)]
    struct DeferredDispose {
        outcome: Arc<Mutex<Option<std::result::Result<(), String>>>>,
        notify: Arc<Notify>,
        calls: Arc<AtomicUsize>,
    }

    impl DeferredDispose {
        async fn wait(&self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            loop {
                let notified = self.notify.notified();
                if let Some(outcome) = lock(&self.outcome).take() {
                    return outcome.map_err(anyhow::Error::msg);
                }
                notified.await;
            }
        }

        fn resolve(&self) {
            *lock(&self.outcome) = Some(Ok(()));
            self.notify.notify_waiters();
        }

        fn reject(&self, message: &str) {
            *lock(&self.outcome) = Some(Err(message.to_owned()));
            self.notify.notify_waiters();
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    fn deferred_controller(disposal: &DeferredDispose, recorder: &ExitRecorder) -> ProcessShutdown {
        let disposal = disposal.clone();
        recorder.controller(move || async move { disposal.wait().await })
    }

    async fn settle_tasks() {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    #[test]
    fn constants_match_source_policy() {
        assert_eq!(PROCESS_SHUTDOWN_TIMEOUT, Duration::from_secs(5));
        assert_eq!(SIGTERM_EXIT_CODE, 0);
        assert_eq!(SIGINT_EXIT_CODE, 130);
    }

    #[tokio::test(start_paused = true)]
    async fn completes_naturally_after_disposal_resolves_and_forces_when_it_rejects() {
        let resolved_recorder = ExitRecorder::default();
        let resolved = resolved_recorder.controller(|| async { Ok(()) });

        resolved.shutdown(0).await;

        assert_eq!(resolved_recorder.completed(), vec![0]);
        assert!(resolved_recorder.forced().is_empty());

        let rejected_recorder = ExitRecorder::default();
        let rejected = rejected_recorder.controller(|| async {
            anyhow::bail!("dispose failed");
        });

        rejected.shutdown(1).await;

        assert_eq!(rejected_recorder.forced(), vec![1]);
        assert!(rejected_recorder.completed().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn natural_completion_reports_the_code_without_forcing_exit() {
        let recorder = ExitRecorder::default();
        let shutdown = recorder.controller(|| async { Ok(()) });

        shutdown.shutdown(7).await;

        assert_eq!(recorder.completed(), vec![7]);
        assert!(recorder.forced().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn forces_exit_when_graceful_disposal_reaches_the_default_bound() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let shutdown = deferred_controller(&disposal, &recorder);
        let pending = shutdown.shutdown(0);

        tokio::time::advance(
            PROCESS_SHUTDOWN_TIMEOUT
                .checked_sub(Duration::from_millis(1))
                .expect("the production shutdown bound exceeds one millisecond"),
        )
        .await;
        settle_tasks().await;
        assert!(recorder.forced().is_empty());

        tokio::time::advance(Duration::from_millis(1)).await;
        settle_tasks().await;
        assert_eq!(recorder.forced(), vec![0]);

        disposal.resolve();
        pending.await;
        assert_eq!(recorder.forced(), vec![0]);
        assert!(recorder.completed().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn honors_a_caller_supplied_grace_period() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let disposal_for_hook = disposal.clone();
        let forced = recorder.clone();
        let completed = recorder.clone();
        let shutdown = ProcessShutdown::with_timeout(
            move || async move { disposal_for_hook.wait().await },
            move |code| forced.record_force(code),
            move |code| completed.record_complete(code),
            Duration::from_millis(25),
        );
        let pending = shutdown.shutdown(0);

        tokio::time::advance(Duration::from_millis(24)).await;
        settle_tasks().await;
        assert!(recorder.forced().is_empty());

        tokio::time::advance(Duration::from_millis(1)).await;
        settle_tasks().await;
        assert_eq!(recorder.forced(), vec![0]);

        disposal.resolve();
        pending.await;
        assert_eq!(recorder.forced(), vec![0]);
        assert!(recorder.completed().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn uses_the_injected_clock_once_with_the_exact_grace_period() {
        let recorder = ExitRecorder::default();
        let observed_delays = Arc::new(Mutex::new(Vec::new()));
        let forced = recorder.clone();
        let completed = recorder.clone();
        let observed_delays_for_clock = Arc::clone(&observed_delays);
        let shutdown = ProcessShutdown::with_clock(
            || async { Ok(()) },
            move |code| forced.record_force(code),
            move |code| completed.record_complete(code),
            Duration::from_millis(37),
            move |duration| {
                lock(&observed_delays_for_clock).push(duration);
                tokio::time::sleep(duration)
            },
        );

        shutdown.shutdown(4).await;

        assert_eq!(*lock(&observed_delays), vec![Duration::from_millis(37)]);
        assert_eq!(recorder.completed(), vec![4]);
        assert!(recorder.forced().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn interrupt_forces_a_normal_shutdown_already_stuck_in_disposal() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let shutdown = deferred_controller(&disposal, &recorder);
        let pending = shutdown.shutdown(0);

        shutdown.interrupt(SIGINT_EXIT_CODE);
        assert_eq!(recorder.forced(), vec![SIGINT_EXIT_CODE]);

        disposal.resolve();
        pending.await;
        assert_eq!(recorder.forced(), vec![SIGINT_EXIT_CODE]);
        assert!(recorder.completed().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn forces_exit_after_disposal_started_by_a_signal() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let shutdown = deferred_controller(&disposal, &recorder);

        shutdown.interrupt(143);
        disposal.resolve();
        shutdown.shutdown(0).await;

        assert_eq!(recorder.forced(), vec![143]);
        assert!(recorder.completed().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn first_signal_drains_and_second_signal_forces() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let shutdown = deferred_controller(&disposal, &recorder);

        shutdown.interrupt(143);
        settle_tasks().await;
        assert_eq!(disposal.calls(), 1);
        assert!(recorder.forced().is_empty());

        shutdown.interrupt(SIGINT_EXIT_CODE);
        assert_eq!(recorder.forced(), vec![SIGINT_EXIT_CODE]);

        disposal.resolve();
        shutdown.shutdown(0).await;
        assert_eq!(recorder.forced(), vec![SIGINT_EXIT_CODE]);
    }

    #[tokio::test(start_paused = true)]
    async fn coalesces_normal_shutdown_calls_without_escalating() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let shutdown = deferred_controller(&disposal, &recorder);

        let first = shutdown.shutdown(0);
        let second = shutdown.shutdown(1);
        assert!(second.same_operation(&first));
        assert!(recorder.forced().is_empty());

        disposal.resolve();
        first.await;
        assert_eq!(recorder.completed(), vec![0]);
        assert!(recorder.forced().is_empty());
        assert_eq!(disposal.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn signal_forces_exit_after_natural_completion_starts_handle_drain() {
        let recorder = ExitRecorder::default();
        let shutdown = recorder.controller(|| async { Ok(()) });

        shutdown.shutdown(0).await;
        shutdown.interrupt_sigint();

        assert_eq!(recorder.completed(), vec![0]);
        assert_eq!(recorder.forced(), vec![SIGINT_EXIT_CODE]);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_timeout_cannot_force_after_completion_but_interrupt_can() {
        let recorder = ExitRecorder::default();
        let shutdown = recorder.controller(|| async { Ok(()) });

        shutdown.shutdown(5).await;
        shutdown.inner.timeout_exit_once(5);

        assert_eq!(recorder.completed(), vec![5]);
        assert!(recorder.forced().is_empty());

        shutdown.interrupt(SIGINT_EXIT_CODE);
        assert_eq!(recorder.completed(), vec![5]);
        assert_eq!(recorder.forced(), vec![SIGINT_EXIT_CODE]);
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_disposal_beats_a_zero_length_timeout() {
        let recorder = ExitRecorder::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_disposal = Arc::clone(&calls);
        let forced = recorder.clone();
        let completed = recorder.clone();
        let shutdown = ProcessShutdown::with_timeout(
            move || async move {
                calls_for_disposal.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            move |code| forced.record_force(code),
            move |code| completed.record_complete(code),
            Duration::ZERO,
        );

        shutdown.shutdown(6).await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.completed(), vec![6]);
        assert!(recorder.forced().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn signal_helpers_use_the_source_exit_codes() {
        let first_disposal = DeferredDispose::default();
        let first_recorder = ExitRecorder::default();
        let first = deferred_controller(&first_disposal, &first_recorder);
        first.interrupt_sigterm();
        first.interrupt_sigint();
        assert_eq!(first_recorder.forced(), vec![SIGINT_EXIT_CODE]);
        first_disposal.resolve();
        first.shutdown(99).await;

        let second_disposal = DeferredDispose::default();
        let second_recorder = ExitRecorder::default();
        let second = deferred_controller(&second_disposal, &second_recorder);
        second.interrupt_sigint();
        second.interrupt_sigterm();
        assert_eq!(second_recorder.forced(), vec![SIGTERM_EXIT_CODE]);
        second_disposal.resolve();
        second.shutdown(99).await;
    }

    #[tokio::test(start_paused = true)]
    async fn rejected_deferred_disposal_forces_the_first_code_once() {
        let disposal = DeferredDispose::default();
        let recorder = ExitRecorder::default();
        let shutdown = deferred_controller(&disposal, &recorder);
        let pending = shutdown.shutdown(23);

        disposal.reject("dispose failed");
        pending.await;

        assert_eq!(recorder.forced(), vec![23]);
        assert!(recorder.completed().is_empty());
    }
}
