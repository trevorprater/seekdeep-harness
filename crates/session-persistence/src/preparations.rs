//! Bounded sharing and exclusive reservation of unpublished sessions.

use std::{future::Future, sync::Arc};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::AbortSignal;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Notify, OnceCell};
use uuid::Uuid;

/// A cached source that owns one exact unpublished session.
pub trait PreparedSource: Send + Sync + 'static {
    /// Exact unpublished session represented by this source.
    fn session(&self) -> &Arc<Session>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Loading,
    Ready,
    Committing,
    Reserved,
}

struct EntryState<Source, CommitState> {
    phase: Phase,
    source: Option<Arc<Source>>,
    reservation_token: Option<Uuid>,
    committed_state: Option<Arc<CommitState>>,
}

struct PreparationEntry<Source, CommitState> {
    id: SessionId,
    result: OnceCell<Result<Arc<Source>, Arc<String>>>,
    state: Mutex<EntryState<Source, CommitState>>,
    changed: Notify,
}

struct PoolInner<Source, CommitState> {
    capacity: usize,
    entries: Mutex<IndexMap<SessionId, Arc<PreparationEntry<Source, CommitState>>>>,
}

/// One exclusively held prepared source and its committed persistence state.
pub struct SessionPreparationReservation<Source, CommitState> {
    entry: std::sync::Weak<PreparationEntry<Source, CommitState>>,
    token: Uuid,
    /// Exact cached source.
    pub source: Arc<Source>,
    /// Durable coordinator state established before reservation.
    pub state: Arc<CommitState>,
}

impl<Source, CommitState> Clone for SessionPreparationReservation<Source, CommitState> {
    fn clone(&self) -> Self {
        Self {
            entry: self.entry.clone(),
            token: self.token,
            source: self.source.clone(),
            state: self.state.clone(),
        }
    }
}

impl<Source, CommitState> std::fmt::Debug for SessionPreparationReservation<Source, CommitState> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionPreparationReservation")
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

/// Per-coordinator cold-read sharing, exclusive reservation, and ready-entry LRU.
pub struct SessionPreparations<Source, CommitState> {
    inner: Arc<PoolInner<Source, CommitState>>,
}

impl<Source, CommitState> Clone for SessionPreparations<Source, CommitState> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<Source, CommitState> std::fmt::Debug for SessionPreparations<Source, CommitState> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionPreparations")
            .field("capacity", &self.inner.capacity)
            .field("len", &self.inner.entries.lock().len())
            .finish()
    }
}

