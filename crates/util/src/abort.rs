//! Cloneable first-cause cancellation signal shared across runtime layers.

use std::{
    any::Any,
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::future::BoxFuture;
use serde_json::Value;

#[derive(Clone)]
struct StoredReason {
    json: Value,
    typed: Option<Arc<dyn Any + Send + Sync>>,
    error: Option<Arc<dyn Error + Send + Sync>>,
}

impl fmt::Debug for StoredReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredReason")
            .field("json", &self.json)
            .field("typed", &self.typed.as_ref().map(|_| ".."))
            .field("error", &self.error.as_ref().map(|_| ".."))
            .finish()
    }
}

#[derive(Debug, Default)]
struct AbortState {
    aborted: AtomicBool,
    order: AtomicU64,
    reason: parking_lot::Mutex<Option<StoredReason>>,
    sources: Vec<AbortSignal>,
    notify: tokio::sync::Notify,
}

static NEXT_ABORT_ORDER: AtomicU64 = AtomicU64::new(1);

/// Cloneable caller-cancellation signal with lossless JSON reasons.
#[derive(Clone, Debug, Default)]
pub struct AbortSignal(Arc<AbortState>);

impl AbortSignal {
    /// Requests cancellation with a null reason.
    pub fn abort(&self) {
        self.abort_with_reason(Value::Null);
    }

    /// Requests cancellation with a lossless cross-layer reason.
    ///
    /// Like the web `AbortController`, the first request wins.
    pub fn abort_with_reason(&self, reason: Value) {
        self.abort_with_stored_reason(StoredReason {
            json: reason,
            typed: None,
            error: None,
        });
    }

    /// Requests cancellation with both a typed and JSON-visible reason.
    ///
    /// Typed lookup provides the Rust equivalent of JavaScript `instanceof`:
    /// plain JSON with the same fields does not classify as the typed reason.
    pub fn abort_with_typed_reason<T>(&self, reason: Arc<T>, json: Value)
    where
        T: Any + Send + Sync,
    {
        self.abort_with_stored_reason(StoredReason {
            json,
            typed: Some(reason),
            error: None,
        });
    }

    /// Requests cancellation with one reason available through typed, JSON,
    /// and generic Error views.
    pub fn abort_with_error<T>(&self, reason: Arc<T>, json: Value)
    where
        T: Any + Error + Send + Sync,
    {
        self.abort_with_stored_reason(StoredReason {
            json,
            typed: Some(reason.clone()),
            error: Some(reason),
        });
    }

    fn abort_with_stored_reason(&self, reason: StoredReason) {
        let mut stored = self.0.reason.lock();
        if self.0.aborted.load(Ordering::Acquire) {
            return;
        }
        *stored = Some(reason);
        self.0.order.store(
            NEXT_ABORT_ORDER.fetch_add(1, Ordering::AcqRel),
            Ordering::Release,
        );
        self.0.aborted.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    /// Whether this signal or any fused source is cancelled.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.0.aborted.load(Ordering::Acquire) || self.0.sources.iter().any(Self::is_aborted)
    }

    /// First cancellation reason from this signal or its fused sources.
    #[must_use]
    pub fn reason(&self) -> Option<Value> {
        self.winning_reason().map(|reason| reason.json)
    }

    /// Recovers the chronologically winning typed reason by concrete type.
    #[must_use]
    pub fn typed_reason<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        let typed = self.winning_reason()?.typed?;
        Arc::downcast::<T>(typed).ok()
    }

    /// Recovers the chronologically winning shared Error reason.
    #[must_use]
    pub fn error_reason(&self) -> Option<Arc<dyn Error + Send + Sync>> {
        self.winning_reason()?.error
    }

    fn winning_reason(&self) -> Option<StoredReason> {
        let winner = self.winning_signal()?;
        winner.0.reason.lock().clone()
    }

    fn winning_signal(&self) -> Option<Self> {
        let own = self
            .0
            .aborted
            .load(Ordering::Acquire)
            .then(|| (self.0.order.load(Ordering::Acquire), self.clone()));
        self.0
            .sources
            .iter()
            .filter_map(Self::winning_signal)
            .filter_map(|signal| {
                let order = signal.0.order.load(Ordering::Acquire);
                (order != 0).then_some((order, signal))
            })
            .chain(own)
            .min_by_key(|(order, _)| *order)
            .map(|(_, signal)| signal)
    }

    /// Resolves when this signal or any fused source is cancelled.
    #[must_use]
    pub fn cancelled(&self) -> BoxFuture<'static, ()> {
        let signal = self.clone();
        Box::pin(async move {
            if signal.is_aborted() {
                return;
            }
            let mut waits = signal
                .0
                .sources
                .iter()
                .map(Self::cancelled)
                .collect::<Vec<_>>();
            waits.push(signal.direct_cancelled());
            let _ = futures::future::select_all(waits).await;
        })
    }

    fn direct_cancelled(&self) -> BoxFuture<'static, ()> {
        let signal = self.clone();
        Box::pin(async move {
            loop {
                let notified = signal.0.notify.notified();
                if signal.0.aborted.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        })
    }

    /// Creates one live signal observing cancellation from either source.
    ///
    /// Equal inputs preserve their exact identity. Distinct inputs remain flat
    /// sources so replacing a wrapper cannot detach caller cancellation.
    #[must_use]
    pub fn fuse(first: &Self, second: &Self) -> Self {
        if first == second {
            return first.clone();
        }
        Self(Arc::new(AbortState {
            aborted: AtomicBool::new(false),
            order: AtomicU64::new(0),
            reason: parking_lot::Mutex::new(None),
            sources: vec![first.clone(), second.clone()],
            notify: tokio::sync::Notify::new(),
        }))
    }
}

impl PartialEq for AbortSignal {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for AbortSignal {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn first_reason_wins_and_waiters_wake() {
        let signal = AbortSignal::default();
        let waiting = tokio::spawn(signal.cancelled());
        signal.abort_with_reason(json!({ "code": "FIRST" }));
        signal.abort_with_reason(json!({ "code": "SECOND" }));
        waiting.await.unwrap();
        assert!(signal.is_aborted());
        assert_eq!(signal.reason(), Some(json!({ "code": "FIRST" })));
    }

    #[tokio::test]
    async fn fused_signal_preserves_identity_and_observes_sources() {
        let first = AbortSignal::default();
        assert_eq!(AbortSignal::fuse(&first, &first), first);
        let second = AbortSignal::default();
        let fused = AbortSignal::fuse(&first, &second);
        second.abort_with_reason(json!("second"));
        fused.cancelled().await;
        assert!(fused.is_aborted());
        assert_eq!(fused.reason(), Some(json!("second")));
    }

    #[tokio::test]
    async fn fused_signal_preserves_chronologically_first_cause() {
        let upstream = AbortSignal::default();
        let timer = AbortSignal::default();
        let fused = AbortSignal::fuse(&upstream, &timer);
        timer.abort_with_reason(json!("timer first"));
        upstream.abort_with_reason(json!("upstream later"));
        assert_eq!(fused.reason(), Some(json!("timer first")));
    }
}
