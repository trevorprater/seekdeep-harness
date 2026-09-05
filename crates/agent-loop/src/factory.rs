//! Rollback-covered publication and ordered teardown of loop agents and sessions.

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentDetach, AgentEvents, AgentFactory, AgentHandle, AgentOptions, AgentRegistry,
    CreateAgentMeta, CreateAgentOptions, ResumeAgentOptions, SessionStartSource,
};
use seekdeep_cordis::{Context, Fiber, fiber::EffectHandle};
use seekdeep_core::{
    preparation::SessionPreparation,
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::SessionPersistence;
use uuid::Uuid;

use crate::{AgentLoopServices, LoopAgent};

/// `agent/session-start` payload fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStartEvent {
    /// Why this session lifecycle began.
    pub source: SessionStartSource,
}

struct FactoryInner {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: AgentRegistry,
    services: AgentLoopServices,
    prompt_variables: EffectHandle,
    persistence: Mutex<Option<Arc<dyn SessionPersistence>>>,
    active: AtomicBool,
    signal: AbortSignal,
    live: Mutex<IndexMap<Uuid, Arc<AgentLifecycle>>>,
}

impl std::fmt::Debug for FactoryInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FactoryInner")
            .field("active", &self.active.load(Ordering::Acquire))
            .field("live", &self.live.lock().len())
            .finish_non_exhaustive()
    }
}

struct AgentLifecycle {
    id: Uuid,
    factory: Weak<FactoryInner>,
    loop_agent: LoopAgent,
    signal: AbortSignal,
    session_detach: Mutex<Option<EffectHandle>>,
    agent_detach: Mutex<Option<AgentDetach>>,
    disposal: tokio::sync::OnceCell<Result<(), String>>,
}

impl std::fmt::Debug for AgentLifecycle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentLifecycle")
            .field("id", &self.id)
            .field("agent", &self.loop_agent.agent.id())
            .finish_non_exhaustive()
    }
}

impl AgentLifecycle {
    fn assert_live(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.signal.is_aborted(),
            "agent {:?} creation aborted",
            self.loop_agent.agent.id()
        );
        Ok(())
    }

    async fn dispose(&self) -> anyhow::Result<()> {
        let result = self
            .disposal
            .get_or_init(|| async {
                self.signal
                    .abort_with_reason(json_reason("lifecycle disposed"));
                let mut errors = Vec::new();
                self.loop_agent.controller.dispose().await;
                if let Err(error) = self.loop_agent.scope.dispose().await {
                    errors.push(format!("{error:#}"));
                }
                if let Some(detach) = self.agent_detach.lock().take() {
                    detach.detach();
                }
                let session_detach = self.session_detach.lock().take();
                if let Some(detach) = session_detach
                    && let Err(error) = detach.dispose().await
                {
                    errors.push(format!("{error:#}"));
                }
                if let Some(factory) = self.factory.upgrade() {
                    factory.live.lock().shift_remove(&self.id);
                }
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("\n"))
                }
            })
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(anyhow::anyhow!(error.clone())),
        }
    }
}

/// Concrete create factory over paired session and agent registries.
#[derive(Clone, Debug)]
pub struct AgentLoop {
    inner: Arc<FactoryInner>,
}

