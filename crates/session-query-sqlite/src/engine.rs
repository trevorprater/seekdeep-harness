//! Serialized live-preferred reconciliation and FTS5 search execution.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use rusqlite::{
    Connection, OptionalExtension as _, TransactionBehavior, params_from_iter,
    types::Value as SqlValue,
};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionId},
    session_store::SESSIONS,
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionPersistence, SessionPersistenceRevision, SessionPersistenceSnapshot,
};
use seekdeep_session_query::{
    SessionCorpus, SessionEventSearchHit, SessionEventSurface, SessionQueryEngine,
    SessionQueryError, SessionQueryErrorCode, SessionRecord, SessionSearchCursor, SessionSearchHit,
    SessionSearchPage, assert_session_headers_compatible, build_session_event_search_documents,
    types::{
        SessionEventSearchDocument, SessionEventSearchPage, SessionEventSearchRequest,
        SessionSearchExecContext, SessionSearchRequest,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use crate::{
    query::{
        FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START, NormalizedEventRequest, NormalizedRequest,
        NormalizedSessionRequest, QueryLimits, SQLITE_MAX_PAGE_LIMIT, SqlParam,
        assert_fts5_outer_predicate_count, assert_portable_binding_count, build_event_where,
        build_session_where, make_snippet, normalize_event_request, normalize_session_request,
        quote_fts_data, request_fingerprint, sanitize_fts_text,
    },
    schema::{JournalMode, open_search_database},
};

/// Default result page size.
pub const SESSION_QUERY_SQLITE_DEFAULT_LIMIT: u64 = 20;
/// Maximum accepted result page size.
pub const SESSION_QUERY_SQLITE_MAX_LIMIT: u64 = 100;
/// Default maximum snippet length in Unicode code points.
pub const SESSION_QUERY_SQLITE_SNIPPET_CHARS: usize = 240;
const STABLE_OBSERVATION_ATTEMPTS: usize = 2;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// Database opening phase; `Never` disables only full-text calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpenAt {
    /// Open during provider construction.
    #[default]
    Startup,
    /// Open at the first search.
    FirstSearch,
    /// Keep exact reads while rejecting full-text calls.
    Never,
}

/// Combined session-query and `SQLite` search configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SqliteSessionQueryConfig {
    /// Dedicated derived-index path; `:memory:` is supported.
    pub path: String,
    /// Opening phase.
    pub open_at: OpenAt,
    /// Journal mode.
    pub journal_mode: String,
    /// Default page size.
    pub default_limit: u64,
    /// Maximum page size.
    pub max_limit: u64,
    /// Maximum snippet code points.
    pub snippet_chars: usize,
    /// Exact-read window maximum.
    pub read_window_max: u64,
    /// Persisted inspection concurrency inherited by the corpus.
    pub persisted_inspect_concurrency: u64,
}

impl Default for SqliteSessionQueryConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            open_at: OpenAt::Startup,
            journal_mode: "wal".to_owned(),
            default_limit: SESSION_QUERY_SQLITE_DEFAULT_LIMIT,
            max_limit: SESSION_QUERY_SQLITE_MAX_LIMIT,
            snippet_chars: SESSION_QUERY_SQLITE_SNIPPET_CHARS,
            read_window_max: seekdeep_session_query::SESSION_QUERY_READ_WINDOW_MAX,
            persisted_inspect_concurrency:
                seekdeep_session_query::SESSION_QUERY_DEFAULT_PERSISTED_INSPECT_CONCURRENCY,
        }
    }
}

impl SqliteSessionQueryConfig {
    fn resolve(self) -> anyhow::Result<ResolvedSqliteSessionQueryConfig> {
        anyhow::ensure!(
            !self.path.trim().is_empty(),
            invalid_config("path must not be blank")
        );
        anyhow::ensure!(
            (1..=SQLITE_MAX_PAGE_LIMIT).contains(&self.default_limit),
            invalid_config(&format!(
                "defaultLimit must be an integer between 1 and {SQLITE_MAX_PAGE_LIMIT}"
            ))
        );
        anyhow::ensure!(
            (1..=SQLITE_MAX_PAGE_LIMIT).contains(&self.max_limit),
            invalid_config(&format!(
                "maxLimit must be an integer between 1 and {SQLITE_MAX_PAGE_LIMIT}"
            ))
        );
        anyhow::ensure!(
            self.default_limit <= self.max_limit,
            invalid_config("defaultLimit must be less than or equal to maxLimit")
        );
        anyhow::ensure!(
            self.snippet_chars > 0,
            invalid_config("snippetChars must be a positive integer")
        );
        anyhow::ensure!(
            (1..=MAX_SAFE_INTEGER).contains(&self.persisted_inspect_concurrency),
            invalid_config("persistedInspectConcurrency must be a positive safe integer")
        );
        let journal_mode = match self.journal_mode.as_str() {
            "wal" => JournalMode::Wal,
            "delete" => JournalMode::Delete,
            "truncate" => JournalMode::Truncate,
            "persist" => JournalMode::Persist,
            _ => return Err(invalid_config("journalMode is not supported")),
        };
        Ok(ResolvedSqliteSessionQueryConfig {
            path: self.path,
            open_at: self.open_at,
            journal_mode,
            default_limit: self.default_limit,
            max_limit: self.max_limit,
            snippet_chars: self.snippet_chars,
            read_window_max: self.read_window_max,
            persisted_inspect_concurrency: self.persisted_inspect_concurrency,
        })
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        self.clone().resolve().map(|_| ())
    }
}

