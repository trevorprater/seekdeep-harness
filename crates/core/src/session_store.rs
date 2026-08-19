//! Live session ownership, publication, durability checkpoints, and forking.

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, CordisError, EventArgs, Plugin, PreparedEmission, ServiceKey, fiber::EffectHandle,
};
use serde_json::Value;
use thiserror::Error;

use crate::session::{
    SESSION_FORMAT_VERSION, Session, SessionError, SessionEvent, SessionHeader, SessionId,
    SessionOrigin, SessionPublisher,
};

/// Typed context service key for the live session store.
pub const SESSIONS: ServiceKey<SessionStore> = ServiceKey::new("sessions");
/// Cordis service-plugin name.
pub const NAME: &str = "sessions";
/// The live store has no required services.
pub const INJECT: &[&str] = &[];

/// Builds the source-compatible `SessionStore` service plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, _config| {
        Box::pin(async move {
            SessionStore::install(&context)?;
            Ok(())
        })
    })
}

/// Optional metadata and seed for a new session.
#[derive(Clone, Debug, Default)]
pub struct CreateSessionOptions {
    /// Initial replay or fork history.
    pub seed: Option<Vec<SessionEvent>>,
    /// Absolute working directory.
    pub cwd: Option<String>,
    /// Fork parent.
    pub parent_session: Option<SessionId>,
    /// Explicit creation time.
    pub created_at: Option<u64>,
    /// Durable inherited prefix length.
    pub seed_length: Option<u64>,
    /// Subagent origin marker.
    pub origin: Option<SessionOrigin>,
    /// Persisted delegation depth.
    pub delegation_depth: Option<u64>,
    /// Composition preset.
    pub agent_preset: Option<String>,
}

/// Session lifecycle or fork rejection.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    /// Session construction failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// An id already belongs to a live entry.
    #[error("session \"{0}\" already exists")]
    AlreadyExists(SessionId),
    /// The supplied session is not live in this exact store.
    #[error("session \"{0}\" is not live in this store")]
    NotLive(SessionId),
    /// Creation announcement failed synchronously.
    #[error("{0}")]
    Announcement(String),
    /// Owner context became inactive.
    #[error(transparent)]
    Cordis(#[from] CordisError),
    /// Fork source or boundary is invalid.
    #[error("{message}")]
    Fork {
        /// Stable fork rejection code.
        code: ForkErrorCode,
        /// Human-readable diagnostic.
        message: String,
    },
}

/// Stable session-fork rejection code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkErrorCode {
    /// Source id is absent.
    SessionNotFound,
    /// Source object is not the store's exact instance.
    SessionNotLive,
    /// Child id is occupied.
    SessionAlreadyExists,
    /// Boundary does not identify a contiguous existing event.
    InvalidBoundary,
    /// Selected prefix ends inside an open turn.
    OpenTurn,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AnnouncementState {
    #[default]
    Unannounced,
    Announcing,
    Announced,
}

#[derive(Default)]
struct EntryState {
    announcement: AnnouncementState,
    appending: bool,
    detach_requested: bool,
}

struct SessionEntry {
    id: SessionId,
    session: Arc<Session>,
    context: Context,
    store: Weak<SessionStoreInner>,
    state: Mutex<EntryState>,
}

struct EntryPublication {
    entry: Arc<SessionEntry>,
    emission: Option<PreparedEmission>,
}

impl Drop for EntryPublication {
    fn drop(&mut self) {
        let detach = {
            let mut state = self.entry.state.lock();
            state.appending = false;
            state.detach_requested && state.announcement != AnnouncementState::Announcing
        };
        if detach {
            self.entry.detach_now();
        }
    }
}

impl crate::session::PreparedSessionPublication for EntryPublication {
    fn publish(mut self: Box<Self>) {
        if let Some(emission) = self.emission.take() {
            let id = self.entry.id.clone();
            emission.emit_contained(|error| {
                tracing::warn!(session = %id, %error, "session/event listener failed");
            });
        }
    }
}

