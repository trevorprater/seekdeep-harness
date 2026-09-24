//! Cold inspection, publication races, resume sharing, fences, and Typert parity.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentFactory, AgentHandle, AgentOptions, CreateAgentOptions, Inbox,
    NoopInboxNotifications, ResumeAgentOptions,
};
use seekdeep_api_remotes::*;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionId, SessionOrigin},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceService, SessionPersistenceSnapshot, SessionRawArtifact,
};
use seekdeep_typert_protocol::{
    TypertBoundaryValue, TypertContextRegistry as _, TypertHostContextProvider, TypertLookupFailure,
};
use seekdeep_typert_registry::TypertRegistry;

type InspectHook = Arc<dyn Fn() + Send + Sync>;

struct TestPersistence {
    listed: SessionHeader,
    inspected: Mutex<SessionHeader>,
    events: Vec<SessionEvent>,
    hook: Mutex<Option<InspectHook>>,
    inspections: AtomicUsize,
}

impl TestPersistence {
    fn new(header: SessionHeader) -> Arc<Self> {
        Arc::new(Self {
            listed: header.clone(),
            inspected: Mutex::new(header),
            events: Vec::new(),
            hook: Mutex::new(None),
            inspections: AtomicUsize::new(0),
        })
    }

    fn on_inspect(&self, hook: InspectHook) {
        *self.hook.lock() = Some(hook);
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

    async fn read_raw(
        &self,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionRawArtifact>> {
        Ok(None)
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        Ok(())
    }

    async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load(&self, _id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.inspect(_id, None).await
    }

    async fn inspect(
        &self,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspections.fetch_add(1, Ordering::AcqRel);
        if let Some(hook) = self.hook.lock().take() {
            hook();
        }
        Ok(SessionInspection {
            meta: self.inspected.lock().clone(),
            events: self.events.clone(),
        })
    }

    async fn read_from(
        &self,
        id: &SessionId,
        _from_seq: u64,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspect(id, signal).await
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(vec![self.listed.clone()])
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        Ok(vec![SessionPersistenceSnapshot {
            header: self.listed.clone(),
            revision: SessionPersistenceRevision::new("test:1"),
        }])
    }
}

type ResumeCallback = Arc<
    dyn Fn(Context, ResumeAgentOptions) -> BoxFuture<'static, anyhow::Result<AgentHandle>>
        + Send
        + Sync,
>;

struct TestFactory {
    resume: ResumeCallback,
}

#[async_trait]
impl AgentFactory for TestFactory {
    async fn create_agent(
        &self,
        _owner_context: &Context,
        _options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        anyhow::bail!("create is unused")
    }

    async fn resume(
        &self,
        owner_context: &Context,
        options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        (self.resume)(owner_context.clone(), options).await
    }
}

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<seekdeep_agent::AgentRegistry>,
    persistence: Arc<TestPersistence>,
    typert: Arc<TypertRegistry>,
}

impl Harness {
    fn new(id: &str, resume: ResumeCallback) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(seekdeep_agent::AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        agents
            .set_factory(Arc::new(TestFactory { resume }))
            .unwrap();
        let mut header = SessionHeader::new(SessionId::new(id));
        header.cwd = Some("/proj".to_owned());
        let persistence = TestPersistence::new(header);
        SessionPersistenceService::new(persistence.clone())
            .provide(&context)
            .unwrap();
        let typert = TypertRegistry::new();
        typert.provide(&context).unwrap();
        typert
            .contexts()
            .register_host(
                &context,
                "agent",
                TypertHostContextProvider {
                    wire: "agentId".to_owned(),
                    wire_type_symbol: "@seekdeep-ai/seekdeep-session#SessionId".to_owned(),
                    resolve: Arc::new(|_| Box::pin(async { Ok(None) })),
                },
            )
            .unwrap();
        Self {
            context,
            sessions,
            agents,
            persistence,
            typert,
        }
    }

