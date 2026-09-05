//! Semantic durability checkpoints before model and tool side effects.

use std::sync::Arc;

use async_stream::try_stream;
use futures::StreamExt;
use seekdeep_agent::{AgentEvent, PreStepDecision};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, Fiber, Plugin, fiber::EffectHandle};
use seekdeep_core::session_store::{SESSIONS, SessionStore};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{ContentBlock, LlmRuntime};
use seekdeep_tools::{
    TOOL_ABORTED_BEFORE_DISPATCH, TOOLS, ToolErrorInfo, ToolExecutionFailure, ToolExecutionResult,
    ToolFailure, ToolRuntime,
};

/// Loader plugin name.
pub const NAME: &str = "session-checkpoint-policy";
/// Services whose persistence boundaries are joined by the policy.
pub const INJECT: &[&str] = &["llm", "sessionPersistence", "sessions", "tools"];

/// Builds the Loader-compatible checkpoint policy.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            let llm = context
                .get(seekdeep_llm::LLM)
                .ok_or_else(|| anyhow::anyhow!("session-checkpoint-policy requires llm"))?;
            let sessions = context
                .get(SESSIONS)
                .ok_or_else(|| anyhow::anyhow!("session-checkpoint-policy requires sessions"))?;
            let tools = context
                .get(TOOLS)
                .ok_or_else(|| anyhow::anyhow!("session-checkpoint-policy requires tools"))?;
            install(&context, &llm, &sessions, &tools).await?;
            Ok(())
        })
    })
}

/// Installs all three semantic durability boundaries transactionally.
///
/// Model streams with a live session flush before constructing the adapter
/// stream. Root agent tool executions flush before entering the tool body and
/// re-check cancellation after the awaited checkpoint. Every agent pre-step
/// flushes the preceding committed batch before the next request is built.
///
/// # Errors
///
/// Returns when any middleware cannot be registered or partial-installation
/// cleanup fails.
pub async fn install(
    context: &Context,
    llm: &Arc<LlmRuntime>,
    sessions: &Arc<SessionStore>,
    tools: &Arc<ToolRuntime>,
) -> anyhow::Result<EffectHandle> {
    let fiber = Fiber::active_child("session-checkpoint-policy");
    let child = context.with_fiber(fiber.clone());
    let install_result = (|| {
        install_llm(&child, llm, sessions)?;
        install_tools(&child, tools, sessions)?;
        install_pre_step(&child, sessions)?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = install_result {
        return match fiber.dispose().await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
        };
    }

    let cleanup_fiber = fiber.clone();
    let effect = EffectHandle::new("session-checkpoint-policy", move || {
        Box::pin(async move { cleanup_fiber.dispose().await })
    });
    if let Err(error) = context.own(effect.clone()) {
        return match fiber.dispose().await {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
        };
    }
    Ok(effect)
}

fn install_llm(
    context: &Context,
    llm: &Arc<LlmRuntime>,
    sessions: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let sessions = sessions.clone();
    llm.register_stream_middleware(
        context,
        Arc::new(move |options, next| {
            let Some(session) = options.session_id.clone().and_then(|id| sessions.get(&id)) else {
                return next(options);
            };
            let sessions = sessions.clone();
            let downstream = next(options);
            downstream.wrap(move |mut downstream| {
                Box::pin(try_stream! {
                    sessions.flush(&session).await?;
                    while let Some(chunk) = downstream.next().await {
                        yield chunk?;
                    }
                })
            })
        }),
        false,
    )?;
    Ok(())
}

fn install_tools(
    context: &Context,
    tools: &Arc<ToolRuntime>,
    sessions: &Arc<SessionStore>,
) -> anyhow::Result<()> {
    let sessions = sessions.clone();
    tools.on_execute(
        context,
        move |execution, next| {
            let sessions = sessions.clone();
            async move {
                if execution.scope_key().is_none() || execution.parent.is_some() {
                    return next.run().await;
                }
                let Some(session) = execution.session() else {
                    return next.run().await;
                };
                sessions.flush(&session).await?;
                if execution.signal().is_aborted() {
                    return Ok(aborted_before_dispatch_result());
                }
                next.run().await
            }
        },
        EventOptions::default(),
    )?;
    Ok(())
}

fn install_pre_step(context: &Context, sessions: &Arc<SessionStore>) -> anyhow::Result<()> {
    let sessions = sessions.clone();
    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |_, args, next| {
            let event = args.get::<AgentEvent<AgentPreStepEvent>>(0);
            let sessions = sessions.clone();
            Box::pin(async move {
                let event = event
                    .ok_or_else(|| anyhow::anyhow!("agent/pre-step is missing its agent event"))?;
                sessions.flush(event.agent.session()).await?;
                let reply = next.run().await?;
                anyhow::ensure!(
                    reply.downcast::<PreStepDecision>().is_some(),
                    "agent/pre-step returned an invalid decision"
                );
                Ok(reply)
            })
        },
        EventOptions::default(),
    )?;
    Ok(())
}