/// Validated and defaulted `SQLite` session-query configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSqliteSessionQueryConfig {
    /// Dedicated derived-index path.
    pub path: String,
    /// Database opening phase.
    pub open_at: OpenAt,
    /// Effective journal mode.
    pub journal_mode: JournalMode,
    /// Effective default page size.
    pub default_limit: u64,
    /// Effective maximum page size.
    pub max_limit: u64,
    /// Effective snippet bound in Unicode code points.
    pub snippet_chars: usize,
    /// Effective exact-read window maximum.
    pub read_window_max: u64,
    /// Effective persisted inspection concurrency.
    pub persisted_inspect_concurrency: u64,
}

impl ResolvedSqliteSessionQueryConfig {
    const fn limits(&self) -> QueryLimits {
        QueryLimits {
            default_limit: self.default_limit,
            max_limit: self.max_limit,
        }
    }
}

#[derive(Clone, Debug)]
struct ObservedSession {
    header: SessionHeader,
    documents: Vec<SessionEventSearchDocument>,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct ObservedPersistedSession {
    header: SessionHeader,
    revision: SessionPersistenceRevision,
    loaded: Option<ObservedSession>,
}

#[derive(Clone)]
struct PersistenceBinding {
    identity: u64,
    service: Option<Arc<dyn SessionPersistence>>,
}

struct Observation {
    binding: PersistenceBinding,
    persisted: HashMap<SessionId, ObservedPersistedSession>,
    live: HashMap<SessionId, ObservedSession>,
}

#[derive(Clone, Debug)]
struct IndexedPersistedRow {
    revision: String,
}

#[derive(Clone, Debug)]
struct IndexedLiveRow {
    fingerprint: String,
    persisted: bool,
}

#[derive(Clone, Debug)]
struct SearchRow {
    header: SessionHeader,
    live: bool,
    persisted: bool,
    seq: u64,
    event_type: String,
    time: i64,
    surface: SessionEventSurface,
    marked_text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorPayload {
    version: u8,
    instance: String,
    scope: String,
    fingerprint: String,
    generation: String,
    offset: u64,
}

#[derive(Debug)]
struct EngineState {
    connection: Option<Connection>,
    open_failure: Option<String>,
    last_persistence_identity: Option<u64>,
    persistence_epoch: u64,
    global_generation: i64,
    local_generation: i64,
}

/// Concrete `SQLite` owner of the combined session-query service.
pub struct SqliteSessionQueryEngine {
    context: Context,
    corpus: Arc<SessionCorpus>,
    /// Validated and defaulted backend configuration.
    pub config: ResolvedSqliteSessionQueryConfig,
    instance: String,
    state: Mutex<EngineState>,
    open: tokio::sync::Mutex<()>,
    operation: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl std::fmt::Debug for SqliteSessionQueryEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteSessionQueryEngine")
            .field("config", &self.config)
            .field("instance", &self.instance)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SqliteSessionQueryEngine {
    /// Creates a provider and eagerly opens only in startup mode.
    ///
    /// # Errors
    ///
    /// Returns configuration or eager database-opening failures.
    pub fn new(context: &Context, config: SqliteSessionQueryConfig) -> anyhow::Result<Arc<Self>> {
        let config = config.resolve()?;
        let corpus = SessionCorpus::new(context, config.persisted_inspect_concurrency);
        let engine = Arc::new(Self {
            context: context.clone(),
            corpus,
            instance: format!(
                "sqlite-session-query-{}",
                NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed)
            ),
            state: Mutex::new(EngineState {
                connection: None,
                open_failure: None,
                last_persistence_identity: None,
                persistence_epoch: 0,
                global_generation: 0,
                local_generation: 0,
            }),
            open: tokio::sync::Mutex::new(()),
            operation: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
            config,
        });
        if engine.config.open_at == OpenAt::Startup {
            engine.open_sync()?;
        }
        Ok(engine)
    }

    /// Closes after every accepted serialized operation settles.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let _operation = self.operation.lock().await;
        self.state.lock().connection.take();
    }

    fn open_sync(&self) -> anyhow::Result<()> {
        {
            let state = self.state.lock();
            if state.connection.is_some() {
                return Ok(());
            }
            if let Some(message) = &state.open_failure {
                return Err(index_failed(message.clone()));
            }
        }
        let opened = (|| {
            let connection = open_search_database(&self.config.path, self.config.journal_mode)?;
            let generation: i64 = connection.query_row(
                "SELECT global_generation FROM search_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            anyhow::Ok((connection, generation))
        })();
        let (connection, generation) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                let message = format!("session-search SQLite index failed to open: {error}");
                self.state.lock().open_failure = Some(message.clone());
                return Err(index_failed(message));
            }
        };
        let mut state = self.state.lock();
        state.global_generation = generation;
        state.local_generation = generation;
        state.connection = Some(connection);
        Ok(())
    }

