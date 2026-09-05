#![allow(dead_code)]

use std::{collections::HashMap, path::Path, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, PluginFiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionHeader, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{ContentBlock, Message, MessageId, MessageSource};
use seekdeep_message_feedback::{MESSAGE_FEEDBACK, MessageFeedbackService};
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceService, SessionPersistenceSnapshot,
};
use seekdeep_storage::{BackendRegistration, Storage, StorageBackend};
use seekdeep_storage_domain::{DomainConfig, DomainFacility};
use seekdeep_storage_json::JsonStorageBackend;
use serde_json::json;

type AsyncHook = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

pub(crate) struct TestPersistence {
    sessions: Arc<SessionStore>,
    durable: Mutex<HashMap<SessionId, SessionInspection>>,
    logical: Mutex<HashMap<SessionId, SessionInspection>>,
    inspect_failure: Mutex<Option<String>>,
    read_from_hook: Mutex<Option<AsyncHook>>,
    snapshots_hook: Mutex<Option<AsyncHook>>,
    pub(crate) inspect_calls: std::sync::atomic::AtomicUsize,
    pub(crate) read_from_calls: std::sync::atomic::AtomicUsize,
}

impl TestPersistence {
    pub(crate) fn new(sessions: Arc<SessionStore>) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            durable: Mutex::new(HashMap::new()),
            logical: Mutex::new(HashMap::new()),
            inspect_failure: Mutex::new(None),
            read_from_hook: Mutex::new(None),
            snapshots_hook: Mutex::new(None),
            inspect_calls: std::sync::atomic::AtomicUsize::new(0),
            read_from_calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub(crate) fn persist(&self, session: &Session) {
        self.durable.lock().insert(
            session.id().clone(),
            SessionInspection {
                meta: session.header().clone(),
                events: session.events(),
            },
        );
    }

    pub(crate) fn set_durable(&self, inspection: SessionInspection) {
        self.durable
            .lock()
            .insert(inspection.meta.id.clone(), inspection);
    }

    pub(crate) fn set_logical(&self, inspection: SessionInspection) {
        self.logical
            .lock()
            .insert(inspection.meta.id.clone(), inspection);
    }

    pub(crate) fn set_inspect_failure(&self, message: Option<&str>) {
        *self.inspect_failure.lock() = message.map(str::to_owned);
    }

    pub(crate) fn on_read_from(&self, hook: Option<AsyncHook>) {
        *self.read_from_hook.lock() = hook;
    }

    pub(crate) fn on_list_snapshots(&self, hook: Option<AsyncHook>) {
        *self.snapshots_hook.lock() = hook;
    }
}

#[async_trait]
impl SessionPersistence for TestPersistence {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        None
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        Ok(())
    }

    async fn append(
        &self,
        _id: &SessionId,
        _events: &[seekdeep_core::session::SessionEvent],
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.read_from(id, 0, None).await
    }

    async fn inspect(
        &self,
        id: &SessionId,
        _signal: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspect_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(message) = self.inspect_failure.lock().clone() {
            anyhow::bail!(message);
        }
        if let Some(inspection) = self.logical.lock().get(id).cloned() {
            return Ok(inspection);
        }
        if let Some(live) = self.sessions.get(id) {
            return Ok(SessionInspection {
                meta: live.header().clone(),
                events: live.events(),
            });
        }
        self.durable
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("test persistence: session {id:?} not found"))
    }

    async fn read_from(
        &self,
        id: &SessionId,
        from_seq: u64,
        _signal: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.read_from_calls
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let hook = self.read_from_hook.lock().clone();
        if let Some(hook) = hook {
            hook().await?;
        }
        let stored = self
            .durable
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("test persistence: session {id:?} not found"))?;
        Ok(SessionInspection {
            meta: stored.meta,
            events: stored
                .events
                .into_iter()
                .filter(|event| event.seq >= from_seq)
                .collect(),
        })
    }

    async fn list(
        &self,
        _signal: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(self
            .durable
            .lock()
            .values()
            .map(|inspection| inspection.meta.clone())
            .collect())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        let hook = self.snapshots_hook.lock().clone();
        if let Some(hook) = hook {
            hook().await?;
        }
        Ok(self
            .durable
            .lock()
            .values()
            .enumerate()
            .map(|(index, inspection)| SessionPersistenceSnapshot {
                header: inspection.meta.clone(),
                revision: SessionPersistenceRevision::new(format!(
                    "test:{index}:{}",
                    inspection.events.len()
                )),
            })
            .collect())
    }
}

pub(crate) struct Harness {
    pub(crate) context: Context,
    pub(crate) sessions: Arc<SessionStore>,
    pub(crate) persistence: Arc<TestPersistence>,
    pub(crate) service: Arc<MessageFeedbackService>,
    pub(crate) feedback_fiber: Arc<PluginFiber>,
    pub(crate) flush_effect: Option<seekdeep_cordis::fiber::EffectHandle>,
    _storage: Arc<Storage>,
    _storage_effect: seekdeep_cordis::fiber::EffectHandle,
    _backend: Arc<JsonStorageBackend>,
    _backend_registration: BackendRegistration,
    _facility: Arc<DomainFacility>,
    _facility_effect: seekdeep_cordis::fiber::EffectHandle,
    _mount: seekdeep_storage::FormMount,
}