impl SessionPublisher for SessionEntry {
    fn prepare_publish(
        self: Arc<Self>,
        event: &SessionEvent,
    ) -> Result<Box<dyn crate::session::PreparedSessionPublication>, SessionError> {
        {
            let mut state = self.state.lock();
            state.appending = true;
        }
        let args = EventArgs::from_values(vec![self.session.clone(), Arc::new(event.clone())]);
        let mut publication = EntryPublication {
            entry: self,
            emission: None,
        };
        let emission = publication
            .entry
            .context
            .events()
            .prepare_emit(&publication.entry.context, "session/event", &args)
            .map_err(|error| SessionError::InvalidEvent(format!("{error:#}")))?;
        publication.emission = Some(emission);
        Ok(Box::new(publication))
    }
}

#[cfg(test)]
use seekdeep_cordis::EventReply;

impl SessionEntry {
    fn request_detach(self: &Arc<Self>) {
        let defer = {
            let mut state = self.state.lock();
            if state.announcement == AnnouncementState::Announcing || state.appending {
                state.detach_requested = true;
                true
            } else {
                false
            }
        };
        if !defer {
            self.detach_now();
        }
    }

    fn detach_now(&self) {
        let Some(store) = self.store.upgrade() else {
            return;
        };
        let removed = {
            let mut sessions = store.sessions.lock();
            if sessions
                .get(&self.id)
                .is_some_and(|entry| std::ptr::eq(Arc::as_ptr(entry), self))
            {
                sessions.shift_remove(&self.id)
            } else {
                None
            }
        };
        if removed.is_none() {
            return;
        }
        self.session.detach_publisher();
        let announced = {
            let mut state = self.state.lock();
            state.detach_requested = false;
            state.announcement != AnnouncementState::Unannounced
        };
        if announced {
            let args = EventArgs::from_values(vec![self.session.clone()]);
            let emission =
                self.context
                    .events()
                    .prepare_emit(&self.context, "session/disposed", &args);
            match emission {
                Ok(emission) => {
                    let id = self.id.clone();
                    emission.emit_contained(|error| {
                        tracing::warn!(session = %id, %error, "session/disposed listener failed");
                    });
                }
                Err(error) => {
                    tracing::warn!(session = %self.id, %error, "session/disposed dispatch failed");
                }
            }
        }
    }
}

struct SessionStoreInner {
    context: Context,
    sessions: Mutex<IndexMap<SessionId, Arc<SessionEntry>>>,
    counter: Mutex<u64>,
}

/// In-memory store and publication owner for live sessions.
#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<SessionStoreInner>,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionStore")
            .field("len", &self.inner.sessions.lock().len())
            .finish_non_exhaustive()
    }
}

impl SessionStore {
    /// Context that owns this store and receives its lifecycle events.
    #[must_use]
    pub fn context(&self) -> Context {
        self.inner.context.clone()
    }

    /// Installs a store in one context.
    ///
    /// # Errors
    ///
    /// Returns a Cordis service-registration failure.
    pub fn install(context: &Context) -> Result<Arc<Self>, CordisError> {
        let store = Arc::new(Self {
            inner: Arc::new(SessionStoreInner {
                context: context.clone(),
                sessions: Mutex::new(IndexMap::new()),
                counter: Mutex::new(0),
            }),
        });
        context.provide(SESSIONS, store.clone())?;
        Ok(store)
    }

    /// Builds but does not publish a session.
    ///
    /// # Errors
    ///
    /// Returns duplicate-id, header, seed, or surface validation failures.
    pub fn prepare(
        &self,
        id: Option<SessionId>,
        options: CreateSessionOptions,
    ) -> Result<Arc<Session>, SessionStoreError> {
        let id = id.unwrap_or_else(|| self.next_id());
        if self.inner.sessions.lock().contains_key(&id) {
            return Err(SessionStoreError::AlreadyExists(id));
        }
        let mut header = SessionHeader::new(id.clone());
        header.version = SESSION_FORMAT_VERSION;
        if let Some(created_at) = options.created_at {
            header.created_at = created_at;
        }
        header.cwd = options.cwd;
        header.parent_session = options.parent_session;
        header.seed_length = options.seed_length;
        header.origin = options.origin;
        header.delegation_depth = options.delegation_depth;
        header.agent_preset = options.agent_preset;
        Ok(Session::create(&id, options.seed, Some(header))?)
    }