    async fn ensure_ready(&self, signal: Option<&AbortSignal>) -> anyhow::Result<()> {
        ensure_not_aborted(signal)?;
        let _open = self.open.lock().await;
        ensure_not_aborted(signal)?;
        self.open_sync()
    }

    fn assert_enabled(&self) -> anyhow::Result<()> {
        if self.config.open_at == OpenAt::Never {
            return Err(SessionQueryError::new(
                "session search is disabled: this deployment configures the session-query index with openAt \"never\"",
                SessionQueryErrorCode::SessionQuerySearchDisabled,
            )
            .into());
        }
        Ok(())
    }

    async fn serialized<T>(
        &self,
        signal: Option<&AbortSignal>,
        operation: impl AsyncFnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if self.closed.load(Ordering::Acquire) {
            return Err(index_closed());
        }
        let lock = self.operation.lock();
        tokio::pin!(lock);
        let _guard = if let Some(signal) = signal {
            tokio::select! {
                guard = &mut lock => guard,
                () = signal.cancelled() => return Err(aborted()),
            }
        } else {
            lock.await
        };
        if self.closed.load(Ordering::Acquire) {
            return Err(index_closed());
        }
        ensure_not_aborted(signal)?;
        operation().await
    }

    fn persistence_binding(&self) -> PersistenceBinding {
        let service = self
            .context
            .get(SESSION_PERSISTENCE)
            .map(|service| service.persistence());
        let identity = self.context.service_slot_revision(SESSION_PERSISTENCE);
        PersistenceBinding { identity, service }
    }

    fn indexed_rows(
        &self,
    ) -> anyhow::Result<(
        HashMap<SessionId, IndexedPersistedRow>,
        HashMap<SessionId, IndexedLiveRow>,
    )> {
        let state = self.state.lock();
        let connection = state.connection.as_ref().ok_or_else(index_closed)?;
        Ok((
            read_persisted_rows(connection)?,
            read_live_rows(connection)?,
        ))
    }