fn aborted_before_dispatch_result() -> ToolExecutionResult {
    let message = "tool call aborted before dispatch".to_owned();
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: vec![ContentBlock::Text {
            text: format!("Error: {message}"),
        }],
        error: ToolFailure {
            message,
            info: Some(ToolErrorInfo {
                name: "AbortError".to_owned(),
                code: TOOL_ABORTED_BEFORE_DISPATCH.to_owned(),
            }),
        },
        meta: None,
        additional_contexts: Vec::new(),
    })
}

/// Registers the policy's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-session-checkpoint-policy",
        InvariantInstaller::noop(),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context as TaskContext, Poll},
    };

    use futures::stream;
    use parking_lot::Mutex;
    use seekdeep_agent::{Agent, AgentEvents, AgentOptions, Inbox, inbox::NoopInboxNotifications};
    use seekdeep_cordis::EventReply;
    use seekdeep_core::{session::SessionId, session_store::CreateSessionOptions};
    use seekdeep_invariants::InvariantConfig;
    use seekdeep_llm::{
        AbortSignal, AdapterStream, CallId, FinishReason, GenerateOptions, LlmAdapter, StreamChunk,
    };
    use seekdeep_scope::{ScopeKey, create_scope};
    use seekdeep_tools::{
        ToolDefinition, ToolExecutionInput, ToolOutputDefinition, ToolRuntimeConfig,
        assert_supported_json_schema,
    };
    use serde_json::{Map, Value, json};
    use tokio::sync::Notify;

    use super::*;

    struct RecordingAdapter {
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl LlmAdapter for RecordingAdapter {
        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            self.order.lock().push("adapter");
            AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            })]))
        }
    }

    fn options(session_id: Option<&SessionId>) -> GenerateOptions {
        let mut options = GenerateOptions::new(
            seekdeep_llm::ProviderId::new("mock"),
            seekdeep_llm::ModelId::new("mock"),
            Vec::new(),
        );
        options.session_id = session_id.cloned();
        options
    }

    fn tool(name: &str, ran: Arc<AtomicBool>) -> ToolDefinition {
        ToolDefinition::new(
            name,
            "side effect",
            Map::new(),
            ToolOutputDefinition::new(
                Arc::new(assert_supported_json_schema(json!({ "type": "null" })).expect("schema")),
                Arc::new(|_, _| Ok(Vec::new())),
            ),
            Arc::new(move |_, _| {
                let ran = ran.clone();
                Box::pin(async move {
                    ran.store(true, Ordering::Release);
                    Ok(Value::Null)
                })
            }),
        )
    }

    struct Setup {
        context: Context,
        llm: Arc<LlmRuntime>,
        tools: Arc<ToolRuntime>,
        session: Arc<seekdeep_core::session::Session>,
        _policy: EffectHandle,
    }

    async fn setup() -> Setup {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let llm = LlmRuntime::install(&context).expect("llm");
        let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("tools");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("checkpoint-test")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        let policy = install(&context, &llm, &sessions, &tools)
            .await
            .expect("policy");
        Setup {
            context,
            llm,
            tools,
            session,
            _policy: policy,
        }
    }

    fn flush_gate(context: &Context, order: Arc<Mutex<Vec<&'static str>>>, gate: Arc<Notify>) {
        context
            .events()
            .on(
                context,
                "session/flush",
                move |_, _| {
                    let order = order.clone();
                    let gate = gate.clone();
                    Box::pin(async move {
                        order.lock().push("flush:start");
                        gate.notified().await;
                        order.lock().push("flush:end");
                        Ok(EventReply::Undefined)
                    })
                },
                EventOptions::default(),
            )
            .expect("flush listener");
    }

    async fn drain(mut stream: seekdeep_llm::LlmStream) -> anyhow::Result<()> {
        while let Some(chunk) = stream.next().await {
            chunk?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn model_stream_waits_before_downstream_construction() {
        let setup = setup().await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Notify::new());
        flush_gate(&setup.context, order.clone(), gate.clone());
        setup
            .llm
            .register_adapter(
                &["mock".to_owned()],
                Arc::new(RecordingAdapter {
                    order: order.clone(),
                }),
            )
            .expect("adapter");

        let pending = tokio::spawn(drain(setup.llm.stream(options(Some(setup.session.id())))));
        tokio::task::yield_now().await;
        assert_eq!(*order.lock(), ["flush:start"]);
        gate.notify_waiters();
        pending.await.expect("join").expect("stream");
        assert_eq!(*order.lock(), ["flush:start", "flush:end", "adapter"]);
    }

    #[tokio::test]
    async fn model_stream_delegates_without_live_session_and_fails_closed_on_flush_error() {
        let setup = setup().await;
        let order = Arc::new(Mutex::new(Vec::new()));
        setup
            .llm
            .register_adapter(
                &["mock".to_owned()],
                Arc::new(RecordingAdapter {
                    order: order.clone(),
                }),
            )
            .expect("adapter");
        drain(setup.llm.stream(options(None)))
            .await
            .expect("no session");
        drain(setup.llm.stream(options(Some(&SessionId::new("detached")))))
            .await
            .expect("detached session");
        assert_eq!(*order.lock(), ["adapter", "adapter"]);

        setup
            .context
            .events()
            .on(
                &setup.context,
                "session/flush",
                |_, _| Box::pin(async { anyhow::bail!("disk unavailable") }),
                EventOptions::default(),
            )
            .expect("failing flush");
        let error = drain(setup.llm.stream(options(Some(setup.session.id()))))
            .await
            .expect_err("checkpoint failure");
        assert!(error.to_string().contains("disk unavailable"));
        assert_eq!(*order.lock(), ["adapter", "adapter"]);
    }

    #[tokio::test]
    async fn top_level_tool_waits_rechecks_abort_and_maps_checkpoint_failure() {
        let setup = setup().await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Notify::new());
        flush_gate(&setup.context, order.clone(), gate.clone());
        let ran = Arc::new(AtomicBool::new(false));
        setup
            .tools
            .register(&setup.context, tool("write", ran.clone()))
            .expect("tool");
        let signal = AbortSignal::default();
        let execution = setup.tools.execute(
            ToolExecutionInput::new(CallId::new("write-1"), "write", json!({}), signal.clone())
                .with_agent_scope(ScopeKey::new())
                .with_agent_session(setup.session.clone()),
        );
        tokio::pin!(execution);
        assert!(matches!(poll_once(execution.as_mut()), Poll::Pending));
        assert_eq!(*order.lock(), ["flush:start"]);
        signal.abort();
        gate.notify_waiters();
        let result = execution.await;
        assert!(!ran.load(Ordering::Acquire));
        assert_eq!(
            result
                .error()
                .and_then(|error| error.info.as_ref())
                .map(|info| info.code.as_str()),
            Some(TOOL_ABORTED_BEFORE_DISPATCH)
        );
        assert_eq!(*order.lock(), ["flush:start", "flush:end"]);
    }

    #[tokio::test]
    async fn rejected_tool_checkpoint_becomes_error_without_running_body() {
        let setup = setup().await;
        setup
            .context
            .events()
            .on(
                &setup.context,
                "session/flush",
                |_, _| Box::pin(async { anyhow::bail!("disk unavailable") }),
                EventOptions::default(),
            )
            .expect("failing checkpoint");
        let ran = Arc::new(AtomicBool::new(false));
        setup
            .tools
            .register(&setup.context, tool("write", ran.clone()))
            .expect("tool");
        let result = setup
            .tools
            .execute(
                ToolExecutionInput::new(
                    CallId::new("write-failure"),
                    "write",
                    json!({}),
                    AbortSignal::default(),
                )
                .with_agent_scope(ScopeKey::new())
                .with_agent_session(setup.session),
            )
            .await;
        assert!(!ran.load(Ordering::Acquire));
        assert!(result.is_error());
        assert_eq!(
            result.content(),
            [ContentBlock::Text {
                text: "Error: disk unavailable".to_owned(),
            }]
        );
    }

    fn poll_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        let wake = futures::task::noop_waker();
        let mut context = TaskContext::from_waker(&wake);
        future.poll(&mut context)
    }

    #[tokio::test]
    async fn nested_or_agentless_tools_reuse_outer_checkpoint() {
        let setup = setup().await;
        let flushes = Arc::new(Mutex::new(0_u64));
        let observed = flushes.clone();
        setup
            .context
            .events()
            .on_sync(
                &setup.context,
                "session/flush",
                move |_, _| {
                    *observed.lock() += 1;
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("flush listener");
        let ran = Arc::new(AtomicBool::new(false));
        setup
            .tools
            .register(&setup.context, tool("probe", ran.clone()))
            .expect("tool");

        let prepared = setup
            .tools
            .prepare_scheduled(
                ToolExecutionInput::new(
                    CallId::new("outer"),
                    "probe",
                    json!({}),
                    AbortSignal::default(),
                )
                .with_agent_scope(ScopeKey::new())
                .with_agent_session(setup.session.clone()),
            )
            .await;
        let token = match prepared {
            seekdeep_tools::ScheduledToolPreparation::Dispatch { execution } => execution.token,
            _ => panic!("outer tool should prepare for dispatch"),
        };
        setup
            .tools
            .execute(
                ToolExecutionInput::new(
                    CallId::new("nested"),
                    "probe",
                    json!({}),
                    AbortSignal::default(),
                )
                .with_agent_scope(ScopeKey::new())
                .with_agent_session(setup.session.clone())
                .with_parent(token),
            )
            .await;
        setup
            .tools
            .execute(ToolExecutionInput::new(
                CallId::new("agentless"),
                "probe",
                json!({}),
                AbortSignal::default(),
            ))
            .await;
        assert_eq!(*flushes.lock(), 0);
        assert!(ran.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn pre_step_flushes_exact_agent_before_delegating() {
        let setup = setup().await;
        let flushed = Arc::new(Mutex::new(Vec::new()));
        let observed = flushed.clone();
        setup
            .context
            .events()
            .on_sync(
                &setup.context,
                "session/flush",
                move |_, args| {
                    let session = args
                        .get::<seekdeep_core::session::Session>(0)
                        .ok_or_else(|| anyhow::anyhow!("missing session"))?;
                    observed.lock().push(session.id().clone());
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("flush listener");
        let scope_key = ScopeKey::new();
        let scope = create_scope(&setup.context, scope_key, None).expect("scope");
        let inbox = Arc::new(
            Inbox::new(setup.session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"),
        );
        let agent = Arc::new(Agent::new(
            setup.session.id().clone(),
            AgentOptions::default(),
            setup.session.clone(),
            inbox,
            scope.context.clone(),
            scope_key,
        ));
        let signal = AbortSignal::default();
        let decision = AgentEvents::new(scope.context.clone(), agent)
            .waterfall(
                "agent/pre-step",
                AgentPreStepEvent {
                    messages: Vec::new(),
                    turn: 1,
                    step: 1,
                    signal,
                },
                || async {
                    Ok(PreStepDecision::Enter {
                        messages: Vec::new(),
                    })
                },
            )
            .await
            .expect("pre-step");
        assert!(matches!(decision, PreStepDecision::Enter { .. }));
        assert_eq!(*flushed.lock(), [setup.session.id().clone()]);
    }

    #[tokio::test]
    async fn disposal_removes_all_boundaries_and_invariant_is_lifecycle_owned() {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let llm = LlmRuntime::install(&context).expect("llm");
        let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("tools");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("dispose-policy")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        let flushes = Arc::new(Mutex::new(0_u64));
        let observed = flushes.clone();
        context
            .events()
            .on_sync(
                &context,
                "session/flush",
                move |_, _| {
                    *observed.lock() += 1;
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("flush");
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(RecordingAdapter {
                order: Arc::new(Mutex::new(Vec::new())),
            }),
        )
        .expect("adapter");
        let policy = install(&context, &llm, &sessions, &tools)
            .await
            .expect("policy");
        drain(llm.stream(options(Some(session.id()))))
            .await
            .expect("with policy");
        policy.dispose().await.expect("dispose policy");
        drain(llm.stream(options(Some(session.id()))))
            .await
            .expect("without policy");
        assert_eq!(*flushes.lock(), 1);

        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        let registration = register_invariant(&invariants).expect("register invariant");
        registration.await_ready().await.expect("ready");
        assert!(invariants.is_registered("seekdeep-session-checkpoint-policy"));
        registration.dispose().await.expect("dispose invariant");
        assert!(!invariants.is_registered("seekdeep-session-checkpoint-policy"));
    }
}