    /// Enters an unpublished session and returns its detach effect.
    ///
    /// # Errors
    ///
    /// Returns when its id is occupied or the object is already attached.
    pub fn enter(&self, session: &Arc<Session>) -> Result<EffectHandle, SessionStoreError> {
        let id = session.id().clone();
        let entry = Arc::new(SessionEntry {
            id: id.clone(),
            session: session.clone(),
            context: self.inner.context.clone(),
            store: Arc::downgrade(&self.inner),
            state: Mutex::new(EntryState::default()),
        });
        {
            let mut sessions = self.inner.sessions.lock();
            if sessions.contains_key(&id) {
                return Err(SessionStoreError::AlreadyExists(id));
            }
            let publisher: Arc<dyn SessionPublisher> = entry.clone();
            session.attach_publisher(Arc::downgrade(&publisher))?;
            sessions.insert(id.clone(), entry.clone());
        }
        let detached = Arc::new(AtomicBool::new(false));
        Ok(EffectHandle::synchronous("sessions.enter()", move || {
            if !detached.swap(true, Ordering::AcqRel) {
                entry.request_detach();
            }
            Ok(())
        }))
    }

    /// Emits the creation edge for an entered session exactly once.
    ///
    /// # Errors
    ///
    /// Returns when the object is not live, was already announced, or a synchronous listener vetoes.
    pub fn announce(&self, session: &Arc<Session>) -> Result<(), SessionStoreError> {
        let entry = self.live_entry(session)?;
        {
            let mut state = entry.state.lock();
            if state.announcement != AnnouncementState::Unannounced {
                return Err(SessionStoreError::Announcement(format!(
                    "session \"{}\" was already announced",
                    entry.id
                )));
            }
            state.announcement = AnnouncementState::Announcing;
        }
        let result = entry
            .context
            .events()
            .emit(
                &entry.context,
                "session/created",
                &EventArgs::from_values(vec![session.clone()]),
            )
            .map_err(|error| SessionStoreError::Announcement(format!("{error:#}")));
        let detach = {
            let mut state = entry.state.lock();
            state.announcement = AnnouncementState::Announced;
            state.detach_requested && !state.appending
        };
        if detach {
            entry.detach_now();
        }
        result
    }

    /// Creates, enters, announces, and owner-binds one session.
    ///
    /// # Errors
    ///
    /// Returns construction, attachment, ownership, or creation-listener failures after rollback.
    pub fn create(
        &self,
        owner: &Context,
        id: Option<SessionId>,
        options: CreateSessionOptions,
    ) -> Result<Arc<Session>, SessionStoreError> {
        let session = self.prepare(id, options)?;
        let detach = self.enter(&session)?;
        if let Err(error) = owner.own(detach.clone()) {
            futures::executor::block_on(detach.dispose()).ok();
            return Err(error.into());
        }
        if let Err(error) = self.announce(&session) {
            futures::executor::block_on(detach.dispose()).ok();
            return Err(error);
        }
        Ok(session)
    }

    /// Dispatches an awaited durability checkpoint.
    ///
    /// # Errors
    ///
    /// Returns when the session is not live or any durability listener fails.
    pub async fn flush(&self, session: &Arc<Session>) -> anyhow::Result<bool> {
        let entry = self.live_entry(session).map_err(anyhow::Error::from)?;
        let count = entry
            .context
            .events()
            .listener_count(&entry.context, "session/flush");
        entry
            .context
            .events()
            .parallel(
                &entry.context,
                "session/flush",
                &EventArgs::from_values(vec![session.clone()]),
            )
            .await?;
        Ok(count > 0)
    }

