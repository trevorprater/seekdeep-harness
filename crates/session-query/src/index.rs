//! Service definition for combined session-history reads, traces, filters, and
//! full-text search.

use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::ensure_persistence_not_aborted;
use seekdeep_session_title::{SessionTitleSnapshot, fold_session_title};

use crate::{
    config::{
        Config, SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY, SESSION_QUERY_READ_WINDOW_MAX,
        SessionQueryError, SessionQueryErrorCode,
    },
    corpus::{LogicalProjectionResult, LogicalSession, SessionCorpus},
    documents::build_session_event_search_documents,
    filters::{
        filter_session_event_documents, filter_session_results,
        materialize_session_event_result_filters, materialize_session_result_filters,
    },
    tracing,
    types::{
        SessionEventReadRequest, SessionEventRecord, SessionEventResultFilter,
        SessionEventSearchDocument, SessionEventSearchPage, SessionEventSearchRequest,
        SessionEventTraceObservation, SessionEventTraceRequest, SessionEventWindow,
        SessionLineageTrace, SessionLogSnapshot, SessionRecord, SessionResultFilter,
        SessionSearchExecContext, SessionSearchHit, SessionSearchPage, SessionSearchRequest,
        SessionSurfaceSnapshot, SessionTitleObservation, SessionTitleObservationResult,
    },
};