impl AgentLoop {
    /// Creates an active factory.
    ///
    /// # Errors
    ///
    /// Rejects an invalid scheduler cap.
    pub fn new(
        context: Context,
        sessions: Arc<SessionStore>,
        agents: AgentRegistry,
        services: AgentLoopServices,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            services.max_parallel_tool_calls > 0,
            "maxParallelToolCalls must be a positive integer"
        );
        let prompt_variables =
            install_prompt_variables(&context, &services.system_prompt, &agents)?;
        Ok(Self {
            inner: Arc::new(FactoryInner {
                context,
                sessions,
                agents,
                services,
                prompt_variables,
                persistence: Mutex::new(None),
                active: AtomicBool::new(true),
                signal: AbortSignal::default(),
                live: Mutex::new(IndexMap::new()),
            }),
        })
    }

    /// Configured maximum parallel-safe tool calls per step.
    #[must_use]
    pub fn max_parallel_tool_calls(&self) -> usize {
        self.inner.services.max_parallel_tool_calls
    }

    /// Creates, composes, and publishes one exact agent/session lifecycle.
    ///
    /// # Errors
    ///
    /// Returns preparation, setup, cancellation, collision, announcement, or
    /// owner-lifecycle failures after complete rollback.
    pub async fn create_agent(
        &self,
        owner_context: &Context,
        options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        validate_agent_options(&options.agent_options)?;
        self.assert_active()?;
        let session = self.inner.sessions.prepare(
            Some(options.session_id.clone()),
            CreateSessionOptions {
                cwd: options.meta.cwd.clone(),
                parent_session: options.meta.parent_session.clone(),
                seed_length: options.meta.seed_length,
                origin: options.meta.origin,
                delegation_depth: options.meta.delegation_depth,
                agent_preset: options.meta.agent_preset.clone(),
                seed: options.seed.clone(),
                ..CreateSessionOptions::default()
            },
        )?;
        let preparation = SessionPreparation::without_release(session);
        self.setup_and_publish(
            owner_context,
            preparation,
            options,
            SessionStartSource::Startup,
        )
        .await
    }

    /// Installs the persistence backend used by subsequent resume calls.
    ///
    /// # Errors
    ///
    /// Rejects duplicate backend ownership.
    pub fn set_persistence(&self, persistence: Arc<dyn SessionPersistence>) -> anyhow::Result<()> {
        let mut slot = self.inner.persistence.lock();
        anyhow::ensure!(slot.is_none(), "session persistence is already configured");
        *slot = Some(persistence);
        Ok(())
    }

    /// Loads, composes, and publishes one persisted session lifecycle.
    ///
    /// # Errors
    ///
    /// Returns missing-backend, load, cancellation, setup, collision, or
    /// publication failures after releasing every unpublished resource.
    pub async fn resume_agent(
        &self,
        owner_context: &Context,
        options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        validate_agent_options(&options.agent_options)?;
        self.assert_active()?;
        let persistence = self
            .inner
            .persistence
            .lock()
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot resume: session persistence is not configured (load a seekdeep-session-persistence backend)"
                )
            })?;
        let owner_abort = AbortSignal::default();
        let owner_armed = Arc::new(AtomicBool::new(true));
        let owner_abort_effect = owner_abort.clone();
        let owner_armed_effect = owner_armed.clone();
        let owner_effect = EffectHandle::synchronous("agentLoop.resume-load()", move || {
            if owner_armed_effect.swap(false, Ordering::AcqRel) {
                owner_abort_effect.abort_with_reason(json_reason("owner disposed during setup"));
            }
            Ok(())
        });
        let owner_effect = owner_context.own(owner_effect)?;
        let caller = options.signal.clone().unwrap_or_default();
        let caller_and_factory = AbortSignal::fuse(&self.inner.signal, &caller);
        let load_signal = AbortSignal::fuse(&caller_and_factory, &owner_abort);
        let prepared = tokio::select! {
            biased;
            () = load_signal.cancelled() => {
                Err(anyhow::anyhow!("agent {:?} creation aborted", options.resume_session_id))
            }
            result = persistence.prepare(
                &self.inner.sessions,
                &options.resume_session_id,
                Some(load_signal.clone()),
            ) => result,
        };
        owner_armed.store(false, Ordering::Release);
        owner_effect.dispose().await?;
        let preparation = prepared?;
        self.assert_active()?;
        anyhow::ensure!(
            !matches!(
                owner_context.fiber().state(),
                seekdeep_cordis::FiberState::Unloading
                    | seekdeep_cordis::FiberState::Disposed
                    | seekdeep_cordis::FiberState::Failed
            ),
            "agent {:?} setup aborted: owner disposed during setup",
            options.resume_session_id
        );
        let create_options = CreateAgentOptions {
            session_id: options.resume_session_id,
            meta: CreateAgentMeta::default(),
            seed: None,
            agent_options: options.agent_options,
            signal: options.signal,
            setup: options.setup,
            owner_agent: options.owner_agent,
        };
        self.setup_and_publish(
            owner_context,
            preparation,
            create_options,
            SessionStartSource::Resume,
        )
        .await
    }

    async fn setup_and_publish(
        &self,
        owner_context: &Context,
        mut preparation: SessionPreparation,
        options: CreateAgentOptions,
        source: SessionStartSource,
    ) -> anyhow::Result<AgentHandle> {
        let parent_scope = options.owner_agent.as_ref().map(|agent| agent.scope_key());
        let loop_agent = LoopAgent::new_default_with_registry(
            &self.inner.context,
            preparation.session(),
            options.agent_options,
            parent_scope,
            self.inner.services.clone(),
            Some(self.inner.agents.clone()),
        )?
        .0;
        let lifecycle_signal = AbortSignal::fuse(
            &self.inner.signal,
            options.signal.as_ref().unwrap_or(&AbortSignal::default()),
        );
        let lifecycle = Arc::new(AgentLifecycle {
            id: Uuid::now_v7(),
            factory: Arc::downgrade(&self.inner),
            loop_agent,
            signal: lifecycle_signal,
            session_detach: Mutex::new(None),
            agent_detach: Mutex::new(None),
            disposal: tokio::sync::OnceCell::new(),
        });
        self.inner
            .live
            .lock()
            .insert(lifecycle.id, lifecycle.clone());

        let owner_lifecycle = lifecycle.clone();
        let owner_effect = EffectHandle::new("agentLoop.lifecycle()", move || {
            owner_lifecycle
                .signal
                .abort_with_reason(json_reason("owner disposed during setup"));
            Box::pin(async move { owner_lifecycle.dispose().await })
        });
        if let Err(error) = owner_context.own(owner_effect) {
            lifecycle.dispose().await.ok();
            return Err(error.into());
        }

        let result = async {
            lifecycle.assert_live()?;
            let commit = if let Some(setup) = options.setup {
                let setup_future = setup(lifecycle.loop_agent.agent.context().clone());
                tokio::select! {
                    biased;
                    () = lifecycle.signal.cancelled() => {
                        anyhow::bail!("agent {:?} creation aborted", lifecycle.loop_agent.agent.id())
                    }
                    result = setup_future => result?,
                }
            } else {
                None
            };
            lifecycle.assert_live()?;
            if let Some(commit) = commit {
                commit.commit()?;
            }
            lifecycle.assert_live()?;
            self.publish(&lifecycle, options.owner_agent, source)?;
            Ok(AgentHandle::new(lifecycle.loop_agent.agent.clone(), {
                let lifecycle = lifecycle.clone();
                Box::new(move || {
                    let lifecycle = lifecycle.clone();
                    Box::pin(async move { lifecycle.dispose().await })
                })
            }))
        }
        .await;
        preparation.release();
        match result {
            Ok(handle) => Ok(handle),
            Err(error) => {
                lifecycle.dispose().await.ok();
                Err(error)
            }
        }
    }

    fn publish(
        &self,
        lifecycle: &Arc<AgentLifecycle>,
        owner_agent: Option<Arc<Agent>>,
        source: SessionStartSource,
    ) -> anyhow::Result<()> {
        lifecycle.assert_live()?;
        let session_detach = self.inner.sessions.enter_scoped(
            lifecycle.loop_agent.agent.session(),
            lifecycle.loop_agent.agent.scope_key(),
        )?;
        *lifecycle.session_detach.lock() = Some(session_detach);
        let agent_detach = self
            .inner
            .agents
            .enter(lifecycle.loop_agent.agent.clone(), owner_agent)?;
        *lifecycle.agent_detach.lock() = Some(agent_detach);
        self.inner
            .sessions
            .announce(lifecycle.loop_agent.agent.session())?;
        lifecycle.assert_live()?;
        self.inner.agents.announce(&lifecycle.loop_agent.agent)?;
        lifecycle.assert_live()?;
        AgentEvents::new(
            self.inner.context.clone(),
            lifecycle.loop_agent.agent.clone(),
        )
        .emit("agent/session-start", SessionStartEvent { source });
        lifecycle.assert_live()
    }

    /// Stops accepting work and joins every prepared or published lifecycle.
    ///
    /// # Errors
    ///
    /// Returns aggregate lifecycle teardown failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        if self.inner.active.swap(false, Ordering::AcqRel) {
            self.inner
                .signal
                .abort_with_reason(json_reason("agent loop is not active"));
        }
        let live = self.inner.live.lock().values().cloned().collect::<Vec<_>>();
        let mut errors = Vec::new();
        for lifecycle in live {
            if let Err(error) = lifecycle.dispose().await {
                errors.push(format!("{error:#}"));
            }
        }
        if let Err(error) = self.inner.prompt_variables.dispose().await {
            errors.push(format!("prompt-variable teardown failed: {error:#}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("\n")))
        }
    }

    fn assert_active(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.inner.active.load(Ordering::Acquire),
            "agent loop is not active"
        );
        Ok(())
    }
}