    /// Looks up one live session.
    #[must_use]
    pub fn get(&self, id: &SessionId) -> Option<Arc<Session>> {
        self.inner
            .sessions
            .lock()
            .get(id)
            .map(|entry| entry.session.clone())
    }

    /// Returns live sessions in creation order.
    #[must_use]
    pub fn list(&self) -> Vec<Arc<Session>> {
        self.inner
            .sessions
            .lock()
            .values()
            .map(|entry| entry.session.clone())
            .collect()
    }

    /// Forks a live source through an inclusive between-turn boundary.
    ///
    /// # Errors
    ///
    /// Returns a stable fork error for absent sources, invalid boundaries, open turns, or occupied child ids.
    pub fn fork(
        &self,
        owner: &Context,
        source: &Arc<Session>,
        boundary: Option<i64>,
        child_id: Option<SessionId>,
    ) -> Result<Arc<Session>, SessionStoreError> {
        if let Some(child_id) = &child_id
            && self.get(child_id).is_some()
        {
            return Err(fork_error(
                ForkErrorCode::SessionAlreadyExists,
                format!("session \"{child_id}\" already exists"),
            ));
        }
        let live = self.get(source.id()).ok_or_else(|| {
            fork_error(
                ForkErrorCode::SessionNotFound,
                format!("session \"{}\" not found", source.id()),
            )
        })?;
        if !Arc::ptr_eq(&live, source) {
            return Err(fork_error(
                ForkErrorCode::SessionNotLive,
                format!("session \"{}\" is not the live store instance", source.id()),
            ));
        }
        let events = source.events();
        let seed = fork_seed(source.id(), &events, boundary)?;
        self.create(
            owner,
            child_id,
            CreateSessionOptions {
                cwd: source.header().cwd.clone(),
                parent_session: Some(source.id().clone()),
                seed_length: Some(u64::try_from(seed.len()).unwrap_or(u64::MAX)),
                seed: Some(seed),
                ..CreateSessionOptions::default()
            },
        )
    }

    fn live_entry(&self, session: &Arc<Session>) -> Result<Arc<SessionEntry>, SessionStoreError> {
        self.inner
            .sessions
            .lock()
            .get(session.id())
            .filter(|entry| Arc::ptr_eq(&entry.session, session))
            .cloned()
            .ok_or_else(|| SessionStoreError::NotLive(session.id().clone()))
    }

    fn next_id(&self) -> SessionId {
        loop {
            let next = {
                let mut counter = self.inner.counter.lock();
                *counter += 1;
                SessionId::new(format!("session-{counter}"))
            };
            if !self.inner.sessions.lock().contains_key(&next) {
                return next;
            }
        }
    }
}

fn fork_seed(
    id: &SessionId,
    events: &[SessionEvent],
    requested_boundary: Option<i64>,
) -> Result<Vec<SessionEvent>, SessionStoreError> {
    let Some(last) = events.last() else {
        if requested_boundary.is_none() {
            return Ok(Vec::new());
        }
        return Err(fork_error(
            ForkErrorCode::InvalidBoundary,
            format!(
                "fork boundary {} does not exist in session \"{id}\" (last seq: none)",
                requested_boundary.unwrap_or_default()
            ),
        ));
    };
    let boundary =
        requested_boundary.unwrap_or_else(|| i64::try_from(last.seq).unwrap_or(i64::MAX));
    if boundary < 0 {
        return Err(fork_error(
            ForkErrorCode::InvalidBoundary,
            format!(
                "fork boundary for session \"{id}\" must be a non-negative safe integer, got {boundary}"
            ),
        ));
    }
    let boundary = usize::try_from(boundary).map_err(|_| {
        fork_error(
            ForkErrorCode::InvalidBoundary,
            format!("fork boundary for session \"{id}\" must be a non-negative safe integer"),
        )
    })?;
    let Some(boundary_event) = events.get(boundary) else {
        return Err(fork_error(
            ForkErrorCode::InvalidBoundary,
            format!(
                "fork boundary {boundary} does not exist in session \"{id}\" (last seq: {})",
                last.seq
            ),
        ));
    };
    if usize::try_from(boundary_event.seq).ok() != Some(boundary) {
        return Err(fork_error(
            ForkErrorCode::InvalidBoundary,
            format!(
                "fork boundary {boundary} does not match a contiguous event seq in session \"{id}\""
            ),
        ));
    }
    if let Some(opening) = events[..=boundary]
        .iter()
        .rev()
        .find(|event| matches!(event.event_type.as_str(), "turn/start" | "turn/end"))
        .filter(|event| event.event_type == "turn/start")
    {
        let turn = opening.data.get("turn").unwrap_or(&Value::Null);
        return Err(fork_error(
            ForkErrorCode::OpenTurn,
            format!("fork boundary {boundary} in session \"{id}\" ends inside open turn {turn}"),
        ));
    }
    Ok(events[..=boundary].to_vec())
}

