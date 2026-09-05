//! Shared cancellation helpers for stdio host I/O and protocol phases.

use std::{error::Error, fmt, future::Future, sync::Arc};

use seekdeep_llm::AbortSignal;
use seekdeep_util::timeout::timeout_of;

#[derive(Debug)]
struct SharedAbortError(Arc<dyn Error + Send + Sync>);

impl fmt::Display for SharedAbortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for SharedAbortError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Builds the classified Error carried by an aborted query signal.
#[must_use]
pub fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    if let Some(timeout) = timeout_of(signal, None) {
        return anyhow::Error::new((*timeout).clone());
    }
    signal.error_reason().map_or_else(
        || anyhow::anyhow!("LSP query aborted"),
        |reason| anyhow::Error::new(SharedAbortError(reason)),
    )
}

/// Rejects an already-aborted optional query signal.
///
/// # Errors
///
/// Returns the signal's classified abort error.
pub fn throw_if_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if let Some(signal) = signal.filter(|signal| signal.is_aborted()) {
        return Err(abort_error(signal));
    }
    Ok(())
}

/// Awaits owned work while allowing a query signal to abandon only its wait.
///
/// The work runs in an owned task. Cancellation detaches that task rather than
/// aborting it, preserving the owner-defined quiescence boundary.
///
/// # Errors
///
/// Returns the work failure, task panic/cancellation, or classified query abort.
pub async fn abortable<T, Work>(work: Work, signal: Option<&AbortSignal>) -> anyhow::Result<T>
where
    T: Send + 'static,
    Work: Future<Output = anyhow::Result<T>> + Send + 'static,
{
    let mut task = tokio::spawn(work);
    let Some(signal) = signal else {
        return task.await.map_err(join_error)?;
    };
    if signal.is_aborted() {
        return Err(abort_error(signal));
    }
    tokio::select! {
        biased;
        result = &mut task => result.map_err(join_error)?,
        () = signal.cancelled() => Err(abort_error(signal)),
    }
}

fn join_error(error: tokio::task::JoinError) -> anyhow::Error {
    anyhow::Error::new(error).context("owned LSP work failed")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use seekdeep_util::timeout::deadline;
    use serde_json::json;
    use thiserror::Error;
    use tokio::sync::oneshot;

    use super::*;

    #[derive(Debug, Error)]
    #[error("caller stopped")]
    struct CallerStopped;

    async fn wait_for(flag: &AtomicBool) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !flag.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned work did not settle");
    }

    #[tokio::test]
    async fn absent_and_unfired_signals_return_owned_work() {
        assert_eq!(abortable(async { Ok(7) }, None).await.unwrap(), 7);
        let signal = AbortSignal::default();
        assert_eq!(
            abortable(async { Ok("done") }, Some(&signal))
                .await
                .unwrap(),
            "done"
        );
    }

    #[tokio::test]
    async fn cancellation_abandons_the_wait_but_owned_work_reaches_quiescence() {
        let signal = AbortSignal::default();
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let work_finished = finished.clone();
        let wait_signal = signal.clone();
        let wait = tokio::spawn(async move {
            abortable(
                async move {
                    let _ = started.send(());
                    let _ = release_rx.await;
                    work_finished.store(true, Ordering::Release);
                    Ok(())
                },
                Some(&wait_signal),
            )
            .await
        });
        started_rx.await.unwrap();
        signal.abort();
        assert_eq!(
            wait.await.unwrap().unwrap_err().to_string(),
            "LSP query aborted"
        );
        assert!(!finished.load(Ordering::Acquire));
        release.send(()).unwrap();
        wait_for(&finished).await;
    }

    #[tokio::test]
    async fn typed_error_reason_is_preserved_and_preabort_still_runs_owned_work() {
        let signal = AbortSignal::default();
        signal.abort_with_error(
            Arc::new(CallerStopped),
            json!({"message": "caller stopped"}),
        );
        assert_eq!(abort_error(&signal).to_string(), "caller stopped");
        assert!(throw_if_aborted(Some(&signal)).is_err());
        let ran = Arc::new(AtomicBool::new(false));
        let work_ran = ran.clone();
        assert!(
            abortable(
                async move {
                    work_ran.store(true, Ordering::Release);
                    Ok(())
                },
                Some(&signal),
            )
            .await
            .is_err()
        );
        wait_for(&ran).await;
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_reason_keeps_its_classification() {
        let timeout = deadline(None, 100.0, "LSP_TIMEOUT").unwrap();
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let error = abort_error(&timeout.signal);
        let reason = error.downcast_ref::<seekdeep_util::timeout::TimeoutReason>();
        assert_eq!(
            reason.map(|reason| reason.code.as_str()),
            Some("LSP_TIMEOUT")
        );
    }
}