    async fn reconcile(&self, signal: Option<&AbortSignal>) -> anyhow::Result<PersistenceBinding> {
        ensure_not_aborted(signal)?;
        let (persisted_rows, live_rows) = self.indexed_rows()?;
        let observation = self.observe_stable(&persisted_rows, signal).await?;
        ensure_not_aborted(signal)?;
        let persisted_replacements = if observation.binding.service.is_some() {
            observation
                .persisted
                .values()
                .filter_map(|entry| {
                    entry
                        .loaded
                        .clone()
                        .map(|loaded| (loaded, entry.revision.clone()))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let persisted_deletes = if observation.binding.service.is_some() {
            persisted_rows
                .keys()
                .filter(|id| !observation.persisted.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let live_changes = observation
            .live
            .values()
            .filter(|entry| {
                let indexed = live_rows.get(&entry.header.id);
                let persisted = observation.persisted.contains_key(&entry.header.id);
                indexed.is_none_or(|indexed| {
                    indexed.fingerprint != entry.fingerprint || indexed.persisted != persisted
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let live_deletes = live_rows
            .keys()
            .filter(|id| !observation.live.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        let mut state = self.state.lock();
        let pointer_changed = state
            .last_persistence_identity
            .is_some_and(|identity| identity != observation.binding.identity);
        let persisted_changed = !persisted_replacements.is_empty() || !persisted_deletes.is_empty();
        let has_writes = persisted_changed || !live_changes.is_empty() || !live_deletes.is_empty();
        let mut main_generation: i64 = state
            .connection
            .as_ref()
            .ok_or_else(index_closed)?
            .query_row(
                "SELECT global_generation FROM search_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
        let mut local_generation = state.local_generation;
        if persisted_changed {
            main_generation = next_generation(main_generation)?;
        }
        let live_replacements = live_changes
            .into_iter()
            .map(|entry| -> anyhow::Result<_> {
                local_generation = next_generation(local_generation.max(main_generation))?;
                let persisted = observation.persisted.contains_key(&entry.header.id);
                Ok((entry, local_generation, persisted))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if has_writes {
            let connection = state.connection.as_mut().ok_or_else(index_closed)?;
            apply_reconciliation(
                connection,
                &persisted_deletes,
                &persisted_replacements,
                &live_deletes,
                &live_replacements,
                main_generation,
                persisted_changed,
            )?;
        }
        if has_writes || pointer_changed {
            state.global_generation = next_generation(state.global_generation)?;
        }
        if pointer_changed {
            state.persistence_epoch += 1;
        }
        state.local_generation = local_generation;
        state.last_persistence_identity = Some(observation.binding.identity);
        Ok(observation.binding)
    }

    async fn observe_stable(
        &self,
        indexed: &HashMap<SessionId, IndexedPersistedRow>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Observation> {
        for _ in 0..STABLE_OBSERVATION_ATTEMPTS {
            ensure_not_aborted(signal)?;
            let binding = self.persistence_binding();
            let initially_live = live_ids(&self.context);
            let persisted = match self
                .observe_persisted(&binding, indexed, &initially_live, signal)
                .await
            {
                Ok(Some(persisted)) => persisted,
                Ok(None) => continue,
                Err(error) => {
                    if signal.is_some_and(AbortSignal::is_aborted) || is_abort(&error) {
                        return Err(aborted());
                    }
                    if self.persistence_binding().identity != binding.identity {
                        continue;
                    }
                    if error.downcast_ref::<SessionQueryError>().is_some() {
                        return Err(error);
                    }
                    return Err(observation_failed(&error));
                }
            };
            let mut live = HashMap::new();
            for session in self.live_sessions() {
                let events = session.events();
                let observed = observe_session(session.header().clone(), &events)?;
                if let Some(durable) = persisted.get(session.id()) {
                    assert_session_headers_compatible(&observed.header, &durable.header)?;
                }
                live.insert(session.id().clone(), observed);
            }
            if initially_live != live.keys().cloned().collect() {
                continue;
            }
            return Ok(Observation {
                binding,
                persisted,
                live,
            });
        }
        Err(SessionQueryError::new(
            "session-search persistence observation did not stabilize after one retry",
            SessionQueryErrorCode::SessionQueryPersistenceFailed,
        )
        .into())
    }

    async fn observe_persisted(
        &self,
        binding: &PersistenceBinding,
        indexed: &HashMap<SessionId, IndexedPersistedRow>,
        initially_live: &HashSet<SessionId>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<HashMap<SessionId, ObservedPersistedSession>>> {
        let Some(persistence) = &binding.service else {
            return Ok(Some(HashMap::new()));
        };
        let before = list_snapshots(persistence.as_ref(), signal).await?;
        ensure_not_aborted(signal)?;
        let mut persisted = materialize_snapshots(before)?;
        let can_reuse = self
            .state
            .lock()
            .last_persistence_identity
            .is_none_or(|identity| identity == binding.identity);
        for entry in persisted.values_mut() {
            if can_reuse
                && indexed
                    .get(&entry.header.id)
                    .is_some_and(|row| row.revision == entry.revision.as_str())
            {
                continue;
            }
            if initially_live.contains(&entry.header.id)
                || self.live_session(&entry.header.id).is_some()
            {
                continue;
            }
            ensure_not_aborted(signal)?;
            let inspection = persistence
                .inspect(&entry.header.id, signal.cloned())
                .await?;
            ensure_not_aborted(signal)?;
            assert_session_headers_compatible(&entry.header, &inspection.meta)?;
            entry.loaded = Some(observe_session(inspection.meta, &inspection.events)?);
        }
        let after = materialize_snapshots(list_snapshots(persistence.as_ref(), signal).await?)?;
        ensure_not_aborted(signal)?;
        if !same_snapshots(&persisted, &after)
            || self.persistence_binding().identity != binding.identity
        {
            return Ok(None);
        }
        Ok(Some(persisted))
    }

    fn live_sessions(&self) -> Vec<Arc<Session>> {
        self.context
            .get(SESSIONS)
            .map(|sessions| sessions.list())
            .unwrap_or_default()
    }

    fn live_session(&self, id: &SessionId) -> Option<Arc<Session>> {
        self.context
            .get(SESSIONS)
            .and_then(|sessions| sessions.get(id))
    }

    fn query_sessions(
        &self,
        request: &NormalizedSessionRequest,
        offset: u64,
        persistence_visible: bool,
    ) -> anyhow::Result<Vec<SearchRow>> {
        let session_where = build_session_where(&request.session_filters)?;
        let event_where = build_event_where(&request.event_filters)?;
        assert_fts5_outer_predicate_count(
            session_where.predicate_count + event_where.predicate_count,
        )?;
        let where_clause = [session_where.sql.as_str(), event_where.sql.as_str()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut bindings = selected_params(&request.query, persistence_visible);
        bindings.extend(sql_params(session_where.params));
        bindings.extend(sql_params(event_where.params));
        bindings.push(SqlValue::Integer(i64::try_from(request.limit + 1)?));
        bindings.push(SqlValue::Integer(i64::try_from(offset)?));
        assert_portable_binding_count(bindings.len())?;
        let sql = format!(
            "{}, filtered AS (SELECT * FROM matched {}), ranked AS (SELECT *, ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY match_count DESC, document_length ASC, time DESC, seq DESC) AS event_rank FROM filtered) SELECT * FROM ranked WHERE event_rank = 1 ORDER BY match_count DESC, document_length ASC, time DESC, session_id ASC, seq DESC LIMIT ? OFFSET ?",
            selected_documents_sql(),
            if where_clause.is_empty() {
                String::new()
            } else {
                format!("WHERE {where_clause}")
            }
        );
        self.read_search_rows(&sql, bindings)
    }

    fn query_events(
        &self,
        request: &NormalizedEventRequest,
        offset: u64,
        persistence_visible: bool,
    ) -> anyhow::Result<Vec<SearchRow>> {
        let event_where = build_event_where(&request.filters)?;
        assert_fts5_outer_predicate_count(1 + event_where.predicate_count)?;
        let where_clause = if event_where.sql.is_empty() {
            "session_id = ?".to_owned()
        } else {
            format!("session_id = ? AND {}", event_where.sql)
        };
        let mut bindings = selected_params(&request.query, persistence_visible);
        bindings.push(SqlValue::Text(request.session_id.as_str().to_owned()));
        bindings.extend(sql_params(event_where.params));
        bindings.push(SqlValue::Integer(i64::try_from(request.limit + 1)?));
        bindings.push(SqlValue::Integer(i64::try_from(offset)?));
        assert_portable_binding_count(bindings.len())?;
        let sql = format!(
            "{} SELECT * FROM matched WHERE {where_clause} ORDER BY match_count DESC, document_length ASC, time DESC, seq DESC LIMIT ? OFFSET ?",
            selected_documents_sql()
        );
        self.read_search_rows(&sql, bindings)
    }

    fn read_search_rows(
        &self,
        sql: &str,
        bindings: Vec<SqlValue>,
    ) -> anyhow::Result<Vec<SearchRow>> {
        let state = self.state.lock();
        let connection = state.connection.as_ref().ok_or_else(index_closed)?;
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map(params_from_iter(bindings), row_to_search)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn target_observation(
        &self,
        id: &SessionId,
        persistence_visible: bool,
    ) -> anyhow::Result<(SessionHeader, String)> {
        let state = self.state.lock();
        let connection = state.connection.as_ref().ok_or_else(index_closed)?;
        if let Some((header, generation)) = read_target(connection, true, id)? {
            return Ok((header, format!("live:{generation}")));
        }
        if persistence_visible
            && let Some((header, generation)) = read_target(connection, false, id)?
        {
            return Ok((
                header,
                format!("persisted:{}:{generation}", state.persistence_epoch),
            ));
        }
        Err(SessionQueryError::new(
            format!("session \"{id}\" not found"),
            SessionQueryErrorCode::SessionQuerySessionNotFound,
        )
        .into())
    }

    fn event_hit(&self, row: &SearchRow) -> SessionEventSearchHit {
        SessionEventSearchHit {
            record: seekdeep_session_query::SessionEventRecord {
                session_id: row.header.id.clone(),
                seq: row.seq,
                event_type: row.event_type.clone(),
                time: row.time,
                surface: row.surface,
            },
            snippet: make_snippet(&row.marked_text, self.config.snippet_chars),
        }
    }
}

#[async_trait]
impl SessionQueryEngine for SqliteSessionQueryEngine {
    fn corpus(&self) -> &SessionCorpus {
        &self.corpus
    }

    fn read_window_max(&self) -> u64 {
        self.config.read_window_max
    }

    async fn search_sessions(
        &self,
        request: SessionSearchRequest,
        exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionSearchPage<SessionSearchHit>> {
        self.assert_enabled()?;
        let normalized = normalize_session_request(request, self.config.limits())?;
        let signal = exec.as_ref().and_then(|exec| exec.signal.as_ref());
        self.serialized(signal, async || {
            self.ensure_ready(signal).await?;
            let binding = self.reconcile(signal).await?;
            ensure_not_aborted(signal)?;
            let generation = self.state.lock().global_generation.to_string();
            let fingerprint = request_fingerprint(&NormalizedRequest::Sessions(&normalized));
            let offset = normalized.cursor.as_ref().map_or(Ok(0), |cursor| {
                decode_cursor(
                    cursor,
                    &self.instance,
                    "sessions",
                    &fingerprint,
                    &generation,
                )
            })?;
            let rows = self.query_sessions(&normalized, offset, binding.service.is_some())?;
            let has_more = rows.len() > usize::try_from(normalized.limit)?;
            let items = rows
                .iter()
                .take(usize::try_from(normalized.limit)?)
                .map(|row| SessionSearchHit {
                    record: SessionRecord {
                        header: row.header.clone(),
                        live: row.live,
                        persisted: row.persisted,
                    },
                    best_match: self.event_hit(row),
                })
                .collect();
            Ok(SessionSearchPage {
                items,
                next_cursor: has_more.then(|| {
                    encode_cursor(&CursorPayload {
                        version: 1,
                        instance: self.instance.clone(),
                        scope: "sessions".to_owned(),
                        fingerprint,
                        generation,
                        offset: offset + normalized.limit,
                    })
                }),
            })
        })
        .await
    }

    async fn search_events(
        &self,
        request: SessionEventSearchRequest,
        exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionEventSearchPage> {
        self.assert_enabled()?;
        let normalized = normalize_event_request(request, self.config.limits())?;
        let signal = exec.as_ref().and_then(|exec| exec.signal.as_ref());
        self.serialized(signal, async || {
            self.ensure_ready(signal).await?;
            let binding = self.reconcile(signal).await?;
            ensure_not_aborted(signal)?;
            let (header, generation) =
                self.target_observation(&normalized.session_id, binding.service.is_some())?;
            let fingerprint = request_fingerprint(&NormalizedRequest::Events(&normalized));
            let offset = normalized.cursor.as_ref().map_or(Ok(0), |cursor| {
                decode_cursor(cursor, &self.instance, "events", &fingerprint, &generation)
            })?;
            let rows = self.query_events(&normalized, offset, binding.service.is_some())?;
            let has_more = rows.len() > usize::try_from(normalized.limit)?;
            let items = rows
                .iter()
                .take(usize::try_from(normalized.limit)?)
                .map(|row| self.event_hit(row))
                .collect();
            Ok(SessionEventSearchPage {
                page: SessionSearchPage {
                    items,
                    next_cursor: has_more.then(|| {
                        encode_cursor(&CursorPayload {
                            version: 1,
                            instance: self.instance.clone(),
                            scope: "events".to_owned(),
                            fingerprint,
                            generation,
                            offset: offset + normalized.limit,
                        })
                    }),
                },
                session: header,
            })
        })
        .await
    }
}

fn live_ids(context: &Context) -> HashSet<SessionId> {
    context
        .get(SESSIONS)
        .map(|sessions| {
            sessions
                .list()
                .into_iter()
                .map(|session| session.id().clone())
                .collect()
        })
        .unwrap_or_default()
}

async fn list_snapshots(
    persistence: &dyn SessionPersistence,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
    persistence.list_snapshots(signal.cloned()).await
}

fn materialize_snapshots(
    snapshots: Vec<SessionPersistenceSnapshot>,
) -> anyhow::Result<HashMap<SessionId, ObservedPersistedSession>> {
    let mut result = HashMap::new();
    for snapshot in snapshots {
        let id = snapshot.header.id.clone();
        anyhow::ensure!(
            !result.contains_key(&id),
            "persistence listed duplicate session \"{id}\""
        );
        result.insert(
            id,
            ObservedPersistedSession {
                header: snapshot.header,
                revision: snapshot.revision,
                loaded: None,
            },
        );
    }
    Ok(result)
}

fn same_snapshots(
    left: &HashMap<SessionId, ObservedPersistedSession>,
    right: &HashMap<SessionId, ObservedPersistedSession>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, first)| {
            right.get(id).is_some_and(|second| {
                first.revision == second.revision && first.header == second.header
            })
        })
}

fn observe_session(
    header: SessionHeader,
    events: &[SessionEvent],
) -> anyhow::Result<ObservedSession> {
    let documents = build_session_event_search_documents(&header.id, events)?;
    let encoded = serde_json::to_vec(&json!({"header": &header, "events": events}))?;
    let fingerprint = URL_SAFE_NO_PAD.encode(Sha256::digest(encoded));
    Ok(ObservedSession {
        header,
        documents,
        fingerprint,
    })
}

fn read_persisted_rows(
    connection: &Connection,
) -> anyhow::Result<HashMap<SessionId, IndexedPersistedRow>> {
    let mut statement = connection.prepare("SELECT id, revision FROM persisted_sessions")?;
    Ok(statement
        .query_map([], |row| {
            Ok((
                SessionId::new(row.get::<_, String>(0)?),
                IndexedPersistedRow {
                    revision: row.get(1)?,
                },
            ))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?)
}

fn read_live_rows(connection: &Connection) -> anyhow::Result<HashMap<SessionId, IndexedLiveRow>> {
    let mut statement =
        connection.prepare("SELECT id, fingerprint, persisted FROM temp.live_sessions")?;
    Ok(statement
        .query_map([], |row| {
            Ok((
                SessionId::new(row.get::<_, String>(0)?),
                IndexedLiveRow {
                    fingerprint: row.get(1)?,
                    persisted: row.get::<_, i64>(2)? == 1,
                },
            ))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?)
}

fn apply_reconciliation(
    connection: &mut Connection,
    persisted_deletes: &[SessionId],
    persisted_replacements: &[(ObservedSession, SessionPersistenceRevision)],
    live_deletes: &[SessionId],
    live_replacements: &[(ObservedSession, i64, bool)],
    main_generation: i64,
    persisted_changed: bool,
) -> anyhow::Result<()> {
    let result = (|| -> rusqlite::Result<()> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in persisted_deletes {
            delete_session(&transaction, false, id)?;
        }
        for (entry, revision) in persisted_replacements {
            replace_persisted(&transaction, entry, revision, main_generation)?;
        }
        if persisted_changed {
            transaction.execute(
                "UPDATE search_state SET global_generation = ? WHERE singleton = 1",
                [main_generation],
            )?;
        }
        for id in live_deletes {
            delete_session(&transaction, true, id)?;
        }
        for (entry, generation, persisted) in live_replacements {
            replace_live(&transaction, entry, *generation, *persisted)?;
        }
        transaction.commit()
    })();
    result.map_err(|error| reconcile_failed(&error))
}

fn delete_session(connection: &Connection, live: bool, id: &SessionId) -> rusqlite::Result<()> {
    let source = if live { "temp.live" } else { "persisted" };
    connection.execute(
        &format!("DELETE FROM {source}_docs WHERE session_id = ?"),
        [id.as_str()],
    )?;
    connection.execute(
        &format!("DELETE FROM {source}_sessions WHERE id = ?"),
        [id.as_str()],
    )?;
    Ok(())
}

fn replace_persisted(
    connection: &Connection,
    entry: &ObservedSession,
    revision: &SessionPersistenceRevision,
    generation: i64,
) -> rusqlite::Result<()> {
    delete_session(connection, false, &entry.header.id)?;
    let bindings = header_bindings(&entry.header);
    connection.execute(
        "INSERT INTO persisted_sessions (id,version,created_at,cwd,parent_session,seed_length,delegation_depth,agent_preset,revision,generation) VALUES (?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![bindings.0,bindings.1,bindings.2,bindings.3,bindings.4,bindings.5,bindings.6,bindings.7,revision.as_str(),generation],
    )?;
    insert_documents(connection, false, entry)
}

fn replace_live(
    connection: &Connection,
    entry: &ObservedSession,
    generation: i64,
    persisted: bool,
) -> rusqlite::Result<()> {
    delete_session(connection, true, &entry.header.id)?;
    let bindings = header_bindings(&entry.header);
    connection.execute(
        "INSERT INTO temp.live_sessions (id,version,created_at,cwd,parent_session,seed_length,delegation_depth,agent_preset,fingerprint,persisted,generation) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![bindings.0,bindings.1,bindings.2,bindings.3,bindings.4,bindings.5,bindings.6,bindings.7,entry.fingerprint,i64::from(persisted),generation],
    )?;
    insert_documents(connection, true, entry)
}

fn insert_documents(
    connection: &Connection,
    live: bool,
    entry: &ObservedSession,
) -> rusqlite::Result<()> {
    let table = if live {
        "temp.live_docs"
    } else {
        "persisted_docs"
    };
    let mut statement = connection.prepare(&format!(
        "INSERT INTO {table} (text,session_id,seq,type,time,surface,codepoint_length) VALUES (?,?,?,?,?,?,?)"
    ))?;
    for document in &entry.documents {
        let text = sanitize_fts_text(&document.text);
        statement.execute(rusqlite::params![
            text,
            document.record.session_id.as_str(),
            i64::try_from(document.record.seq).unwrap_or(i64::MAX),
            document.record.event_type,
            document.record.time,
            surface_text(document.record.surface),
            i64::try_from(text.chars().count()).unwrap_or(i64::MAX),
        ])?;
    }
    Ok(())
}

type HeaderBindings = (
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

fn header_bindings(header: &SessionHeader) -> HeaderBindings {
    (
        header.id.as_str().to_owned(),
        i64::from(header.version),
        i64::try_from(header.created_at).unwrap_or(i64::MAX),
        header.cwd.clone(),
        header
            .parent_session
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        header
            .seed_length
            .and_then(|value| i64::try_from(value).ok()),
        header
            .delegation_depth
            .and_then(|value| i64::try_from(value).ok()),
        header.agent_preset.clone(),
    )
}

fn selected_documents_sql() -> &'static str {
    r"WITH candidates AS (
      SELECT pd.session_id,ps.version,ps.created_at,ps.cwd,ps.parent_session,ps.seed_length,ps.delegation_depth,ps.agent_preset,0 AS live,1 AS persisted,CAST(pd.seq AS INTEGER) AS seq,pd.type,CAST(pd.time AS INTEGER) AS time,pd.surface,highlight(persisted_docs,0,?,?) AS marked_text,CAST(pd.codepoint_length AS INTEGER) AS document_length
      FROM persisted_docs AS pd JOIN persisted_sessions AS ps ON ps.id=pd.session_id
      WHERE persisted_docs MATCH ? AND ?=1 AND NOT EXISTS (SELECT 1 FROM temp.live_sessions AS ls WHERE ls.id=pd.session_id)
      UNION ALL
      SELECT ld.session_id,ls.version,ls.created_at,ls.cwd,ls.parent_session,ls.seed_length,ls.delegation_depth,ls.agent_preset,1 AS live,CASE WHEN ?=1 THEN ls.persisted ELSE 0 END AS persisted,CAST(ld.seq AS INTEGER),ld.type,CAST(ld.time AS INTEGER),ld.surface,highlight(live_docs,0,?,?),CAST(ld.codepoint_length AS INTEGER)
      FROM temp.live_docs AS ld JOIN temp.live_sessions AS ls ON ls.id=ld.session_id WHERE live_docs MATCH ?
    ), matched AS (
      SELECT *, (length(CAST(marked_text AS BLOB))-length(CAST(replace(marked_text,?, '') AS BLOB)))/? AS match_count FROM candidates
    )"
}

fn selected_params(query: &str, persistence_visible: bool) -> Vec<SqlValue> {
    let expression = quote_fts_data(query);
    let visible = i64::from(persistence_visible);
    vec![
        SqlValue::Text(FTS_HIGHLIGHT_START.to_string()),
        SqlValue::Text(FTS_HIGHLIGHT_END.to_string()),
        SqlValue::Text(expression.clone()),
        SqlValue::Integer(visible),
        SqlValue::Integer(visible),
        SqlValue::Text(FTS_HIGHLIGHT_START.to_string()),
        SqlValue::Text(FTS_HIGHLIGHT_END.to_string()),
        SqlValue::Text(expression),
        SqlValue::Text(FTS_HIGHLIGHT_START.to_string()),
        SqlValue::Integer(i64::try_from(FTS_HIGHLIGHT_START.len_utf8()).expect("marker bytes")),
    ]
}

fn sql_params(params: Vec<SqlParam>) -> Vec<SqlValue> {
    params
        .into_iter()
        .map(|param| match param {
            SqlParam::Text(value) => SqlValue::Text(value),
            SqlParam::Number(value) => SqlValue::Real(value.value()),
        })
        .collect()
}

fn row_to_search(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchRow> {
    Ok(SearchRow {
        header: row_header(row)?,
        live: row.get::<_, i64>(8)? == 1,
        persisted: row.get::<_, i64>(9)? == 1,
        seq: u64::try_from(row.get::<_, i64>(10)?).unwrap_or_default(),
        event_type: row.get(11)?,
        time: row.get(12)?,
        surface: parse_surface(&row.get::<_, String>(13)?),
        marked_text: row.get(14)?,
    })
}

fn row_header(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionHeader> {
    Ok(SessionHeader {
        id: SessionId::new(row.get::<_, String>(0)?),
        version: u32::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
        created_at: u64::try_from(row.get::<_, i64>(2)?).unwrap_or_default(),
        cwd: row.get(3)?,
        parent_session: row.get::<_, Option<String>>(4)?.map(SessionId::new),
        seed_length: row
            .get::<_, Option<i64>>(5)?
            .and_then(|value| u64::try_from(value).ok()),
        origin: None,
        delegation_depth: row
            .get::<_, Option<i64>>(6)?
            .and_then(|value| u64::try_from(value).ok()),
        agent_preset: row.get(7)?,
    })
}

fn read_target(
    connection: &Connection,
    live: bool,
    id: &SessionId,
) -> anyhow::Result<Option<(SessionHeader, i64)>> {
    let table = if live {
        "temp.live_sessions"
    } else {
        "persisted_sessions"
    };
    let sql = format!(
        "SELECT id,version,created_at,cwd,parent_session,seed_length,delegation_depth,agent_preset,generation FROM {table} WHERE id=?"
    );
    Ok(connection
        .query_row(&sql, [id.as_str()], |row| {
            Ok((row_header(row)?, row.get(8)?))
        })
        .optional()?)
}

fn surface_text(surface: SessionEventSurface) -> &'static str {
    match surface {
        SessionEventSurface::Current => "current",
        SessionEventSurface::Shadowed => "shadowed",
        SessionEventSurface::LogOnly => "log-only",
    }
}

fn parse_surface(value: &str) -> SessionEventSurface {
    match value {
        "current" => SessionEventSurface::Current,
        "shadowed" => SessionEventSurface::Shadowed,
        _ => SessionEventSurface::LogOnly,
    }
}

fn encode_cursor(payload: &CursorPayload) -> SessionSearchCursor {
    SessionSearchCursor::new(URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).expect("cursor")))
}

fn decode_cursor(
    cursor: &SessionSearchCursor,
    instance: &str,
    scope: &str,
    fingerprint: &str,
    generation: &str,
) -> anyhow::Result<u64> {
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CursorPayload>(&bytes).ok())
        .ok_or_else(invalid_cursor)?;
    if decoded.version != 1
        || decoded.instance != instance
        || decoded.scope != scope
        || decoded.fingerprint != fingerprint
        || decoded.offset > MAX_SAFE_INTEGER
    {
        return Err(invalid_cursor());
    }
    if decoded.generation != generation {
        return Err(SessionQueryError::new(
            "session-search cursor is stale because its relevant corpus changed",
            SessionQueryErrorCode::SessionQueryStaleCursor,
        )
        .into());
    }
    Ok(decoded.offset)
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return Err(aborted());
    }
    Ok(())
}

fn invalid_config(detail: &str) -> anyhow::Error {
    SessionQueryError::new(
        format!("session-search SQLite config: {detail}"),
        SessionQueryErrorCode::SessionQueryInvalidConfig,
    )
    .into()
}

fn index_closed() -> anyhow::Error {
    index_failed("session-search SQLite index is closed")
}

fn index_failed(message: impl Into<String>) -> anyhow::Error {
    SessionQueryError::new(message, SessionQueryErrorCode::SessionQueryIndexFailed).into()
}

fn reconcile_failed(error: &rusqlite::Error) -> anyhow::Error {
    index_failed(format!("session-search reconciliation failed: {error}"))
}

fn next_generation(current: i64) -> anyhow::Result<i64> {
    current.checked_add(1).ok_or_else(|| {
        index_failed("session-search reconciliation failed: generation counter exhausted")
    })
}

fn observation_failed(error: &anyhow::Error) -> anyhow::Error {
    SessionQueryError::new(
        format!("session-search persistence observation failed: {error}"),
        SessionQueryErrorCode::SessionQueryPersistenceFailed,
    )
    .into()
}

fn is_abort(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SessionQueryError>()
        .is_some_and(|error| error.code == SessionQueryErrorCode::SessionQueryAborted)
}

fn aborted() -> anyhow::Error {
    SessionQueryError::new(
        "session-search aborted",
        SessionQueryErrorCode::SessionQueryAborted,
    )
    .into()
}

fn invalid_cursor() -> anyhow::Error {
    SessionQueryError::new(
        "session-search cursor is invalid",
        SessionQueryErrorCode::SessionQueryInvalidCursor,
    )
    .into()
}