fn fork_error(code: ForkErrorCode, message: String) -> SessionStoreError {
    SessionStoreError::Fork { code, message }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use seekdeep_cordis::EventOptions;
    use seekdeep_llm::{ContentBlock, Message, MessageSource};
    use serde_json::json;

    use super::*;
    use crate::session::{AppendOptions, SurfaceOp};

    #[tokio::test]
    async fn lifecycle_edges_pair_and_append_is_committed_before_observation() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let created = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(AtomicUsize::new(0));
        let disposed = Arc::new(AtomicUsize::new(0));
        for (name, counter) in [
            ("session/created", created.clone()),
            ("session/event", events.clone()),
            ("session/disposed", disposed.clone()),
        ] {
            context
                .events()
                .on_sync(
                    &context,
                    name,
                    move |_, _| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )
                .expect("listener");
        }
        let session = store
            .create(
                &context,
                Some(SessionId::new("s")),
                CreateSessionOptions::default(),
            )
            .expect("create");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("append");
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert_eq!(events.load(Ordering::SeqCst), 1);
        context.fiber().restart().await.expect("owner restart");
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fork_rejects_an_open_turn() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let session = store
            .create(&context, None, CreateSessionOptions::default())
            .expect("create");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("append");
        let error = store
            .fork(&context, &session, None, None)
            .expect_err("open turn");
        assert!(matches!(
            error,
            SessionStoreError::Fork {
                code: ForkErrorCode::OpenTurn,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn plugin_publishes_and_withdraws_the_live_store() {
        let context = Context::new();
        let mounted = context
            .plugin(plugin(), serde_json::Value::Null)
            .expect("mount");
        mounted.await_settled().await.expect("active");
        assert!(context.get(SESSIONS).is_some());
        mounted.dispose().await.expect("dispose");
        assert!(context.get(SESSIONS).is_none());
    }

    fn user_message(text: &str) -> Value {
        serde_json::to_value(Message::user(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::user(),
        ))
        .expect("serialize user message")
    }

    fn append_closed_turn(session: &Session, turn: i64, text: &str) {
        session
            .append(
                "turn/start",
                json!({"turn": turn}),
                AppendOptions::default(),
            )
            .expect("turn start");
        session
            .append(
                "user/message",
                user_message(text),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("user message");
        session
            .append(
                "turn/end",
                json!({"turn": turn, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("turn end");
    }

    fn append_open_turn(session: &Session, turn: i64) {
        session
            .append(
                "turn/start",
                json!({"turn": turn}),
                AppendOptions::default(),
            )
            .expect("turn start");
        session
            .append(
                "user/message",
                user_message(&format!("open {turn}")),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("user message");
    }

    fn last_seq(session: &Session) -> u64 {
        session.events().last().expect("last event").seq
    }

    fn inherited(session: &Session) -> Vec<SessionEvent> {
        let mut events = session.events();
        assert_eq!(
            events.last().expect("end-seed marker").event_type,
            "session/end-seed"
        );
        events.pop();
        events
    }

    fn fork_code(error: &SessionStoreError) -> Option<ForkErrorCode> {
        match error {
            SessionStoreError::Fork { code, .. } => Some(*code),
            _ => None,
        }
    }

    type BoundaryCase = (String, Box<dyn Fn(&Session) -> u64>);

    #[tokio::test]
    async fn fork_empty_live_session_carries_lineage_metadata() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("empty-parent")),
                CreateSessionOptions {
                    cwd: Some("/workspace".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .expect("source");

        let child = store
            .fork(&context, &source, None, Some(SessionId::new("empty-child")))
            .expect("fork");

        assert!(inherited(&child).is_empty());
        let header = child.header();
        assert_eq!(header.id, SessionId::new("empty-child"));
        assert_eq!(header.cwd.as_deref(), Some("/workspace"));
        assert_eq!(
            header.parent_session.as_ref(),
            Some(&SessionId::new("empty-parent"))
        );
        assert_eq!(header.seed_length, Some(0));
    }

    #[tokio::test]
    async fn fork_defaults_to_latest_closed_boundary_into_detached_seed() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("parent")),
                CreateSessionOptions {
                    cwd: Some("/workspace".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .expect("source");
        append_closed_turn(&source, 1, "hello");

        let child = store
            .fork(&context, &source, None, Some(SessionId::new("child")))
            .expect("fork");

        assert_eq!(inherited(&child), source.events());
        assert_eq!(
            child.header().seed_length,
            Some(u64::try_from(source.events().len()).unwrap())
        );
        assert_eq!(child.header().cwd.as_deref(), Some("/workspace"));
        assert_eq!(
            child.header().parent_session.as_ref(),
            Some(&SessionId::new("parent"))
        );
    }

    #[tokio::test]
    async fn fork_includes_log_only_events_appended_after_a_closed_turn() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("log-only-parent")),
                CreateSessionOptions::default(),
            )
            .expect("source");
        append_closed_turn(&source, 1, "hello");
        source
            .append(
                "test/log-only",
                json!({"value": "after execution"}),
                AppendOptions::default(),
            )
            .expect("log-only");

        let child = store
            .fork(
                &context,
                &source,
                None,
                Some(SessionId::new("log-only-child")),
            )
            .expect("fork");

        assert_eq!(inherited(&child), source.events());
        assert_eq!(
            inherited(&child).last().expect("tail").event_type,
            "test/log-only"
        );
    }

    #[tokio::test]
    async fn fork_from_an_earlier_boundary_when_source_has_an_open_tail() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("parent")),
                CreateSessionOptions::default(),
            )
            .expect("source");
        append_closed_turn(&source, 1, "first");
        let first_boundary = last_seq(&source);
        append_closed_turn(&source, 2, "second");
        append_open_turn(&source, 3);

        let child = store
            .fork(
                &context,
                &source,
                Some(i64::try_from(first_boundary).unwrap()),
                Some(SessionId::new("child-from-first")),
            )
            .expect("fork");

        let expected = source.events()[..=usize::try_from(first_boundary).unwrap()].to_vec();
        assert_eq!(inherited(&child), expected);
        assert_eq!(child.header().seed_length, Some(first_boundary + 1));
    }

    #[tokio::test]
    async fn fork_accepts_every_turn_end_reason_as_a_boundary() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let reasons = [
            json!({"kind": "completed"}),
            json!({"kind": "aborted", "reason": {"kind": "user"}}),
            json!({"kind": "error", "error": {"message": "model failed", "code": "UNKNOWN"}}),
            json!({"kind": "aborted", "reason": {"kind": "disposed"}}),
            json!({"kind": "max-tokens"}),
            json!({"kind": "interrupted"}),
        ];
        for (index, reason) in reasons.iter().enumerate() {
            let source = store
                .create(
                    &context,
                    Some(SessionId::new(format!("parent-{index}"))),
                    CreateSessionOptions::default(),
                )
                .expect("source");
            source
                .append("turn/start", json!({"turn": 1}), AppendOptions::default())
                .expect("turn start");
            source
                .append(
                    "user/message",
                    user_message("hello"),
                    AppendOptions {
                        surface_op: Some(SurfaceOp::append()),
                        ..AppendOptions::default()
                    },
                )
                .expect("user message");
            source
                .append(
                    "turn/end",
                    json!({"turn": 1, "reason": reason}),
                    AppendOptions::default(),
                )
                .expect("turn end");

            let boundary = last_seq(&source);
            let child = store
                .fork(
                    &context,
                    &source,
                    Some(i64::try_from(boundary).unwrap()),
                    None,
                )
                .expect("fork");
            assert_eq!(inherited(&child), source.events());
        }
    }

    #[tokio::test]
    async fn fork_keeps_an_open_bracket_in_the_seed_and_starts_live_after_it() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("bracket-parent")),
                CreateSessionOptions::default(),
            )
            .expect("source");
        append_closed_turn(&source, 1, "work");
        let open = source
            .append(
                "test/bracket-open",
                json!({"id": "op-1"}),
                AppendOptions::default(),
            )
            .expect("bracket open");

        let child = store
            .fork(
                &context,
                &source,
                None,
                Some(SessionId::new("bracket-child")),
            )
            .expect("fork");

        let source_events = source.events();
        assert_eq!(
            source_events.last().expect("live tail").event_type,
            "test/bracket-open"
        );
        assert!(
            !source_events
                .iter()
                .any(|event| event.event_type == "session/end-seed")
        );
        let child_events = child.events();
        let boundary = child_events.last().expect("seed end");
        assert_eq!(boundary.event_type, "session/end-seed");
        assert!(boundary.seq > open.seq);
        assert_eq!(child.first_live_seq(), open.seq + 1);
        assert_eq!(
            inherited(&child).last().expect("bracket").event_type,
            "test/bracket-open"
        );
    }

    #[tokio::test]
    async fn fork_rejects_invalid_boundaries_before_creating_a_child() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");

        let empty = store
            .create(
                &context,
                Some(SessionId::new("empty")),
                CreateSessionOptions::default(),
            )
            .expect("empty");
        let error = store
            .fork(
                &context,
                &empty,
                Some(0),
                Some(SessionId::new("empty-child")),
            )
            .expect_err("boundary 0 on empty");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::InvalidBoundary));
        assert!(store.get(&SessionId::new("empty-child")).is_none());

        let source = store
            .create(
                &context,
                Some(SessionId::new("parent")),
                CreateSessionOptions::default(),
            )
            .expect("source");
        append_closed_turn(&source, 1, "hello");
        let error = store
            .fork(
                &context,
                &source,
                Some(-1),
                Some(SessionId::new("negative")),
            )
            .expect_err("negative boundary");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::InvalidBoundary));

        let past_end = i64::try_from(source.seq()).unwrap();
        let error = store
            .fork(
                &context,
                &source,
                Some(past_end),
                Some(SessionId::new("past-end")),
            )
            .expect_err("past-end boundary");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::InvalidBoundary));
    }

    #[tokio::test]
    async fn fork_rejects_a_detached_session_object() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let detached = Session::create(&SessionId::new("detached"), None, None).expect("detached");

        let error = store
            .fork(&context, &detached, None, None)
            .expect_err("detached");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::SessionNotFound));
    }

    #[tokio::test]
    async fn fork_rejects_a_stale_session_object_whose_id_is_live_elsewhere() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        store
            .create(
                &context,
                Some(SessionId::new("same-id")),
                CreateSessionOptions::default(),
            )
            .expect("live");
        let stale = Session::create(&SessionId::new("same-id"), None, None).expect("stale");

        let error = store.fork(&context, &stale, None, None).expect_err("stale");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::SessionNotLive));
    }

    #[tokio::test]
    async fn fork_rejects_boundaries_inside_an_open_turn() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");

        let cases: Vec<BoundaryCase> = vec![
            (
                "turn/start".to_owned(),
                Box::new(|session| {
                    session
                        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
                        .expect("append");
                    last_seq(session)
                }),
            ),
            (
                "step/start".to_owned(),
                Box::new(|session| {
                    session
                        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
                        .expect("append");
                    session
                        .append(
                            "step/start",
                            json!({"turn": 1, "step": 1}),
                            AppendOptions::default(),
                        )
                        .expect("append");
                    last_seq(session)
                }),
            ),
            (
                "user/message".to_owned(),
                Box::new(|session| {
                    append_open_turn(session, 1);
                    last_seq(session)
                }),
            ),
            (
                "assistant/message".to_owned(),
                Box::new(|session| {
                    session
                        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
                        .expect("append");
                    session
                        .append(
                            "step/start",
                            json!({"turn": 1, "step": 1}),
                            AppendOptions::default(),
                        )
                        .expect("append");
                    session.append(
                    "assistant/message",
                    json!({"turn": 1, "step": 1, "message": {"id": "m", "role": "assistant", "source": {"kind": "model", "provider": "mock", "model": "mock"}, "content": [{"type": "text", "text": "partial"}]}}),
                    AppendOptions { surface_op: Some(SurfaceOp::append()), ..AppendOptions::default() },
                ).expect("append");
                    last_seq(session)
                }),
            ),
            (
                "tool/call".to_owned(),
                Box::new(|session| {
                    session
                        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
                        .expect("append");
                    session
                        .append(
                            "step/start",
                            json!({"turn": 1, "step": 1}),
                            AppendOptions::default(),
                        )
                        .expect("append");
                    session.append(
                    "assistant/message",
                    json!({"turn": 1, "step": 1, "message": {"id": "m", "role": "assistant", "source": {"kind": "model", "provider": "mock", "model": "mock"}, "content": [{"type": "tool-call", "id": "call-open", "name": "bash", "arguments": "{}"}]}}),
                    AppendOptions { surface_op: Some(SurfaceOp::append()), ..AppendOptions::default() },
                ).expect("append");
                    session.append("tool/call", json!({"turn": 1, "step": 1, "callId": "call-open", "name": "bash", "arguments": "{}"}), AppendOptions::default()).expect("append");
                    last_seq(session)
                }),
            ),
        ];

        for (label, build) in cases {
            let source = store
                .create(
                    &context,
                    Some(SessionId::new(format!("open-{label}"))),
                    CreateSessionOptions::default(),
                )
                .expect("source");
            let boundary = build(&source);
            let error = store
                .fork(
                    &context,
                    &source,
                    Some(i64::try_from(boundary).unwrap()),
                    None,
                )
                .expect_err("open turn boundary");
            assert_eq!(fork_code(&error), Some(ForkErrorCode::OpenTurn), "{label}");
        }
    }

    #[tokio::test]
    async fn fork_rejects_a_child_id_that_is_already_live() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("parent")),
                CreateSessionOptions::default(),
            )
            .expect("source");
        append_closed_turn(&source, 1, "hello");
        store
            .create(
                &context,
                Some(SessionId::new("child")),
                CreateSessionOptions::default(),
            )
            .expect("child");

        let error = store
            .fork(&context, &source, None, Some(SessionId::new("child")))
            .expect_err("child id occupied");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::SessionAlreadyExists));
    }

    #[tokio::test]
    async fn fork_rejects_a_duplicate_child_id_before_validating_the_boundary() {
        let context = Context::new();
        let store = SessionStore::install(&context).expect("install");
        let source = store
            .create(
                &context,
                Some(SessionId::new("open-parent")),
                CreateSessionOptions::default(),
            )
            .expect("source");
        source
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("open turn");
        store
            .create(
                &context,
                Some(SessionId::new("child")),
                CreateSessionOptions::default(),
            )
            .expect("child");

        let error = store
            .fork(&context, &source, None, Some(SessionId::new("child")))
            .expect_err("child id occupied before boundary");
        assert_eq!(fork_code(&error), Some(ForkErrorCode::SessionAlreadyExists));
    }
}