/// Largest safe integer the source runtime can represent exactly.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Unified live-preferred session query service.
///
/// Exact reads, filters, and traces are backend-independent concrete behavior.
/// A backend implements full-text observation, reconciliation, ranking, cursor
/// generations, and query execution.
#[async_trait]
pub trait SessionQueryEngine: Send + Sync + 'static {
    /// Returns the corpus resolver owned by this engine.
    fn corpus(&self) -> &SessionCorpus;

    /// Returns the validated maximum before/after raw-event window.
    fn read_window_max(&self) -> u64;

    /// Searches the live-preferred logical corpus and groups by session.
    async fn search_sessions(
        &self,
        request: SessionSearchRequest,
        exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionSearchPage<SessionSearchHit>>;

    /// Searches events within one live-preferred logical session.
    async fn search_events(
        &self,
        request: SessionEventSearchRequest,
        exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionEventSearchPage>;

    /// Lists the complete logical corpus using live-preferred records.
    ///
    /// # Errors
    ///
    /// Returns cancellation, persistence-listing, or source-conflict failures.
    async fn list_sessions(
        &self,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionRecord>> {
        self.corpus().list_sessions(signal.as_ref()).await
    }

    /// Reads and replay-validates one complete logical session log without
    /// making it live.
    ///
    /// # Errors
    ///
    /// Returns persistence, header-compatibility, or replay-validation failures.
    async fn read_session(&self, session_id: SessionId) -> anyhow::Result<SessionLogSnapshot> {
        let loaded = self.corpus().load(&session_id, None).await?;
        Session::create(
            &session_id,
            Some(loaded.events.clone()),
            Some(loaded.header.clone()),
        )?;
        Ok(SessionLogSnapshot {
            session: loaded.header,
            events: loaded.events,
        })
    }

    /// Filters the complete logical corpus with provider-independent predicates.
    ///
    /// # Errors
    ///
    /// Returns an invalid-filter, cancellation, or persistence-listing failure.
    async fn filter_sessions(
        &self,
        filters: &[SessionResultFilter],
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionRecord>> {
        let owned = materialize_session_result_filters(filters)?;
        let records = self.corpus().list_sessions(signal.as_ref()).await?;
        Ok(filter_session_results(&records, &owned)?)
    }

    /// Folds the latest log-backed title from one live-preferred logical session.
    ///
    /// # Errors
    ///
    /// Returns source-resolution or title-folding failures.
    async fn read_title(
        &self,
        session_id: SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionTitleSnapshot>> {
        Ok(self.read_title_snapshot(session_id, signal).await?.title)
    }

    /// Folds the latest title and returns its source header from one corpus
    /// observation.
    ///
    /// # Errors
    ///
    /// Returns source-resolution or title-folding failures.
    async fn read_title_snapshot(
        &self,
        session_id: SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionTitleObservation> {
        let mut results = self.read_title_snapshots(&[session_id], signal).await?;
        let result = results.pop().expect("one result per requested id");
        match result {
            LogicalProjectionResult::Fulfilled { value, .. } => Ok(value),
            LogicalProjectionResult::Rejected { reason, .. } => Err(into_error(reason)),
        }
    }

    /// Folds titles for unique sessions from one cancellable corpus observation.
    ///
    /// # Errors
    ///
    /// Returns cancellation; per-session failures stay isolated in each rejected
    /// result.
    async fn read_title_snapshots(
        &self,
        session_ids: &[SessionId],
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionTitleObservationResult>> {
        self.corpus()
            .project_many(
                session_ids,
                |source| SessionTitleObservation {
                    session: source.header.clone(),
                    title: fold_session_title(&source.events),
                },
                signal.as_ref(),
            )
            .await
    }

    /// Lists lightweight raw-log event records for one logical session.
    ///
    /// # Errors
    ///
    /// Returns source-resolution or invalid-surface failures.
    async fn list_events(&self, session_id: SessionId) -> anyhow::Result<Vec<SessionEventRecord>> {
        let loaded = self.corpus().load(&session_id, None).await?;
        Ok(tracing::event_records(&session_id, &loaded.events)?)
    }

    /// Scans first-party semantic event documents with provider-independent
    /// filters.
    ///
    /// # Errors
    ///
    /// Returns an invalid-filter, source-resolution, or invalid-surface failure.
    async fn filter_events(
        &self,
        session_id: SessionId,
        filters: &[SessionEventResultFilter],
    ) -> anyhow::Result<Vec<SessionEventSearchDocument>> {
        let owned = materialize_session_event_result_filters(filters)?;
        let loaded = self.corpus().load(&session_id, None).await?;
        let documents = build_session_event_search_documents(&session_id, &loaded.events)?;
        Ok(filter_session_event_documents(&documents, &owned)?)
    }

    /// Reads one session's complete current model surface from one corpus
    /// observation.
    ///
    /// # Errors
    ///
    /// Returns source-resolution or invalid-surface failures.
    async fn read_surface(&self, session_id: SessionId) -> anyhow::Result<SessionSurfaceSnapshot> {
        let loaded = self.corpus().load(&session_id, None).await?;
        let captured_through_seq = loaded.events.last().map(|event| event.seq);
        let events = tracing::current_surface_events(&session_id, &loaded.events)?;
        Ok(SessionSurfaceSnapshot {
            session: loaded.header,
            captured_through_seq,
            events,
        })
    }

    /// Traces known ancestry and descendants from one corpus observation.
    ///
    /// # Errors
    ///
    /// Returns cancellation, persistence, or lineage failures.
    async fn trace_session(
        &self,
        session_id: SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionLineageTrace> {
        let records = self.corpus().list_sessions(signal.as_ref()).await?;
        ensure_persistence_not_aborted(signal.as_ref())?;
        Ok(tracing::trace_session(&records, &session_id)?)
    }

    /// Traces one event's direct positional replacements and cited source events.
    ///
    /// # Errors
    ///
    /// Returns cancellation, source-resolution, not-found, or surface failures.
    async fn trace_event(
        &self,
        request: SessionEventTraceRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionEventTraceObservation> {
        let loaded = self
            .corpus()
            .load(&request.session_id, signal.as_ref())
            .await?;
        ensure_persistence_not_aborted(signal.as_ref())?;
        let trace = tracing::trace_event(&request.session_id, &loaded.events, request.seq)?;
        Ok(SessionEventTraceObservation {
            trace,
            session: loaded.header,
        })
    }

    /// Reads one full event plus a bounded raw-log context window.
    ///
    /// # Errors
    ///
    /// Returns an invalid-window, cancellation, or source-resolution failure.
    async fn read_event(
        &self,
        request: SessionEventReadRequest,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionEventWindow> {
        let before = self.read_window("before", request.before)?;
        let after = self.read_window("after", request.after)?;
        self.read_event_impl(
            request.session_id,
            request.seq,
            before,
            after,
            signal.as_ref(),
        )
        .await
    }

    /// Resolves one raw context window size against the configured maximum.
    ///
    /// # Errors
    ///
    /// Returns an invalid-window failure when the size exceeds the maximum.
    fn read_window(&self, name: &str, value: Option<u64>) -> anyhow::Result<u64> {
        let Some(value) = value else {
            return Ok(0);
        };
        if value > self.read_window_max() {
            return Err(SessionQueryError::new(
                format!(
                    "{name} must be an integer between 0 and {}",
                    self.read_window_max()
                ),
                SessionQueryErrorCode::SessionQueryInvalidWindow,
            )
            .into());
        }
        Ok(value)
    }

    /// Reads one event window after resolving the raw source.
    ///
    /// # Errors
    ///
    /// Returns cancellation, source-resolution, or not-found failures.
    async fn read_event_impl(
        &self,
        session_id: SessionId,
        seq: u64,
        before: u64,
        after: u64,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<SessionEventWindow> {
        let loaded = self.corpus().load(&session_id, signal).await?;
        ensure_persistence_not_aborted(signal)?;
        let LogicalSession { header, events } = loaded;
        let index = usize::try_from(seq).map_err(|_| event_not_found(&session_id, seq))?;
        let target = events
            .get(index)
            .filter(|event| event.seq == seq)
            .ok_or_else(|| event_not_found(&session_id, seq))?;
        let start_seq = seq.saturating_sub(before);
        let end_seq = seq
            .saturating_add(after)
            .min(u64::try_from(events.len() - 1).unwrap_or(u64::MAX));
        let start = usize::try_from(start_seq).map_err(|_| event_not_found(&session_id, seq))?;
        let end = usize::try_from(end_seq).map_err(|_| event_not_found(&session_id, seq))?;
        let target_snapshot = target.clone();
        let window_events = events[start..=end]
            .iter()
            .map(|event| {
                if event.seq == seq {
                    target_snapshot.clone()
                } else {
                    event.clone()
                }
            })
            .collect();
        Ok(SessionEventWindow {
            session: header,
            target: target_snapshot,
            events: window_events,
            start_seq,
            end_seq,
        })
    }
}

/// Typed Cordis seat corresponding to `ctx.sessionQuery`.
pub const SESSION_QUERY: ServiceKey<SessionQueryService> = ServiceKey::new("sessionQuery");

/// Dynamically dispatched exact backend occupying the session-query seat.
#[derive(Clone)]
pub struct SessionQueryService(Arc<dyn SessionQueryEngine>);

impl std::fmt::Debug for SessionQueryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SessionQueryService")
            .field(&"dyn SessionQueryEngine")
            .finish()
    }
}

impl SessionQueryService {
    /// Wraps one concrete query engine.
    #[must_use]
    pub fn new(engine: Arc<dyn SessionQueryEngine>) -> Arc<Self> {
        Arc::new(Self(engine))
    }

    /// Returns the object-safe query engine.
    #[must_use]
    pub fn engine(&self) -> Arc<dyn SessionQueryEngine> {
        self.0.clone()
    }

    /// Publishes this engine on the source-compatible Cordis seat.
    ///
    /// # Errors
    ///
    /// Returns inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(SESSION_QUERY, self.clone())?)
    }
}

impl Deref for SessionQueryService {
    type Target = dyn SessionQueryEngine;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// Resolves and validates the backend-independent query configuration.
///
/// # Errors
///
/// Returns an invalid-config failure for a non-positive or unsafe persisted
/// inspection concurrency.
pub fn resolve_config(config: &Config) -> Result<(u64, u64), SessionQueryError> {
    let read_window_max = config
        .read_window_max
        .unwrap_or(SESSION_QUERY_READ_WINDOW_MAX);
    let persisted_inspect_concurrency = config
        .persisted_inspect_concurrency
        .unwrap_or(SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY);
    if !(1..=MAX_SAFE_INTEGER).contains(&persisted_inspect_concurrency) {
        return Err(SessionQueryError::new(
            "session-query: persistedInspectConcurrency must be a positive safe integer",
            SessionQueryErrorCode::SessionQueryInvalidConfig,
        ));
    }
    Ok((read_window_max, persisted_inspect_concurrency))
}

fn into_error(reason: Arc<anyhow::Error>) -> anyhow::Error {
    Arc::try_unwrap(reason).unwrap_or_else(|arc| anyhow::Error::msg(arc.to_string()))
}

fn event_not_found(session_id: &SessionId, seq: u64) -> SessionQueryError {
    SessionQueryError::new(
        format!("session \"{session_id}\" has no event at seq {seq}"),
        SessionQueryErrorCode::SessionQueryEventNotFound,
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{AppendOptions, SurfaceOp};
    use seekdeep_core::session_store::CreateSessionOptions;
    use seekdeep_llm::{ContentBlock, Message, MessageSource};
    use serde_json::json;

    use super::*;
    use crate::types::SessionAvailability;

    struct MockEngine {
        corpus: Arc<SessionCorpus>,
        read_window_max: u64,
    }

    #[async_trait]
    impl SessionQueryEngine for MockEngine {
        fn corpus(&self) -> &SessionCorpus {
            &self.corpus
        }

        fn read_window_max(&self) -> u64 {
            self.read_window_max
        }

        async fn search_sessions(
            &self,
            _request: SessionSearchRequest,
            _exec: Option<SessionSearchExecContext>,
        ) -> anyhow::Result<SessionSearchPage<SessionSearchHit>> {
            Ok(SessionSearchPage {
                items: Vec::new(),
                next_cursor: None,
            })
        }

        async fn search_events(
            &self,
            request: SessionEventSearchRequest,
            _exec: Option<SessionSearchExecContext>,
        ) -> anyhow::Result<SessionEventSearchPage> {
            let surface = self.read_surface(request.session_id).await?;
            Ok(SessionEventSearchPage {
                page: SessionSearchPage {
                    items: Vec::new(),
                    next_cursor: None,
                },
                session: surface.session,
            })
        }
    }

    fn append_user(session: &Session, text: &str) {
        let message = Message::user(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::user(),
        );
        session
            .append(
                "user/message",
                serde_json::to_value(message).expect("serialize message"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("append user message");
    }

    fn engine(ctx: &Context) -> Arc<MockEngine> {
        Arc::new(MockEngine {
            corpus: SessionCorpus::new(ctx, 4),
            read_window_max: 50,
        })
    }

    #[tokio::test]
    async fn lists_live_sessions_and_their_events() {
        let ctx = Context::new();
        let store = seekdeep_core::session_store::SessionStore::install(&ctx).expect("store");
        let session = store
            .create(
                &ctx,
                Some(SessionId::new("s")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append_user(&session, "hello");

        let engine = engine(&ctx);
        let sessions = engine.list_sessions(None).await.expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].header.id.as_str(), "s");
        assert!(sessions[0].live);
        assert!(!sessions[0].persisted);

        let events = engine
            .list_events(SessionId::new("s"))
            .await
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "user/message");

        let surface = engine
            .read_surface(SessionId::new("s"))
            .await
            .expect("surface");
        assert_eq!(surface.captured_through_seq, Some(0));
        assert_eq!(surface.events.len(), 1);
    }

    #[tokio::test]
    async fn replay_validates_and_reads_titles_and_windows() {
        let ctx = Context::new();
        let store = seekdeep_core::session_store::SessionStore::install(&ctx).expect("store");
        let session = store
            .create(
                &ctx,
                Some(SessionId::new("t")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append_user(&session, "first");
        session
            .append(
                "session/title",
                json!({ "title": "My title", "messageSeqs": [0], "source": { "kind": "fallback" } }),
                AppendOptions::default(),
            )
            .expect("title");

        let engine = engine(&ctx);
        let snapshot = engine
            .read_session(SessionId::new("t"))
            .await
            .expect("replay");
        assert_eq!(snapshot.events.len(), 2);

        let title = engine
            .read_title(SessionId::new("t"), None)
            .await
            .expect("title")
            .expect("some title");
        assert_eq!(title.event.title, "My title");

        let window = engine
            .read_event(
                SessionEventReadRequest {
                    session_id: SessionId::new("t"),
                    seq: 0,
                    before: Some(1),
                    after: Some(1),
                },
                None,
            )
            .await
            .expect("window");
        assert_eq!(window.start_seq, 0);
        assert_eq!(window.end_seq, 1);
        assert_eq!(window.target.seq, 0);

        let error = engine
            .read_event(
                SessionEventReadRequest {
                    session_id: SessionId::new("t"),
                    seq: 0,
                    before: Some(51),
                    after: None,
                },
                None,
            )
            .await
            .expect_err("oversized window");
        assert!(format!("{error:#}").contains("between 0 and 50"));
    }

    #[tokio::test]
    async fn filters_sessions_and_events() {
        let ctx = Context::new();
        let store = seekdeep_core::session_store::SessionStore::install(&ctx).expect("store");
        let session = store
            .create(
                &ctx,
                Some(SessionId::new("f")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        append_user(&session, "Alpha beta");

        let engine = engine(&ctx);
        let hits = engine
            .filter_sessions(
                &[SessionResultFilter::Availability {
                    values: vec![SessionAvailability::Live],
                }],
                None,
            )
            .await
            .expect("filter");
        assert_eq!(hits.len(), 1);

        let docs = engine
            .filter_events(
                SessionId::new("f"),
                &[SessionEventResultFilter::Text {
                    text: "alpha beta".to_owned(),
                }],
            )
            .await
            .expect("events");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].text, "Alpha beta");
    }

    #[test]
    fn validates_query_config() {
        let (read_max, concurrency) = resolve_config(&Config::default()).expect("defaults");
        assert_eq!(read_max, 50);
        assert_eq!(concurrency, 4);
        let error = resolve_config(&Config {
            persisted_inspect_concurrency: Some(0),
            ..Config::default()
        })
        .expect_err("zero concurrency");
        assert_eq!(error.code, SessionQueryErrorCode::SessionQueryInvalidConfig);
    }
}