    fn create_session(&self, id: &SessionId, origin: Option<SessionOrigin>) -> Arc<Session> {
        self.sessions
            .create(
                &self.context,
                Some(id.clone()),
                CreateSessionOptions {
                    cwd: Some("/proj".to_owned()),
                    origin,
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap()
    }
}

fn agent(context: &Context, session: Arc<Session>) -> Arc<Agent> {
    let notifications: Arc<dyn seekdeep_agent::InboxNotifications> =
        Arc::new(NoopInboxNotifications);
    let inbox = Arc::new(Inbox::new(session.clone(), notifications).unwrap());
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn handle(agent: Arc<Agent>) -> AgentHandle {
    AgentHandle::new(agent, Box::new(|| Box::pin(async { Ok(()) })))
}

async fn wait_for_typert(typert: Arc<TypertRegistry>) -> TypertHostContextProvider {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let provider = typert.contexts().get_host("agent").unwrap();
            let marker = TypertBoundaryValue::json(serde_json::json!("missing"));
            if (provider.resolve)(marker).await.is_err() {
                return provider;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn inspected_missing_cwd_and_publication_races_match_source_fences() {
    let calls = Arc::new(AtomicUsize::new(0));
    let resume: ResumeCallback = {
        let calls = calls.clone();
        Arc::new(move |context, options| {
            calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                let session = Session::create(&options.resume_session_id, None, None)?;
                Ok(handle(agent(&context, session)))
            })
        })
    };
    let missing = Harness::new("missing-after-inspect", resume.clone());
    missing.persistence.inspected.lock().cwd = None;
    let resolver = create_api_remote_agent_resolver(
        &missing.context,
        ApiRemoteAgentOptions {
            agent_options: None,
            setup: None,
        },
    );
    assert!(matches!(
        resolver(SessionId::new("missing-after-inspect")).await,
        ApiRemoteAgentResult::Error(ApiRemoteLookupError::SessionNotFound { .. })
    ));
    assert_eq!(calls.load(Ordering::Acquire), 0);

    let ordinary = Harness::new("ordinary-attach-race", resume.clone());
    let id = SessionId::new("ordinary-attach-race");
    let sessions = ordinary.sessions.clone();
    let context = ordinary.context.clone();
    let hook_id = id.clone();
    ordinary.persistence.on_inspect(Arc::new(move || {
        sessions
            .create(
                &context,
                Some(hook_id.clone()),
                CreateSessionOptions {
                    cwd: Some("/proj".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
    }));
    let resolver = create_api_remote_agent_resolver(
        &ordinary.context,
        ApiRemoteAgentOptions {
            agent_options: None,
            setup: None,
        },
    );
    assert!(matches!(resolver(id).await, ApiRemoteAgentResult::Agent(_)));

    let owned = Harness::new("owned-attach-race", resume);
    let id = SessionId::new("owned-attach-race");
    let sessions = owned.sessions.clone();
    let context = owned.context.clone();
    let hook_id = id.clone();
    owned.persistence.on_inspect(Arc::new(move || {
        sessions
            .create(
                &context,
                Some(hook_id.clone()),
                CreateSessionOptions {
                    cwd: Some("/proj".to_owned()),
                    origin: Some(SessionOrigin::Subagent),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
    }));
    let resolver = create_api_remote_agent_resolver(
        &owned.context,
        ApiRemoteAgentOptions {
            agent_options: None,
            setup: None,
        },
    );
    assert!(matches!(
        resolver(id).await,
        ApiRemoteAgentResult::Error(ApiRemoteLookupError::AgentBusy { .. })
    ));
}

#[tokio::test]
async fn concurrent_cold_resume_is_shared_and_typert_context_uses_same_policy() {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    let agent_context = Context::new();
    let resume: ResumeCallback = {
        let calls = calls.clone();
        let gate = gate.clone();
        let agent_context = agent_context.clone();
        Arc::new(move |_, options| {
            calls.fetch_add(1, Ordering::AcqRel);
            let gate = gate.clone();
            let agent_context = agent_context.clone();
            Box::pin(async move {
                gate.notified().await;
                let session = Session::create(&options.resume_session_id, None, None)?;
                Ok(handle(agent(&agent_context, session)))
            })
        })
    };
    let harness = Harness::new("shared", resume);
    let resolver = create_api_remote_agent_resolver(
        &harness.context,
        ApiRemoteAgentOptions {
            agent_options: None,
            setup: None,
        },
    );
    let first = tokio::spawn(resolver(SessionId::new("shared")));
    let second = tokio::spawn(resolver(SessionId::new("shared")));
    while calls.load(Ordering::Acquire) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    gate.notify_one();
    let (first, second) = futures::future::join(first, second).await;
    let (first, second) = (first.unwrap(), second.unwrap());
    let (ApiRemoteAgentResult::Agent(first), ApiRemoteAgentResult::Agent(second)) = (first, second)
    else {
        panic!("expected shared agents")
    };
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(harness.persistence.inspections.load(Ordering::Acquire), 1);

    harness
        .agents
        .register(&harness.context, &first, None)
        .unwrap();
    let provider = wait_for_typert(harness.typert.clone()).await;
    let context_found = (provider.resolve)(TypertBoundaryValue::json(serde_json::json!("shared")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(context_found.fiber().name(), agent_context.fiber().name());

    let cold_context = Context::new();
    let cold_calls = Arc::new(AtomicUsize::new(0));
    let cold_resume: ResumeCallback = {
        let cold_calls = cold_calls.clone();
        let cold_context = cold_context.clone();
        Arc::new(move |_, options| {
            cold_calls.fetch_add(1, Ordering::AcqRel);
            let cold_context = cold_context.clone();
            Box::pin(async move {
                let session = Session::create(&options.resume_session_id, None, None)?;
                Ok(handle(agent(&cold_context, session)))
            })
        })
    };
    let cold = Harness::new("context-cold-resume", cold_resume);
    let _resolver = create_api_remote_agent_resolver(
        &cold.context,
        ApiRemoteAgentOptions {
            agent_options: None,
            setup: None,
        },
    );
    let provider = wait_for_typert(cold.typert.clone()).await;
    let context_found = (provider.resolve)(TypertBoundaryValue::json(serde_json::json!(
        "context-cold-resume"
    )))
    .await
    .unwrap()
    .unwrap();
    assert_eq!(context_found.fiber().name(), cold_context.fiber().name());
    assert_eq!(cold_calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn failed_resume_reclassifies_live_or_attached_subagent_and_typert_rejects() {
    let holder = Arc::new(Mutex::new(None::<Harness>));
    let holder_for_resume = holder.clone();
    let resume: ResumeCallback = Arc::new(move |_, options| {
        let holder = holder_for_resume.clone();
        Box::pin(async move {
            let guard = holder.lock();
            let harness = guard.as_ref().unwrap();
            let session =
                harness.create_session(&options.resume_session_id, Some(SessionOrigin::Subagent));
            let child = agent(&harness.context, session);
            harness
                .agents
                .register(&harness.context, &child, None)
                .unwrap();
            anyhow::bail!("session id already published")
        })
    });
    let harness = Harness::new("owned-resume-race", resume);
    *holder.lock() = Some(harness);
    let resolver = {
        let guard = holder.lock();
        let harness = guard.as_ref().unwrap();
        create_api_remote_agent_resolver(
            &harness.context,
            ApiRemoteAgentOptions {
                agent_options: None,
                setup: None,
            },
        )
    };
    assert!(matches!(
        resolver(SessionId::new("owned-resume-race")).await,
        ApiRemoteAgentResult::Error(ApiRemoteLookupError::AgentBusy { .. })
    ));
    let typert = holder.lock().as_ref().unwrap().typert.clone();
    let provider = wait_for_typert(typert).await;
    let resolution = (provider.resolve)(TypertBoundaryValue::json(serde_json::json!(
        "owned-resume-race"
    )));
    let error = resolution.await.unwrap_err();
    assert!(error.downcast_ref::<TypertLookupFailure>().is_some());

    let attached_holder = Arc::new(Mutex::new(None::<Harness>));
    let attached_for_resume = attached_holder.clone();
    let resume: ResumeCallback = Arc::new(move |_, options| {
        let holder = attached_for_resume.clone();
        Box::pin(async move {
            holder
                .lock()
                .as_ref()
                .unwrap()
                .create_session(&options.resume_session_id, Some(SessionOrigin::Subagent));
            anyhow::bail!("session id already published")
        })
    });
    let attached = Harness::new("owned-session-resume-race", resume);
    *attached_holder.lock() = Some(attached);
    let resolver = {
        let holder = attached_holder.lock();
        create_api_remote_agent_resolver(
            &holder.as_ref().unwrap().context,
            ApiRemoteAgentOptions {
                agent_options: None,
                setup: None,
            },
        )
    };
    assert!(matches!(
        resolver(SessionId::new("owned-session-resume-race")).await,
        ApiRemoteAgentResult::Error(ApiRemoteLookupError::AgentBusy { .. })
    ));
}