fn install_prompt_variables(
    context: &Context,
    system_prompt: &Arc<seekdeep_system_prompt::SystemPrompt>,
    agents: &AgentRegistry,
) -> anyhow::Result<EffectHandle> {
    let fiber = Fiber::active_child("agent-loop prompt variables");
    let owner = context.with_fiber(fiber.clone());
    let install_result = (|| {
        let provider_agents = agents.clone();
        system_prompt.variable(
            &owner,
            "provider",
            Arc::new(move |assemble| {
                Ok(assemble
                    .agent_session
                    .as_ref()
                    .and_then(|session| provider_agents.get(session.id()))
                    .and_then(|agent| agent.options().provider.clone())
                    .map(seekdeep_llm::ProviderId::into_string))
            }),
        )?;
        let model_agents = agents.clone();
        system_prompt.variable(
            &owner,
            "model",
            Arc::new(move |assemble| {
                Ok(assemble
                    .agent_session
                    .as_ref()
                    .and_then(|session| model_agents.get(session.id()))
                    .and_then(|agent| agent.options().model.clone())
                    .map(seekdeep_llm::ModelId::into_string))
            }),
        )?;
        let cwd_agents = agents.clone();
        system_prompt.variable(
            &owner,
            "cwd",
            Arc::new(move |assemble| {
                Ok(assemble
                    .agent_session
                    .as_ref()
                    .and_then(|session| cwd_agents.get(session.id()))
                    .and_then(|agent| agent.session().header().cwd.clone()))
            }),
        )?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = install_result {
        return match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
        };
    }

    let cleanup_fiber = fiber.clone();
    let effect = EffectHandle::new("agent-loop prompt variables", move || {
        Box::pin(async move { cleanup_fiber.dispose().await })
    });
    if let Err(error) = context.own(effect.clone()) {
        return match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
        };
    }
    Ok(effect)
}