impl<Source, CommitState> SessionPreparations<Source, CommitState>
where
    Source: PreparedSource,
    CommitState: Send + Sync + 'static,
{
    /// Creates a pool with a positive completed-ready capacity.
    ///
    /// # Errors
    ///
    /// Rejects zero capacity.
    pub fn new(capacity: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(capacity > 0, "prepared session cache size must be positive");
        Ok(Self {
            inner: Arc::new(PoolInner {
                capacity,
                entries: Mutex::new(IndexMap::new()),
            }),
        })
    }

    /// Whether this pool currently knows one unpublished identity.
    #[must_use]
    pub fn has(&self, id: &SessionId) -> bool {
        self.inner.entries.lock().contains_key(id)
    }

    /// Observes one prepared source, sharing an in-flight read for the same id.
    ///
    /// # Errors
    ///
    /// Returns loader or observer-local cancellation failures.
    pub async fn inspect<Load, LoadFuture>(
        &self,
        id: &SessionId,
        load: Load,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Arc<Source>>
    where
        Load: FnOnce() -> LoadFuture + Send + 'static,
        LoadFuture: Future<Output = anyhow::Result<Source>> + Send + 'static,
    {
        let entry = self.entry_for(id, load);
        let source = await_result(&entry, signal.as_ref()).await?;
        if self.is_current(&entry) && entry.state.lock().phase == Phase::Ready {
            self.touch(&entry);
        }
        Ok(source)
    }

    /// Reserves a ready source after committing pending durable state.
    ///
    /// # Errors
    ///
    /// Returns loader, commit, or observer-local cancellation failures.
    pub async fn reserve<Load, LoadFuture, Commit, CommitFuture>(
        &self,
        id: &SessionId,
        load: Load,
        commit: Commit,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionPreparationReservation<Source, CommitState>>>
    where
        Load: FnOnce() -> LoadFuture + Send + 'static,
        LoadFuture: Future<Output = anyhow::Result<Source>> + Send + 'static,
        Commit: FnOnce(Arc<Source>) -> CommitFuture + Send,
        CommitFuture: Future<Output = anyhow::Result<Option<(Arc<Source>, CommitState)>>> + Send,
    {
        let entry = self.entry_for(id, load);
        let _ = await_result(&entry, signal.as_ref()).await?;
        loop {
            let changed = entry.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.is_current(&entry) {
                return Ok(None);
            }
            let became_committer = {
                let mut state = entry.state.lock();
                if state.phase == Phase::Ready {
                    state.phase = Phase::Committing;
                    true
                } else {
                    false
                }
            };
            if became_committer {
                break;
            }
            wait_notification(changed, signal.as_ref()).await?;
        }

        let source = entry
            .state
            .lock()
            .source
            .clone()
            .ok_or_else(|| anyhow::anyhow!("prepared source disappeared before commit"))?;
        let committed = match commit(source).await {
            Ok(committed) => committed,
            Err(error) => {
                self.remove(&entry);
                return Err(error);
            }
        };
        let Some((source, committed_state)) = committed else {
            self.remove(&entry);
            return Ok(None);
        };
        if let Err(error) = ensure_not_aborted(signal.as_ref()) {
            {
                let mut state = entry.state.lock();
                state.source = Some(source);
            }
            self.make_ready(&entry);
            return Err(error);
        }
        if !self.is_current(&entry) {
            return Ok(None);
        }
        let token = Uuid::now_v7();
        let committed_state = Arc::new(committed_state);
        {
            let mut state = entry.state.lock();
            state.source = Some(source.clone());
            state.committed_state = Some(committed_state.clone());
            state.reservation_token = Some(token);
            state.phase = Phase::Reserved;
        }
        Ok(Some(SessionPreparationReservation {
            entry: Arc::downgrade(&entry),
            token,
            source,
            state: committed_state,
        }))
    }

    /// Returns the reservation for an exact publication candidate. Any alias
    /// while the pool owns the identity is rejected.
    ///
    /// # Errors
    ///
    /// Rejects non-reserved or aliased publication candidates.
    pub fn reservation_for(
        &self,
        session: &Arc<Session>,
    ) -> anyhow::Result<Option<SessionPreparationReservation<Source, CommitState>>> {
        let Some(entry) = self.inner.entries.lock().get(session.id()).cloned() else {
            return Ok(None);
        };
        let state = entry.state.lock();
        if state.phase == Phase::Reserved
            && state
                .source
                .as_ref()
                .is_some_and(|source| Arc::ptr_eq(source.session(), session))
        {
            let token = state
                .reservation_token
                .ok_or_else(|| anyhow::anyhow!("reserved preparation lacks a token"))?;
            let committed_state = state
                .committed_state
                .clone()
                .ok_or_else(|| anyhow::anyhow!("reserved preparation lacks committed state"))?;
            let source = state
                .source
                .clone()
                .ok_or_else(|| anyhow::anyhow!("reserved preparation lacks a source"))?;
            return Ok(Some(SessionPreparationReservation {
                entry: Arc::downgrade(&entry),
                token,
                source,
                state: committed_state,
            }));
        }
        anyhow::bail!(
            "cannot publish session \"{}\": persisted state already owns this identity",
            session.id()
        )
    }

    /// Consumes an exact reservation after its session attaches.
    ///
    /// # Errors
    ///
    /// Rejects stale or aliased reservations.
    pub fn attach(
        &self,
        reservation: &SessionPreparationReservation<Source, CommitState>,
    ) -> anyhow::Result<()> {
        let entry = self.require_reservation(reservation)?;
        self.remove(&entry);
        Ok(())
    }

    /// Consumes a reservation used only for a committed inspection.
    pub fn discard(&self, reservation: &SessionPreparationReservation<Source, CommitState>) {
        if let Some(entry) = self.matching_reservation(reservation) {
            self.remove(&entry);
        }
    }

    /// Releases a reservation to the ready LRU or invalidates it.
    pub fn release(
        &self,
        reservation: &SessionPreparationReservation<Source, CommitState>,
        reusable: bool,
    ) {
        let Some(entry) = self.matching_reservation(reservation) else {
            return;
        };
        if !reusable {
            self.remove(&entry);
            return;
        }
        {
            let mut state = entry.state.lock();
            state.reservation_token = None;
            state.committed_state = None;
        }
        self.make_ready(&entry);
    }

    /// Invalidates any cached phase for an identity.
    pub fn invalidate(&self, id: &SessionId) {
        let entry = self.inner.entries.lock().get(id).cloned();
        if let Some(entry) = entry {
            self.remove(&entry);
        }
    }

    /// Discards an exact stale ready source without disturbing exclusivity.
    #[must_use]
    pub fn discard_ready(&self, id: &SessionId, expected: &Arc<Source>) -> DiscardReady {
        let Some(entry) = self.inner.entries.lock().get(id).cloned() else {
            return DiscardReady::Missing;
        };
        let state = entry.state.lock();
        if !state
            .source
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, expected))
        {
            return DiscardReady::Missing;
        }
        if state.phase != Phase::Ready {
            return DiscardReady::Retained;
        }
        drop(state);
        self.remove(&entry);
        DiscardReady::Discarded
    }

    /// Rejects writes during commit or exclusive reservation.
    ///
    /// # Errors
    ///
    /// Returns an ownership conflict.
    pub fn assert_writable(&self, id: &SessionId) -> anyhow::Result<()> {
        let entry = self.inner.entries.lock().get(id).cloned();
        if entry.is_some_and(|entry| {
            matches!(
                entry.state.lock().phase,
                Phase::Committing | Phase::Reserved
            )
        }) {
            anyhow::bail!(
                "cannot append session \"{id}\" while its persisted preparation is reserved"
            );
        }
        Ok(())
    }

    /// Takes one ready source for serialized append adoption.
    #[must_use]
    pub fn take_ready(&self, id: &SessionId) -> Option<Arc<Source>> {
        let entry = self.inner.entries.lock().get(id).cloned()?;
        let state = entry.state.lock();
        if state.phase != Phase::Ready {
            return None;
        }
        let source = state.source.clone()?;
        drop(state);
        self.remove(&entry);
        Some(source)
    }

    fn entry_for<Load, LoadFuture>(
        &self,
        id: &SessionId,
        load: Load,
    ) -> Arc<PreparationEntry<Source, CommitState>>
    where
        Load: FnOnce() -> LoadFuture + Send + 'static,
        LoadFuture: Future<Output = anyhow::Result<Source>> + Send + 'static,
    {
        let mut entries = self.inner.entries.lock();
        if let Some(existing) = entries.get(id) {
            return existing.clone();
        }
        let entry = Arc::new(PreparationEntry {
            id: id.clone(),
            result: OnceCell::new(),
            state: Mutex::new(EntryState {
                phase: Phase::Loading,
                source: None,
                reservation_token: None,
                committed_state: None,
            }),
            changed: Notify::new(),
        });
        entries.insert(id.clone(), entry.clone());
        drop(entries);

        let pool = self.clone();
        let loading_entry = entry.clone();
        tokio::spawn(async move {
            let result = load()
                .await
                .map(Arc::new)
                .map_err(|error| Arc::new(error.to_string()));
            let _ = loading_entry.result.set(result.clone());
            match result {
                Ok(source) if pool.is_current(&loading_entry) => {
                    loading_entry.state.lock().source = Some(source);
                    pool.make_ready(&loading_entry);
                }
                Ok(_) => loading_entry.changed.notify_waiters(),
                Err(_) => pool.remove(&loading_entry),
            }
        });
        entry
    }

    fn matching_reservation(
        &self,
        reservation: &SessionPreparationReservation<Source, CommitState>,
    ) -> Option<Arc<PreparationEntry<Source, CommitState>>> {
        let entry = reservation.entry.upgrade()?;
        if !self.is_current(&entry) {
            return None;
        }
        let state = entry.state.lock();
        (state.phase == Phase::Reserved && state.reservation_token == Some(reservation.token))
            .then_some(entry.clone())
    }

    fn require_reservation(
        &self,
        reservation: &SessionPreparationReservation<Source, CommitState>,
    ) -> anyhow::Result<Arc<PreparationEntry<Source, CommitState>>> {
        self.matching_reservation(reservation).ok_or_else(|| {
            anyhow::anyhow!("session preparation is no longer reserved by this pool")
        })
    }

    fn is_current(&self, entry: &Arc<PreparationEntry<Source, CommitState>>) -> bool {
        self.inner
            .entries
            .lock()
            .get(&entry.id)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
    }

    fn make_ready(&self, entry: &Arc<PreparationEntry<Source, CommitState>>) {
        if !self.is_current(entry) {
            return;
        }
        entry.state.lock().phase = Phase::Ready;
        entry.changed.notify_waiters();
        self.touch(entry);
    }

    fn remove(&self, entry: &Arc<PreparationEntry<Source, CommitState>>) {
        let mut entries = self.inner.entries.lock();
        if entries
            .get(&entry.id)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            entries.shift_remove(&entry.id);
        }
        drop(entries);
        entry.changed.notify_waiters();
    }

    fn touch(&self, entry: &Arc<PreparationEntry<Source, CommitState>>) {
        let mut entries = self.inner.entries.lock();
        let Some((id, current)) = entries.shift_remove_entry(&entry.id) else {
            return;
        };
        entries.insert(id, current);
        let ready_count = entries
            .values()
            .filter(|candidate| candidate.state.lock().phase == Phase::Ready)
            .count();
        if ready_count <= self.inner.capacity {
            return;
        }
        if let Some(index) = entries
            .values()
            .position(|candidate| candidate.state.lock().phase == Phase::Ready)
        {
            entries.shift_remove_index(index);
        }
    }
}

