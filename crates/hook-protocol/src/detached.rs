//! Quiescence tracking for emit-shaped hook runs that no extension point awaits.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::FutureExt;
use seekdeep_llm::AbortSignal;
use serde_json::json;
use tokio::sync::Notify;

/// In-flight registry for one bridge's detached hook runs.
#[derive(Clone, Debug)]
pub struct DetachedRuns {
    /// The abort signal every tracked run must hand to `run_hook`.
    pub signal: AbortSignal,
    pending: Arc<AtomicUsize>,
    notify: Arc<Notify>,
}

impl DetachedRuns {
    /// Registers one detached run until it settles.
    ///
    /// The run is driven to completion eagerly; a panicking run is absorbed by
    /// the settlement bookkeeping and never leaves the registry dangling.
    pub fn track<F>(&self, run: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let pending = Arc::clone(&self.pending);
        let notify = Arc::clone(&self.notify);
        let run = std::panic::AssertUnwindSafe(run).catch_unwind();
        tokio::spawn(async move {
            let _ = run.await;
            if pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                notify.notify_waiters();
            }
        });
    }

    /// Aborts the shared signal, then resolves once every tracked run settles.
    pub async fn drain(&self) {
        self.signal.abort_with_reason(json!("hook bridge disposed"));
        loop {
            let notified = self.notify.notified();
            if self.pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// Creates a detached-runs tracker (one per bridge apply).
#[must_use]
pub fn create_detached_runs() -> DetachedRuns {
    DetachedRuns {
        signal: AbortSignal::default(),
        pending: Arc::new(AtomicUsize::new(0)),
        notify: Arc::new(Notify::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn starts_unfired_and_drain_fires_the_signal() {
        let detached = create_detached_runs();
        assert!(!detached.signal.is_aborted());
        detached.drain().await;
        assert!(detached.signal.is_aborted());
        assert_eq!(
            detached.signal.reason(),
            Some(json!("hook bridge disposed"))
        );
    }

    #[tokio::test]
    async fn drain_with_nothing_tracked_resolves_immediately() {
        create_detached_runs().drain().await;
    }

    #[tokio::test]
    async fn drain_waits_for_a_tracked_run_to_settle() {
        let detached = create_detached_runs();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        detached.track(async move {
            let _ = rx.await;
        });
        let drained = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&drained);
        let draining = tokio::spawn(async move {
            detached.drain().await;
            flag.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(!drained.load(Ordering::SeqCst));
        let _ = tx.send(());
        draining.await.expect("drain joins");
        assert!(drained.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn drain_waits_for_a_run_tracked_while_a_prior_wave_settles() {
        let detached = create_detached_runs();
        let (tx_first, rx_first) = tokio::sync::oneshot::channel::<()>();
        let (tx_second, rx_second) = tokio::sync::oneshot::channel::<()>();
        let inner = detached.clone();
        detached.track(async move {
            let _ = rx_first.await;
            inner.track(async move {
                let _ = rx_second.await;
            });
        });
        let drained = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&drained);
        let draining = tokio::spawn(async move {
            detached.drain().await;
            flag.store(true, Ordering::SeqCst);
        });
        let _ = tx_first.send(());
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert!(!drained.load(Ordering::SeqCst));
        let _ = tx_second.send(());
        draining.await.expect("drain joins");
        assert!(drained.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_panicking_tracked_run_is_absorbed() {
        let detached = create_detached_runs();
        detached.track(async { panic!("hook run boom") });
        detached.drain().await;
    }
}