fn validate_agent_options(options: &AgentOptions) -> anyhow::Result<()> {
    if let Some(max_tokens) = options.max_tokens {
        anyhow::ensure!(
            max_tokens > 0 && max_tokens <= 9_007_199_254_740_991,
            "agent maxTokens must be a positive safe integer"
        );
    }
    Ok(())
}

fn json_reason(message: &str) -> serde_json::Value {
    serde_json::json!({"kind": "disposed", "message": message})
}

#[async_trait::async_trait]
impl AgentFactory for AgentLoop {
    async fn create_agent(
        &self,
        owner_ctx: &Context,
        options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        AgentLoop::create_agent(self, owner_ctx, options).await
    }

    async fn resume(
        &self,
        owner_ctx: &Context,
        options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        AgentLoop::resume_agent(self, owner_ctx, options).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::stream;
    use seekdeep_agent::{AGENT, AgentEvent, AgentLifecycleEvent, AgentSetupCommit};
    use seekdeep_cordis::{EventOptions, EventReply, Fiber};
    use seekdeep_core::session::{SessionEvent, SessionId};
    use seekdeep_llm::{
        AdapterStream, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime, StreamChunk,
    };
    use seekdeep_system_prompt::{PromptSection, SystemPrompt, SystemPromptConfig};
    use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};

    use super::*;

    #[derive(Debug)]
    struct StopAdapter;

