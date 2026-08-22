//! Exact-once fail-loud reporting and bounded release parity.

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_app_boot::{
    FAIL_LOUD_RELEASE_TIMEOUT, FailLoudController, FailLoudProcess, FailLoudRelease, FailLoudTimer,
};

#[derive(Debug, Default)]
struct RecordingProcess {
    written: Mutex<Vec<String>>,
    exits: Mutex<Vec<i32>>,
}

impl FailLoudProcess for RecordingProcess {
    fn write_stderr(&self, text: &str) {
        self.written.lock().push(text.to_owned());
    }

    fn exit(&self, code: i32) {
        self.exits.lock().push(code);
    }
}

#[derive(Debug, Default)]
struct ManualTimer {
    durations: Mutex<Vec<Duration>>,
    release: Arc<tokio::sync::Notify>,
}

impl FailLoudTimer for ManualTimer {
    fn wait(&self, duration: Duration) -> futures::future::BoxFuture<'static, ()> {
        self.durations.lock().push(duration);
        let release = self.release.clone();
        Box::pin(async move { release.notified().await })
    }
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn immediate_failure_writes_one_labelled_line_and_exits_one() {
    let process = Arc::new(RecordingProcess::default());
    let controller = FailLoudController::new(
        "seekdeep-test-bin",
        process.clone(),
        Arc::new(ManualTimer::default()),
        None,
    );
    assert!(controller.report_message("plain failure"));
    assert!(!controller.report_message("second failure"));
    assert_eq!(process.exits.lock().as_slice(), &[1]);
    assert_eq!(process.written.lock().len(), 1);
    assert_eq!(
        process.written.lock()[0],
        "seekdeep-test-bin: fatal load failure: plain failure\n"
    );
    assert!(controller.is_exiting());
}

#[tokio::test]
async fn release_finishes_before_exit_and_later_failures_are_swallowed() {
    let process = Arc::new(RecordingProcess::default());
    let timer = Arc::new(ManualTimer::default());
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let receiver = Arc::new(Mutex::new(Some(release_receiver)));
    let release: FailLoudRelease = Arc::new(move || {
        let receiver = receiver.lock().take().expect("release called once");
        Box::pin(async move {
            receiver.await?;
            Ok(())
        })
    });
    let controller =
        FailLoudController::new("seekdeep-test-bin", process.clone(), timer, Some(release));
    assert!(controller.report_message("first rejection"));
    assert!(!controller.report_message("second rejection"));
    settle().await;
    assert!(process.exits.lock().is_empty());
    assert_eq!(process.written.lock().len(), 1);
    release_sender.send(()).unwrap();
    settle().await;
    assert_eq!(process.exits.lock().as_slice(), &[1]);
}

#[tokio::test]
async fn rejecting_release_still_exits_and_never_settling_release_is_bounded() {
    let rejected_process = Arc::new(RecordingProcess::default());
    let rejected = FailLoudController::new(
        "seekdeep-test-bin",
        rejected_process.clone(),
        Arc::new(ManualTimer::default()),
        Some(Arc::new(|| {
            Box::pin(async { anyhow::bail!("release failed") })
        })),
    );
    rejected.report_message("boom");
    settle().await;
    assert_eq!(rejected_process.exits.lock().as_slice(), &[1]);

    let process = Arc::new(RecordingProcess::default());
    let timer = Arc::new(ManualTimer::default());
    let controller = FailLoudController::new(
        "seekdeep-test-bin",
        process.clone(),
        timer.clone(),
        Some(Arc::new(|| Box::pin(std::future::pending()))),
    );
    controller.report_message("timeout");
    settle().await;
    assert!(process.exits.lock().is_empty());
    assert_eq!(
        timer.durations.lock().as_slice(),
        &[FAIL_LOUD_RELEASE_TIMEOUT]
    );
    timer.release.notify_one();
    settle().await;
    assert_eq!(process.exits.lock().as_slice(), &[1]);
}

#[test]
fn error_reporting_preserves_the_causal_chain() {
    let process = Arc::new(RecordingProcess::default());
    let controller = FailLoudController::new(
        "seekdeep-test-bin",
        process.clone(),
        Arc::new(ManualTimer::default()),
        None,
    );
    let error = anyhow::anyhow!("outer").context("inner");
    controller.report_error(&error);
    let written = process.written.lock();
    assert!(written[0].contains("inner"));
    assert!(written[0].contains("outer"));
}