pub(crate) async fn setup(root: &Path, max_note_bytes: usize, with_flush: bool) -> Harness {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let persistence = TestPersistence::new(sessions.clone());
    SessionPersistenceService::new(persistence.clone())
        .provide(&context)
        .unwrap();
    let storage = Storage::new();
    let storage_effect = storage.provide(&context).unwrap();
    let backend = JsonStorageBackend::new(root);
    let backend_registration = storage
        .backend
        .register("json", backend.clone() as Arc<dyn StorageBackend>)
        .unwrap();
    let facility = DomainFacility::new(
        context.clone(),
        storage.clone(),
        DomainConfig {
            backend: "json".to_owned(),
            routes: HashMap::new(),
        },
    );
    let (facility_effect, mount) = facility.mount(&context).unwrap();
    let flush_effect = with_flush.then(|| {
        let persistence = persistence.clone();
        context
            .events()
            .on(
                &context,
                "session/flush",
                move |_, args| {
                    let persistence = persistence.clone();
                    Box::pin(async move {
                        let session = args
                            .get::<Session>(0)
                            .ok_or_else(|| anyhow::anyhow!("session/flush lacks session"))?;
                        persistence.persist(&session);
                        Ok(EventReply::Undefined)
                    })
                },
                EventOptions::default(),
            )
            .unwrap()
    });
    let feedback_fiber = context
        .plugin(
            seekdeep_message_feedback::plugin(),
            json!({"maxNoteBytes": max_note_bytes}),
        )
        .unwrap();
    feedback_fiber.await_settled().await.unwrap();
    let service = context.get(MESSAGE_FEEDBACK).unwrap();
    Harness {
        context,
        sessions,
        persistence,
        service,
        feedback_fiber,
        flush_effect,
        _storage: storage,
        _storage_effect: storage_effect,
        _backend: backend,
        _backend_registration: backend_registration,
        _facility: facility,
        _facility_effect: facility_effect,
        _mount: mount,
    }
}

pub(crate) struct MessageFixture {
    pub(crate) session: Arc<Session>,
    pub(crate) user_message_id: MessageId,
    pub(crate) assistant_message_ids: [MessageId; 2],
    pub(crate) empty_assistant_message_id: MessageId,
    pub(crate) replacement_assistant_message_id: MessageId,
}

#[allow(clippy::too_many_lines)] // one ordered transcript keeps source fixture parity visible
pub(crate) fn append_message_fixture(session: Arc<Session>) -> MessageFixture {
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "step/start",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    let user = Message::user(
        vec![ContentBlock::Text {
            text: "Question".to_owned(),
        }],
        MessageSource::user(),
    );
    session
        .append(
            "user/message",
            serde_json::to_value(&user).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let first = Message::assistant(
        vec![ContentBlock::Text {
            text: "First answer".to_owned(),
        }],
        "test",
        "test",
    );
    let first_event = session
        .append(
            "assistant/message",
            json!({"turn": 1, "step": 1, "message": first}),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let second = Message::assistant(
        vec![ContentBlock::Text {
            text: "Second answer".to_owned(),
        }],
        "test",
        "test",
    );
    session
        .append(
            "assistant/message",
            json!({"turn": 1, "step": 1, "message": second}),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    let empty = Message::assistant(Vec::new(), "test", "test");
    session
        .append(
            "assistant/message",
            json!({"turn": 1, "step": 1, "message": empty}),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    session
        .append(
            "step/end",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
    let replacement = Message::assistant(
        vec![ContentBlock::Text {
            text: "Model-only replacement".to_owned(),
        }],
        "test",
        "test",
    );
    session
        .append(
            "assistant/message",
            json!({"turn": 1, "step": 1, "message": replacement}),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(first_event.seq, first_event.seq)),
                source_event_seqs: Some(vec![first_event.seq]),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    MessageFixture {
        session,
        user_message_id: user.id().clone(),
        assistant_message_ids: [first.id().clone(), second.id().clone()],
        empty_assistant_message_id: empty.id().clone(),
        replacement_assistant_message_id: replacement.id().clone(),
    }
}

pub(crate) fn cold_fixture(id: &str, created_at: u64, cwd: Option<&str>) -> MessageFixture {
    let id = SessionId::new(id);
    let mut header = SessionHeader::new(id.clone());
    header.created_at = created_at;
    header.cwd = cwd.map(str::to_owned);
    let session = Session::create(&id, None, Some(header)).unwrap();
    append_message_fixture(session)
}

pub(crate) fn live_fixture(harness: &Harness, id: &str) -> MessageFixture {
    let session = harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .unwrap();
    append_message_fixture(session)
}

pub(crate) fn inspection(fixture: &MessageFixture) -> SessionInspection {
    SessionInspection {
        meta: fixture.session.header().clone(),
        events: fixture.session.events(),
    }
}