/// Outcome of exact ready-source invalidation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReady {
    /// Exact source was removed.
    Discarded,
    /// Exact source has an exclusive committing/reserved owner.
    Retained,
    /// Entry or exact source was absent.
    Missing,
}

/// Observer-local queued cancellation retaining the exact JSON reason.
#[derive(Clone, Debug, Error, PartialEq)]
#[error("queued observation aborted: {reason}")]
pub struct QueuedObservationAborted {
    /// Exact first cancellation reason carried by the signal.
    pub reason: Value,
}

/// Observes shared queued work with prompt observer-local cancellation.
///
/// The shared operation is never cancelled. Cancellation wins until the
/// operation crosses its caller-defined ownership cutoff.
///
/// # Errors
///
/// Returns the operation failure or [`QueuedObservationAborted`].
pub async fn observe_queued_abort<T, Operation>(
    operation: Operation,
    signal: &AbortSignal,
) -> anyhow::Result<T>
where
    T: Send + 'static,
    Operation: Future<Output = anyhow::Result<T>> + Send + 'static,
{
    observe_queued_abort_with(operation, signal, || false).await
}

/// Observes shared work with a dynamic cancellation cutoff predicate.
///
/// Once `started` returns true, the operation owns settlement even if the
/// observer signal is cancelled.
///
/// # Errors
///
/// Returns the operation failure or [`QueuedObservationAborted`].
pub async fn observe_queued_abort_with<T, Operation, Started>(
    operation: Operation,
    signal: &AbortSignal,
    started: Started,
) -> anyhow::Result<T>
where
    T: Send + 'static,
    Operation: Future<Output = anyhow::Result<T>> + Send + 'static,
    Started: Fn() -> bool,
{
    let mut operation = tokio::spawn(operation);
    if signal.is_aborted() && !started() {
        return Err(queued_abort_error(signal));
    }
    tokio::select! {
        result = &mut operation => flatten_observed_operation(result),
        () = signal.cancelled() => {
            if started() {
                flatten_observed_operation(operation.await)
            } else {
                Err(queued_abort_error(signal))
            }
        }
    }
}