    #[async_trait]
    impl LlmAdapter for StopAdapter {
        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            AdapterStream::new(stream::iter(vec![Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            })]))
        }
    }

    #[derive(Debug)]
    struct InitiatorAdapter {
        agents: AgentRegistry,
        barrier: Arc<tokio::sync::Barrier>,
        observations: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    #[derive(Debug)]
    struct MemoryPersistence {
        inspection: seekdeep_session_persistence::SessionInspection,
        load_entered: Option<Arc<tokio::sync::Semaphore>>,
        load_release: Option<Arc<tokio::sync::Notify>>,
    }

    #[async_trait]
    impl SessionPersistence for MemoryPersistence {
        fn locate(
            &self,
            _meta: &seekdeep_core::session::SessionHeader,
        ) -> Option<seekdeep_session_persistence::SessionLocation> {
            None
        }

        fn supports_raw_artifacts(&self) -> bool {
            false
        }

        async fn create(
            &self,
            _meta: &seekdeep_core::session::SessionHeader,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
            Ok(())
        }

        async fn load(
            &self,
            id: &SessionId,
        ) -> anyhow::Result<seekdeep_session_persistence::SessionInspection> {
            anyhow::ensure!(self.inspection.meta.id == *id, "not found");
            if let Some(entered) = &self.load_entered {
                entered.add_permits(1);
            }
            if let Some(release) = &self.load_release {
                release.notified().await;
            }
            Ok(self.inspection.clone())
        }

        async fn inspect(
            &self,
            id: &SessionId,
            _signal: Option<AbortSignal>,
        ) -> anyhow::Result<seekdeep_session_persistence::SessionInspection> {
            self.load(id).await
        }

        async fn read_from(
            &self,
            id: &SessionId,
            from_seq: u64,
            _signal: Option<AbortSignal>,
        ) -> anyhow::Result<seekdeep_session_persistence::SessionInspection> {
            let mut inspection = self.load(id).await?;
            inspection.events.retain(|event| event.seq >= from_seq);
            Ok(inspection)
        }

        async fn list(
            &self,
            _signal: Option<AbortSignal>,
        ) -> anyhow::Result<Vec<seekdeep_core::session::SessionHeader>> {
            Ok(vec![self.inspection.meta.clone()])
        }

        async fn list_snapshots(
            &self,
            _signal: Option<AbortSignal>,
        ) -> anyhow::Result<Vec<seekdeep_session_persistence::SessionPersistenceSnapshot>> {
            Ok(vec![
                seekdeep_session_persistence::SessionPersistenceSnapshot {
                    header: self.inspection.meta.clone(),
                    revision: seekdeep_session_persistence::SessionPersistenceRevision::new(
                        "memory-1",
                    ),
                },
            ])
        }
    }

    #[async_trait]
    impl LlmAdapter for InitiatorAdapter {
        fn stream(&self, options: GenerateOptions) -> AdapterStream {
            let agents = self.agents.clone();
            let barrier = self.barrier.clone();
            let observations = self.observations.clone();
            AdapterStream::new(async_stream::stream! {
                let before = agents.require_initiator().expect("before initiator");
                barrier.wait().await;
                tokio::task::yield_now().await;
                let after = agents.require_initiator().expect("after initiator");
                observations.lock().push((
                    options
                        .session_id
                        .as_ref()
                        .map_or_else(String::new, ToString::to_string),
                    before.id().as_str().to_owned(),
                    after.id().as_str().to_owned(),
                ));
                yield Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                });
            })
        }
    }

    fn factory(
        context: &Context,
    ) -> (
        AgentLoop,
        Arc<SessionStore>,
        AgentRegistry,
        Arc<SystemPrompt>,
    ) {
        let sessions = SessionStore::install(context).expect("sessions");
        let agents = AgentRegistry::new(context.clone());
        let llm = LlmRuntime::install(context).expect("llm");
        llm.register_adapter(&["mock".to_owned()], Arc::new(StopAdapter))
            .expect("adapter");
        let prompt = SystemPrompt::new(context, SystemPromptConfig::default()).expect("prompt");
        let tools =
            ToolRuntime::new_with_system_prompt(context, &prompt, ToolRuntimeConfig::default())
                .expect("tools");
        let factory = AgentLoop::new(
            context.clone(),
            sessions.clone(),
            agents.clone(),
            AgentLoopServices {
                llm,
                system_prompt: prompt.clone(),
                tools,
                max_parallel_tool_calls: 10,
            },
        )
        .expect("factory");
        (factory, sessions, agents, prompt)
    }

    fn options(id: &str) -> CreateAgentOptions {
        let mut options = CreateAgentOptions::new(SessionId::new(id));
        options.agent_options = AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        };
        options
    }

    #[tokio::test]
    async fn dynamically_created_agents_supply_builtin_prompt_variables() {
        let context = Context::new();
        let (factory, _sessions, _agents, prompt) = factory(&context);
        let mut request = options("dynamic-prompt-variables");
        request.meta.cwd = Some("/work/dynamic-child".to_owned());
        let handle = factory
            .create_agent(&context, request)
            .await
            .expect("create dynamic agent");

        let assembly = prompt
            .assemble(seekdeep_agent::assemble_context_for(&handle.agent, None))
            .await
            .expect("assemble dynamic agent prompt");
        assert_eq!(
            assembly.variables,
            indexmap::IndexMap::from([
                ("provider".to_owned(), Some("mock".to_owned())),
                ("model".to_owned(), Some("model".to_owned())),
                ("cwd".to_owned(), Some("/work/dynamic-child".to_owned())),
            ])
        );

        handle.dispose().await.expect("dispose dynamic agent");
        factory.dispose().await.expect("dispose factory");
        context.fiber().dispose().await.expect("dispose context");
    }

    #[tokio::test]
    async fn factory_drivers_keep_overlapping_initiator_identity_exact() {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let agents = AgentRegistry::new(context.clone());
        let llm = LlmRuntime::install(&context).expect("llm");
        let observations = Arc::new(Mutex::new(Vec::new()));
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(InitiatorAdapter {
                agents: agents.clone(),
                barrier: Arc::new(tokio::sync::Barrier::new(2)),
                observations: observations.clone(),
            }),
        )
        .expect("adapter");
        let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).expect("prompt");
        let tools =
            ToolRuntime::new_with_system_prompt(&context, &prompt, ToolRuntimeConfig::default())
                .expect("tools");
        let factory = AgentLoop::new(
            context.clone(),
            sessions,
            agents.clone(),
            AgentLoopServices {
                llm,
                system_prompt: prompt,
                tools,
                max_parallel_tool_calls: 10,
            },
        )
        .expect("factory");
        let first = factory
            .create_agent(&context, options("initiator-a"))
            .await
            .expect("first");
        let second = factory
            .create_agent(&context, options("initiator-b"))
            .await
            .expect("second");
        first
            .agent
            .followup(seekdeep_llm::UserMessage::new(
                vec![seekdeep_llm::ContentBlock::Text {
                    text: "a".to_owned(),
                }],
                seekdeep_llm::MessageSource::user(),
            ))
            .expect("first prompt");
        second
            .agent
            .followup(seekdeep_llm::UserMessage::new(
                vec![seekdeep_llm::ContentBlock::Text {
                    text: "b".to_owned(),
                }],
                seekdeep_llm::MessageSource::user(),
            ))
            .expect("second prompt");
        let (first_idle, second_idle) = futures::future::join(
            first.agent.when_idle().expect("first idle"),
            second.agent.when_idle().expect("second idle"),
        )
        .await;
        first_idle.unwrap();
        second_idle.unwrap();
        let mut observations = observations.lock().clone();
        observations.sort();
        assert_eq!(
            observations,
            [
                (
                    "initiator-a".to_owned(),
                    "initiator-a".to_owned(),
                    "initiator-a".to_owned()
                ),
                (
                    "initiator-b".to_owned(),
                    "initiator-b".to_owned(),
                    "initiator-b".to_owned()
                )
            ]
        );
        assert!(agents.current_initiator().expect("outside").is_none());
        first.dispose().await.expect("dispose first");
        second.dispose().await.expect("dispose second");
        factory.dispose().await.expect("dispose factory");
        agents.dispose_initiators().await;
    }

    #[tokio::test]
    async fn resume_rehydrates_exact_history_and_anchors_the_next_request() {
        let context = Context::new();
        let (factory, _sessions, _agents, _prompt) = factory(&context);
        let original = factory
            .create_agent(&context, options("resume-exact"))
            .await
            .expect("create");
        original
            .agent
            .followup(seekdeep_llm::UserMessage::new(
                vec![seekdeep_llm::ContentBlock::Text {
                    text: "before restart".to_owned(),
                }],
                seekdeep_llm::MessageSource::user(),
            ))
            .expect("prompt");
        original.agent.when_idle().expect("idle").await.unwrap();
        let persisted = seekdeep_session_persistence::SessionInspection {
            meta: original.agent.session().header().clone(),
            events: original.agent.session().events(),
        };
        let persisted_len = persisted.events.len();
        original.dispose().await.expect("dispose original");
        factory
            .set_persistence(Arc::new(MemoryPersistence {
                inspection: persisted,
                load_entered: None,
                load_release: None,
            }))
            .expect("persistence");
        let starts = Arc::new(Mutex::new(Vec::new()));
        let observed = starts.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/session-start",
                move |_, args| {
                    let event = args
                        .get::<AgentEvent<SessionStartEvent>>(0)
                        .expect("session start");
                    observed.lock().push(event.payload.source);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("listener");
        let mut resume = ResumeAgentOptions::new(SessionId::new("resume-exact"));
        resume.agent_options = AgentOptions {
            provider: Some("mock".into()),
            model: Some("model".into()),
            max_tokens: None,
            subagent_depth: None,
        };
        let resumed = factory
            .resume_agent(&context, resume)
            .await
            .expect("resume");
        assert_eq!(*starts.lock(), [SessionStartSource::Resume]);
        assert_eq!(
            resumed.agent.session().first_live_seq(),
            u64::try_from(persisted_len).expect("seed length")
        );
        assert_eq!(
            resumed.agent.session().events()[persisted_len].event_type,
            "session/end-seed"
        );
        resumed
            .agent
            .followup(seekdeep_llm::UserMessage::new(
                vec![seekdeep_llm::ContentBlock::Text {
                    text: "after restart".to_owned(),
                }],
                seekdeep_llm::MessageSource::user(),
            ))
            .expect("prompt after resume");
        resumed
            .agent
            .when_idle()
            .expect("idle resumed")
            .await
            .unwrap();
        let events = resumed.agent.session().events();
        let live_header = events
            .iter()
            .skip(persisted_len + 1)
            .find(|event| event.event_type == "request/header")
            .expect("resume request anchor");
        assert_eq!(live_header.data["reason"], "resume");
        let last_turn = events
            .iter()
            .rev()
            .find(|event| event.event_type == "turn/start")
            .expect("turn");
        assert_eq!(last_turn.data["turn"], 2);
        resumed.dispose().await.expect("dispose resumed");
        factory.dispose().await.expect("dispose factory");
    }

    #[tokio::test]
    async fn resume_load_is_cancelled_by_caller_and_owner_without_publication() {
        let context = Context::new();
        let (loop_factory, sessions, agents, _prompt) = factory(&context);
        let id = SessionId::new("blocked-resume");
        let persisted_session =
            seekdeep_core::session::Session::create(&id, None, None).expect("persisted session");
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        loop_factory
            .set_persistence(Arc::new(MemoryPersistence {
                inspection: seekdeep_session_persistence::SessionInspection {
                    meta: persisted_session.header().clone(),
                    events: Vec::new(),
                },
                load_entered: Some(entered.clone()),
                load_release: Some(release.clone()),
            }))
            .expect("persistence");
        let signal = AbortSignal::default();
        let mut options = ResumeAgentOptions::new(id.clone());
        options.signal = Some(signal.clone());
        let task_factory = loop_factory.clone();
        let task_context = context.clone();
        let caller_task =
            tokio::spawn(async move { task_factory.resume_agent(&task_context, options).await });
        entered.acquire().await.expect("load entered").forget();
        signal.abort_with_reason(serde_json::json!({"kind": "user"}));
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), caller_task)
            .await
            .expect("caller cancellation timeout")
            .expect("caller task")
            .expect_err("caller cancelled");
        assert!(error.to_string().contains("creation aborted"));
        assert!(sessions.get(&id).is_none());
        assert!(agents.get(&id).is_none());

        release.notify_waiters();
        loop_factory.dispose().await.expect("dispose first factory");

        let second_context = Context::new();
        let (second_factory, second_sessions, second_agents, _prompt) = factory(&second_context);
        let second_id = SessionId::new("owner-blocked-resume");
        let persisted_session = seekdeep_core::session::Session::create(&second_id, None, None)
            .expect("persisted session");
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        second_factory
            .set_persistence(Arc::new(MemoryPersistence {
                inspection: seekdeep_session_persistence::SessionInspection {
                    meta: persisted_session.header().clone(),
                    events: Vec::new(),
                },
                load_entered: Some(entered.clone()),
                load_release: Some(Arc::new(tokio::sync::Notify::new())),
            }))
            .expect("persistence");
        let owner_fiber = Fiber::active_child("resume-owner");
        let owner = second_context.with_fiber(owner_fiber.clone());
        let task_factory = second_factory.clone();
        let owner_for_task = owner.clone();
        let owner_task = tokio::spawn(async move {
            task_factory
                .resume_agent(&owner_for_task, ResumeAgentOptions::new(second_id.clone()))
                .await
                .map(|_| second_id)
        });
        entered
            .acquire()
            .await
            .expect("owner load entered")
            .forget();
        owner_fiber.dispose().await.expect("owner dispose");
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), owner_task)
            .await
            .expect("owner cancellation timeout")
            .expect("owner task");
        let failed_id = result.expect_err("owner cancelled");
        assert!(failed_id.to_string().contains("creation aborted"));
        assert!(second_sessions.list().is_empty());
        assert!(second_agents.list().is_empty());
        second_factory
            .dispose()
            .await
            .expect("dispose second factory");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn setup_is_unpublished_then_announcements_pair_and_dispose_in_order() {
        let context = Context::new();
        let (factory, sessions, agents, prompt) = factory(&context);
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let session_created = lifecycle.clone();
        context
            .events()
            .on_sync(
                &context,
                "session/created",
                move |_, args| {
                    let session = args
                        .get::<seekdeep_core::session::Session>(0)
                        .expect("session");
                    session_created
                        .lock()
                        .push(format!("session-created:{}", session.id()));
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("session created");
        let agent_created = lifecycle.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/created",
                move |_, args| {
                    let event = args.get::<AgentLifecycleEvent>(0).expect("agent");
                    agent_created
                        .lock()
                        .push(format!("agent-created:{}", event.agent.id()));
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("agent created");
        let session_start = lifecycle.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/session-start",
                move |_, args| {
                    let event = args.get::<AgentEvent<SessionStartEvent>>(0).expect("start");
                    session_start
                        .lock()
                        .push(format!("session-start:{}", event.agent.id()));
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("session start");
        for (name, label, agent_event) in [
            ("agent/disposed", "agent-disposed", true),
            ("session/disposed", "session-disposed", false),
        ] {
            let lifecycle = lifecycle.clone();
            context
                .events()
                .on_sync(
                    &context,
                    name,
                    move |_, args| {
                        let id = if agent_event {
                            args.get::<AgentLifecycleEvent>(0)
                                .expect("agent")
                                .agent
                                .id()
                                .to_string()
                        } else {
                            args.get::<seekdeep_core::session::Session>(0)
                                .expect("session")
                                .id()
                                .to_string()
                        };
                        lifecycle.lock().push(format!("{label}:{id}"));
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )
                .expect("dispose listener");
        }

        let mut create = options("published");
        let setup_sessions = sessions.clone();
        let setup_agents = agents.clone();
        let setup_prompt = prompt.clone();
        create.setup = Some(Arc::new(move |agent_context| {
            let setup_sessions = setup_sessions.clone();
            let setup_agents = setup_agents.clone();
            let setup_prompt = setup_prompt.clone();
            Box::pin(async move {
                let agent = agent_context.get(AGENT).expect("scoped agent");
                assert!(setup_sessions.get(agent.id()).is_none());
                assert!(setup_agents.get(agent.id()).is_none());
                setup_prompt.section(
                    &agent_context,
                    PromptSection::new("setup", 10.0, "setup prompt"),
                )?;
                Ok(None)
            })
        }));
        let handle = factory
            .create_agent(&context, create)
            .await
            .expect("create");
        assert!(Arc::ptr_eq(
            &sessions.get(handle.agent.id()).expect("session live"),
            handle.agent.session()
        ));
        assert!(Arc::ptr_eq(
            &agents.get(handle.agent.id()).expect("agent live"),
            &handle.agent
        ));
        assert_eq!(
            *lifecycle.lock(),
            [
                "session-created:published",
                "agent-created:published",
                "session-start:published"
            ]
        );
        handle.dispose().await.expect("dispose");
        assert!(sessions.get(handle.agent.id()).is_none());
        assert!(agents.get(handle.agent.id()).is_none());
        assert_eq!(
            *lifecycle.lock(),
            [
                "session-created:published",
                "agent-created:published",
                "session-start:published",
                "agent-disposed:published",
                "session-disposed:published"
            ]
        );
    }

    #[tokio::test]
    async fn setup_and_commit_failures_publish_nothing() {
        #[derive(Debug)]
        struct RejectCommit;
        impl AgentSetupCommit for RejectCommit {
            fn commit(&self) -> anyhow::Result<()> {
                anyhow::bail!("commit veto")
            }
        }

        let context = Context::new();
        let (factory, sessions, agents, _prompt) = factory(&context);
        let mut setup_failure = options("setup-failure");
        setup_failure.setup = Some(Arc::new(|_| {
            Box::pin(async { anyhow::bail!("setup veto") })
        }));
        let error = factory
            .create_agent(&context, setup_failure)
            .await
            .expect_err("setup fails");
        assert!(error.to_string().contains("setup veto"));
        assert!(sessions.list().is_empty());
        assert!(agents.list().is_empty());

        let mut commit_failure = options("commit-failure");
        commit_failure.setup = Some(Arc::new(|_| {
            Box::pin(async { Ok(Some(Arc::new(RejectCommit) as Arc<dyn AgentSetupCommit>)) })
        }));
        let error = factory
            .create_agent(&context, commit_failure)
            .await
            .expect_err("commit fails");
        assert!(error.to_string().contains("commit veto"));
        assert!(sessions.list().is_empty());
        assert!(agents.list().is_empty());
    }

    #[tokio::test]
    async fn agent_announcement_veto_pairs_every_started_lifecycle() {
        let context = Context::new();
        let (factory, sessions, agents, _prompt) = factory(&context);
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        for (name, label) in [
            ("session/created", "session-created"),
            ("agent/created", "agent-created"),
            ("agent/disposed", "agent-disposed"),
            ("session/disposed", "session-disposed"),
        ] {
            let lifecycle = lifecycle.clone();
            context
                .events()
                .on_sync(
                    &context,
                    name,
                    move |_, _| {
                        lifecycle.lock().push(label);
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )
                .expect("listener");
        }
        context
            .events()
            .on_sync(
                &context,
                "agent/created",
                |_, _| anyhow::bail!("agent veto"),
                EventOptions::default(),
            )
            .expect("veto");
        let error = factory
            .create_agent(&context, options("vetoed"))
            .await
            .expect_err("veto");
        assert!(error.to_string().contains("agent veto"));
        assert!(sessions.list().is_empty());
        assert!(agents.list().is_empty());
        assert_eq!(
            *lifecycle.lock(),
            [
                "session-created",
                "agent-created",
                "agent-disposed",
                "session-disposed"
            ]
        );
    }

    #[tokio::test]
    async fn owner_and_factory_disposal_join_the_same_teardown() {
        let context = Context::new();
        let (factory, sessions, agents, _prompt) = factory(&context);
        let owner_fiber = Fiber::active_child("owner");
        let owner = context.with_fiber(owner_fiber.clone());
        let handle = factory
            .create_agent(&owner, options("owned"))
            .await
            .expect("owned");
        owner_fiber.dispose().await.expect("owner dispose");
        assert!(sessions.get(handle.agent.id()).is_none());
        assert!(agents.get(handle.agent.id()).is_none());
        handle.dispose().await.expect("joined handle");

        let second = factory
            .create_agent(&context, options("factory-owned"))
            .await
            .expect("second");
        factory.dispose().await.expect("factory dispose");
        assert!(sessions.get(second.agent.id()).is_none());
        assert!(agents.get(second.agent.id()).is_none());
        assert!(
            factory
                .create_agent(&context, options("after"))
                .await
                .expect_err("inactive")
                .to_string()
                .contains("not active")
        );
    }

    #[tokio::test]
    async fn caller_abort_ends_never_settling_setup_without_publication() {
        let context = Context::new();
        let (factory, sessions, agents, _prompt) = factory(&context);
        let signal = AbortSignal::default();
        let mut create = options("cancel-setup");
        create.signal = Some(signal.clone());
        create.setup = Some(Arc::new(|_| {
            Box::pin(async {
                futures::future::pending::<()>().await;
                Ok(None)
            })
        }));
        let task_factory = factory.clone();
        let task_context = context.clone();
        let task =
            tokio::spawn(async move { task_factory.create_agent(&task_context, create).await });
        tokio::task::yield_now().await;
        signal.abort_with_reason(serde_json::json!({"kind": "user"}));
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("settles")
            .expect("join")
            .expect_err("aborted");
        assert!(error.to_string().contains("creation aborted"));
        assert!(sessions.list().is_empty());
        assert!(agents.list().is_empty());
    }
}
