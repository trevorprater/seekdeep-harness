//! Exact-once late-failure reporting with bounded terminal-owner release.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, future::BoxFuture};

/// Maximum wait for terminal/resource release before a fatal process exit.
pub const FAIL_LOUD_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

/// Process effects used by the fail-loud controller.
pub trait FailLoudProcess: Send + Sync + 'static {
    /// Writes the labelled fatal diagnostic.
    fn write_stderr(&self, text: &str);
    /// Commits process termination.
    fn exit(&self, code: i32);
}

/// Injectable timeout seam for deterministic release-bound tests.
pub trait FailLoudTimer: Send + Sync + 'static {
    /// Resolves after the requested duration.
    fn wait(&self, duration: Duration) -> BoxFuture<'static, ()>;
}

/// Production timer driven by Tokio's monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioFailLoudTimer;

impl FailLoudTimer for TokioFailLoudTimer {
    fn wait(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Optional teardown invoked before fatal exit.
pub type FailLoudRelease = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Latches the first late failure, reports it, releases resources, then exits.
pub struct FailLoudController {
    bin_name: String,
    process: Arc<dyn FailLoudProcess>,
    timer: Arc<dyn FailLoudTimer>,
    release: Option<FailLoudRelease>,
    exiting: AtomicBool,
}

impl std::fmt::Debug for FailLoudController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FailLoudController")
            .field("bin_name", &self.bin_name)
            .field("has_release", &self.release.is_some())
            .field("exiting", &self.exiting.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl FailLoudController {
    /// Builds one exact-once late-failure controller.
    #[must_use]
    pub fn new(
        bin_name: impl Into<String>,
        process: Arc<dyn FailLoudProcess>,
        timer: Arc<dyn FailLoudTimer>,
        release: Option<FailLoudRelease>,
    ) -> Arc<Self> {
        Arc::new(Self {
            bin_name: bin_name.into(),
            process,
            timer,
            release,
            exiting: AtomicBool::new(false),
        })
    }

    /// Reports an error with its complete causal chain.
    ///
    /// Returns whether this failure won the exact-once latch.
    pub fn report_error(self: &Arc<Self>, error: &anyhow::Error) -> bool {
        let rendered = catch_unwind(AssertUnwindSafe(|| format!("{error:#}")))
            .unwrap_or_else(|_| "[unrenderable failure]".to_owned());
        self.report_message(&rendered)
    }

    /// Reports an arbitrary thrown-value equivalent.
    ///
    /// Returns whether this failure won the exact-once latch.
    pub fn report_message(self: &Arc<Self>, message: &str) -> bool {
        if self.exiting.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.process.write_stderr(&format!(
            "{}: fatal load failure: {message}\n",
            self.bin_name
        ));
        let Some(release) = self.release.clone() else {
            self.process.exit(1);
            return true;
        };
        let process = self.process.clone();
        let timer = self.timer.clone();
        let task = async move {
            let release = catch_unwind(AssertUnwindSafe(|| release()));
            if let Ok(release) = release {
                let release = AssertUnwindSafe(release).catch_unwind();
                tokio::select! {
                    _ = release => {}
                    () = timer.wait(FAIL_LOUD_RELEASE_TIMEOUT) => {}
                }
            }
            process.exit(1);
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(task);
        } else {
            std::thread::spawn(move || futures::executor::block_on(task));
        }
        true
    }

    /// Whether one failure already owns process exit.
    #[must_use]
    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::Acquire)
    }
}