fn flatten_observed_operation<T>(
    result: Result<anyhow::Result<T>, tokio::task::JoinError>,
) -> anyhow::Result<T> {
    result.map_err(anyhow::Error::from)?
}

fn queued_abort_error(signal: &AbortSignal) -> anyhow::Error {
    QueuedObservationAborted {
        reason: signal.reason().unwrap_or(Value::Null),
    }
    .into()
}

async fn await_result<Source, CommitState>(
    entry: &Arc<PreparationEntry<Source, CommitState>>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Arc<Source>> {
    loop {
        let changed = entry.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if let Some(result) = entry.result.get() {
            return result
                .clone()
                .map_err(|error| anyhow::Error::msg((*error).clone()));
        }
        wait_notification(changed, signal).await?;
    }
}

async fn wait_notification<Changed>(
    changed: Changed,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<()>
where
    Changed: Future<Output = ()>,
{
    if let Some(signal) = signal {
        ensure_not_aborted(Some(signal))?;
        tokio::select! {
            () = changed => Ok(()),
            () = signal.cancelled() => ensure_not_aborted(Some(signal)),
        }
    } else {
        changed.await;
        Ok(())
    }
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if let Some(signal) = signal
        && signal.is_aborted()
    {
        anyhow::bail!(
            "session preparation observation aborted: {}",
            signal.reason().unwrap_or(serde_json::Value::Null)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use seekdeep_core::session::Session;
    use tokio::sync::Notify;

    use super::*;

    #[derive(Debug)]
    struct Source {
        session: Arc<Session>,
        label: String,
    }

    impl PreparedSource for Source {
        fn session(&self) -> &Arc<Session> {
            &self.session
        }
    }

    fn source(id: &str, label: &str) -> Source {
        Source {
            session: Session::create(&SessionId::new(id), None, None).expect("session"),
            label: label.to_owned(),
        }
    }

    #[tokio::test]
    async fn shares_in_flight_and_ready_sources_then_invalidates() {
        let pool = SessionPreparations::<Source, String>::new(2).expect("pool");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let loads = Arc::new(AtomicUsize::new(0));
        let first = tokio::spawn({
            let pool = pool.clone();
            let entered = entered.clone();
            let release = release.clone();
            let loads = loads.clone();
            async move {
                pool.inspect(
                    &SessionId::new("shared"),
                    move || async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        entered.notify_one();
                        release.notified().await;
                        Ok(source("shared", "one"))
                    },
                    None,
                )
                .await
            }
        });
        entered.notified().await;
        let second = tokio::spawn({
            let pool = pool.clone();
            async move {
                pool.inspect(
                    &SessionId::new("shared"),
                    || async { Ok(source("shared", "unused")) },
                    None,
                )
                .await
            }
        });
        release.notify_one();
        let first = first.await.expect("first join").expect("first");
        let second = second.await.expect("second join").expect("second");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.label, "one");
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        pool.invalidate(&SessionId::new("shared"));
        let reloaded = pool
            .inspect(
                &SessionId::new("shared"),
                || async { Ok(source("shared", "two")) },
                None,
            )
            .await
            .expect("reload");
        assert_eq!(reloaded.label, "two");
    }

    #[tokio::test]
    async fn reservation_is_exclusive_releasable_and_exact_for_publication() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let reservation = pool
            .reserve(
                &SessionId::new("reserved"),
                || async { Ok(source("reserved", "source")) },
                |source| async move { Ok(Some((source, "state".to_owned()))) },
                None,
            )
            .await
            .expect("reserve")
            .expect("reservation");
        assert_eq!(reservation.state.as_str(), "state");
        let exact = pool
            .reservation_for(reservation.source.session())
            .expect("exact")
            .expect("held");
        assert_eq!(exact.token, reservation.token);
        let alias = Session::create(&SessionId::new("reserved"), None, None).expect("alias");
        assert!(pool.reservation_for(&alias).is_err());
        assert!(pool.assert_writable(&SessionId::new("reserved")).is_err());
        pool.release(&reservation, true);
        assert!(pool.assert_writable(&SessionId::new("reserved")).is_ok());
        let source = pool.take_ready(&SessionId::new("reserved")).expect("ready");
        assert_eq!(source.label, "source");
    }

    #[tokio::test]
    async fn cancelling_one_observer_does_not_cancel_shared_load() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let release = Arc::new(Notify::new());
        let signal = AbortSignal::default();
        let observer = tokio::spawn({
            let pool = pool.clone();
            let release = release.clone();
            let signal = signal.clone();
            async move {
                pool.inspect(
                    &SessionId::new("cancel"),
                    move || async move {
                        release.notified().await;
                        Ok(source("cancel", "loaded"))
                    },
                    Some(signal),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        signal.abort_with_reason(serde_json::json!({"kind": "caller"}));
        assert!(
            observer
                .await
                .expect("join")
                .expect_err("cancelled")
                .to_string()
                .contains("caller")
        );
        release.notify_one();
        let loaded = pool
            .inspect(
                &SessionId::new("cancel"),
                || async { Ok(source("cancel", "unused")) },
                None,
            )
            .await
            .expect("shared completion");
        assert_eq!(loaded.label, "loaded");
    }

    #[tokio::test]
    async fn ready_lru_evicts_only_ready_entries() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let held = pool
            .reserve(
                &SessionId::new("held"),
                || async { Ok(source("held", "held")) },
                |source| async move { Ok(Some((source, "state".to_owned()))) },
                None,
            )
            .await
            .expect("held")
            .expect("reservation");
        let _ = pool
            .inspect(
                &SessionId::new("one"),
                || async { Ok(source("one", "one")) },
                None,
            )
            .await
            .expect("one");
        let _ = pool
            .inspect(
                &SessionId::new("two"),
                || async { Ok(source("two", "two")) },
                None,
            )
            .await
            .expect("two");
        assert!(!pool.has(&SessionId::new("one")));
        assert!(pool.has(&SessionId::new("two")));
        assert!(pool.has(&SessionId::new("held")));
        pool.discard(&held);
    }

    #[tokio::test]
    async fn failed_and_invalidated_loads_leave_no_cached_ownership() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let failed_id = SessionId::new("failed-load");
        let failure = pool
            .inspect(
                &failed_id,
                || async { Err(anyhow::anyhow!("load failed")) },
                None,
            )
            .await
            .expect_err("failed load");
        assert_eq!(failure.to_string(), "load failed");
        assert!(!pool.has(&failed_id));

        let invalidated_id = SessionId::new("invalidated-load");
        let release = Arc::new(Notify::new());
        let inspection = tokio::spawn({
            let pool = pool.clone();
            let id = invalidated_id.clone();
            let release = release.clone();
            async move {
                pool.inspect(
                    &id,
                    move || async move {
                        release.notified().await;
                        Ok(source("invalidated-load", "loaded"))
                    },
                    None,
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        pool.invalidate(&invalidated_id);
        release.notify_one();
        assert_eq!(
            inspection
                .await
                .expect("join")
                .expect("original observer")
                .label,
            "loaded"
        );
        assert!(!pool.has(&invalidated_id));
    }

    #[tokio::test]
    async fn cancelled_observers_still_feed_ready_lru_eviction() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let first_signal = AbortSignal::default();
        let second_signal = AbortSignal::default();
        let first_gate = Arc::new(Notify::new());
        let second_gate = Arc::new(Notify::new());
        let first = tokio::spawn({
            let pool = pool.clone();
            let signal = first_signal.clone();
            let gate = first_gate.clone();
            async move {
                pool.inspect(
                    &SessionId::new("cancelled-ready-first"),
                    move || async move {
                        gate.notified().await;
                        Ok(source("cancelled-ready-first", "first"))
                    },
                    Some(signal),
                )
                .await
            }
        });
        let second = tokio::spawn({
            let pool = pool.clone();
            let signal = second_signal.clone();
            let gate = second_gate.clone();
            async move {
                pool.inspect(
                    &SessionId::new("cancelled-ready-second"),
                    move || async move {
                        gate.notified().await;
                        Ok(source("cancelled-ready-second", "second"))
                    },
                    Some(signal),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        first_signal.abort();
        second_signal.abort();
        assert!(first.await.expect("first join").is_err());
        assert!(second.await.expect("second join").is_err());
        first_gate.notify_one();
        pool.inspect(
            &SessionId::new("cancelled-ready-first"),
            || async { Ok(source("cancelled-ready-first", "unused")) },
            None,
        )
        .await
        .expect("first ready");
        second_gate.notify_one();
        pool.inspect(
            &SessionId::new("cancelled-ready-second"),
            || async { Ok(source("cancelled-ready-second", "unused")) },
            None,
        )
        .await
        .expect("second ready");
        assert!(!pool.has(&SessionId::new("cancelled-ready-first")));
        assert!(pool.has(&SessionId::new("cancelled-ready-second")));
    }

    #[tokio::test]
    async fn discard_ready_is_exact_and_never_breaks_a_reservation() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let id = SessionId::new("discard-ready");
        let ready = pool
            .inspect(&id, || async { Ok(source("discard-ready", "ready")) }, None)
            .await
            .expect("ready");
        let different = Arc::new(source("discard-ready", "different"));
        assert_eq!(pool.discard_ready(&id, &different), DiscardReady::Missing);
        assert_eq!(pool.discard_ready(&id, &ready), DiscardReady::Discarded);

        let reservation = pool
            .reserve(
                &id,
                || async { Ok(source("discard-ready", "reserved")) },
                |source| async move { Ok(Some((source, "state".to_owned()))) },
                None,
            )
            .await
            .expect("reserve")
            .expect("reservation");
        assert_eq!(
            pool.discard_ready(&id, &reservation.source),
            DiscardReady::Retained
        );
        pool.release(&reservation, false);
        assert!(!pool.has(&id));
    }

    #[tokio::test]
    async fn reservation_wait_reuses_exact_source_and_attach_consumes_once() {
        let pool = SessionPreparations::<Source, String>::new(2).expect("pool");
        let id = SessionId::new("reservation-wait");
        let first = pool
            .reserve(
                &id,
                || async { Ok(source("reservation-wait", "source")) },
                |source| async move { Ok(Some((source, "first".to_owned()))) },
                None,
            )
            .await
            .expect("first")
            .expect("first reservation");
        let second = tokio::spawn({
            let pool = pool.clone();
            let id = id.clone();
            async move {
                pool.reserve(
                    &id,
                    || async { Ok(source("reservation-wait", "unused")) },
                    |source| async move { Ok(Some((source, "second".to_owned()))) },
                    None,
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        pool.release(&first, true);
        let second = second
            .await
            .expect("join")
            .expect("second")
            .expect("second reservation");
        assert!(Arc::ptr_eq(&first.source, &second.source));
        pool.attach(&second).expect("attach");
        assert!(pool.attach(&second).is_err());
        assert!(
            pool.reservation_for(second.source.session())
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn reservation_wait_is_abortable_without_cancelling_held_owner() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let id = SessionId::new("abortable-reservation-wait");
        let first = pool
            .reserve(
                &id,
                || async { Ok(source("abortable-reservation-wait", "source")) },
                |source| async move { Ok(Some((source, "state".to_owned()))) },
                None,
            )
            .await
            .expect("first")
            .expect("reservation");
        let signal = AbortSignal::default();
        let waiting = tokio::spawn({
            let pool = pool.clone();
            let id = id.clone();
            let signal = signal.clone();
            async move {
                pool.reserve(
                    &id,
                    || async { Ok(source("abortable-reservation-wait", "unused")) },
                    |source| async move { Ok(Some((source, "state".to_owned()))) },
                    Some(signal),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        signal.abort_with_reason(serde_json::json!({"kind": "cancelled"}));
        assert!(waiting.await.expect("join").is_err());
        assert!(
            pool.reservation_for(first.source.session())
                .expect("reservation lookup")
                .is_some()
        );
        pool.release(&first, false);
        assert!(!pool.has(&id));
    }

    #[tokio::test]
    async fn failed_or_invalidated_commit_wakes_waiters_without_reviving_entry() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let id = SessionId::new("failed-commit");
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let first = tokio::spawn({
            let pool = pool.clone();
            let id = id.clone();
            let started = started.clone();
            let release = release.clone();
            async move {
                pool.reserve(
                    &id,
                    || async { Ok(source("failed-commit", "source")) },
                    move |_source| async move {
                        started.notify_one();
                        release.notified().await;
                        Err(anyhow::anyhow!("commit failed"))
                    },
                    None,
                )
                .await
            }
        });
        started.notified().await;
        let second = tokio::spawn({
            let pool = pool.clone();
            let id = id.clone();
            async move {
                pool.reserve(
                    &id,
                    || async { Ok(source("failed-commit", "unused")) },
                    |source| async move { Ok(Some((source, "state".to_owned()))) },
                    None,
                )
                .await
            }
        });
        release.notify_one();
        assert_eq!(
            first
                .await
                .expect("first join")
                .expect_err("commit failure")
                .to_string(),
            "commit failed"
        );
        assert!(
            second
                .await
                .expect("second join")
                .expect("second")
                .is_none()
        );
        assert!(!pool.has(&id));

        let invalidated_id = SessionId::new("invalidated-commit");
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let reservation = tokio::spawn({
            let pool = pool.clone();
            let id = invalidated_id.clone();
            let started = started.clone();
            let release = release.clone();
            async move {
                pool.reserve(
                    &id,
                    || async { Ok(source("invalidated-commit", "source")) },
                    move |source| async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(Some((source, "state".to_owned())))
                    },
                    None,
                )
                .await
            }
        });
        started.notified().await;
        pool.invalidate(&invalidated_id);
        release.notify_one();
        assert!(
            reservation
                .await
                .expect("reservation join")
                .expect("reservation")
                .is_none()
        );
        assert!(!pool.has(&invalidated_id));
    }

    #[tokio::test]
    async fn post_commit_cancellation_returns_exact_source_to_ready_pool() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let id = SessionId::new("post-commit-cancel");
        let signal = AbortSignal::default();
        let commit_signal = signal.clone();
        let error = pool
            .reserve(
                &id,
                || async { Ok(source("post-commit-cancel", "source")) },
                move |source| async move {
                    commit_signal.abort_with_reason(serde_json::json!({"kind": "cancelled"}));
                    Ok(Some((source, "state".to_owned())))
                },
                Some(signal),
            )
            .await
            .expect_err("post-commit cancellation");
        assert!(error.to_string().contains("cancelled"));
        let ready = pool.take_ready(&id).expect("ready source");
        assert_eq!(ready.label, "source");
        assert!(pool.take_ready(&id).is_none());
    }

    #[tokio::test]
    async fn post_commit_cancellation_does_not_revive_an_invalidated_entry() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let id = SessionId::new("invalidated-commit-cancel");
        let signal = AbortSignal::default();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let reservation = tokio::spawn({
            let pool = pool.clone();
            let id = id.clone();
            let signal = signal.clone();
            let started = started.clone();
            let release = release.clone();
            async move {
                pool.reserve(
                    &id,
                    || async { Ok(source("invalidated-commit-cancel", "source")) },
                    move |source| async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(Some((source, "state".to_owned())))
                    },
                    Some(signal),
                )
                .await
            }
        });
        started.notified().await;
        pool.invalidate(&id);
        signal.abort_with_reason(serde_json::json!("cancel invalidated commit"));
        release.notify_one();
        assert!(
            reservation
                .await
                .expect("join")
                .expect_err("cancelled")
                .to_string()
                .contains("cancel invalidated commit")
        );
        assert!(!pool.has(&id));
    }

    #[tokio::test]
    async fn invalidated_pending_reservation_and_inspection_publication_fail_closed() {
        let pool = SessionPreparations::<Source, String>::new(1).expect("pool");
        let id = SessionId::new("invalidated-reservation");
        let release = Arc::new(Notify::new());
        let reservation = tokio::spawn({
            let pool = pool.clone();
            let id = id.clone();
            let release = release.clone();
            async move {
                pool.reserve(
                    &id,
                    move || async move {
                        release.notified().await;
                        Ok(source("invalidated-reservation", "source"))
                    },
                    |source| async move { Ok(Some((source, "state".to_owned()))) },
                    None,
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        assert!(pool.take_ready(&id).is_none());
        pool.invalidate(&id);
        release.notify_one();
        assert!(
            reservation
                .await
                .expect("join")
                .expect("reservation")
                .is_none()
        );

        let inspected = pool
            .inspect(
                &SessionId::new("inspection-publication"),
                || async { Ok(source("inspection-publication", "source")) },
                None,
            )
            .await
            .expect("inspection");
        assert!(pool.reservation_for(inspected.session()).is_err());
    }

    #[tokio::test]
    async fn queued_abort_relays_fulfillment_and_operation_failure() {
        let signal = AbortSignal::default();
        assert_eq!(
            observe_queued_abort(async { Ok("value") }, &signal)
                .await
                .expect("value"),
            "value"
        );
        let failure = observe_queued_abort::<(), _>(
            async { Err(anyhow::anyhow!("operation failed")) },
            &signal,
        )
        .await
        .expect_err("failure");
        assert_eq!(failure.to_string(), "operation failed");
    }

    #[tokio::test]
    async fn queued_abort_is_prompt_retains_reason_and_does_not_cancel_shared_work() {
        let signal = AbortSignal::default();
        let completed = Arc::new(Notify::new());
        let completed_task = completed.clone();
        let release = Arc::new(Notify::new());
        let release_task = release.clone();
        let observed = tokio::spawn({
            let signal = signal.clone();
            async move {
                observe_queued_abort(
                    async move {
                        release_task.notified().await;
                        completed_task.notify_one();
                        Ok("late")
                    },
                    &signal,
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        signal.abort_with_reason(serde_json::json!({"kind": "aborted"}));
        let error = observed
            .await
            .expect("observer join")
            .expect_err("observer abort");
        assert_eq!(
            error
                .downcast_ref::<QueuedObservationAborted>()
                .expect("typed abort")
                .reason,
            serde_json::json!({"kind": "aborted"})
        );
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), completed.notified())
            .await
            .expect("shared work continued");
    }

    #[tokio::test]
    async fn preaborted_observer_rejects_but_started_operation_owns_settlement() {
        let preaborted = AbortSignal::default();
        preaborted.abort_with_reason(serde_json::json!("pre-aborted"));
        let error = observe_queued_abort(async { Ok::<_, anyhow::Error>("ignored") }, &preaborted)
            .await
            .expect_err("pre-aborted");
        assert_eq!(
            error
                .downcast_ref::<QueuedObservationAborted>()
                .expect("typed abort")
                .reason,
            serde_json::json!("pre-aborted")
        );

        let signal = AbortSignal::default();
        let started = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = Arc::new(Notify::new());
        let gate_task = gate.clone();
        let observed = tokio::spawn({
            let signal = signal.clone();
            let started = started.clone();
            async move {
                observe_queued_abort_with(
                    async move {
                        gate_task.notified().await;
                        Ok("owned")
                    },
                    &signal,
                    move || started.load(Ordering::Acquire),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        signal.abort_with_reason(serde_json::json!("too late"));
        gate.notify_one();
        assert_eq!(
            observed.await.expect("join").expect("owned settlement"),
            "owned"
        );
    }
}
