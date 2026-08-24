//! Loader-mounted headless persistence mirror of `packages/goal/goal/tests/goal.e2e.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use futures::stream;
use seekdeep_agent::{AgentEvent, AgentRegistry, ModelSelection};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, AgentPreStepEvent, DEFAULT_MAX_PARALLEL_TOOL_CALLS,
    install_request_invariant,
};
use seekdeep_cordis::{Context, EventOptions, Plugin, ServiceKey};
use seekdeep_core::session_store::SessionStore;
use seekdeep_goal::{
    CreateGoalRequest, GOAL, GoalChangeMeta, GoalOperation, fold::decode_goal_change,
};
use seekdeep_headless::HeadlessRunner;
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime,
    ModelId, ProviderId, StreamChunk,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionPersistence, SessionPersistenceService,
};
use seekdeep_session_persistence_jsonl::{JsonlSessionPersistence, plugin as jsonl_plugin};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_todo::{Config as TodoConfig, apply as install_todo};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig, install as install_tools};
use serde_json::{Value, json};

const E2E_SPINE: ServiceKey<E2eSpine> = ServiceKey::new("goalE2eSpine");
const E2E_RUNTIME: ServiceKey<E2eRuntime> = ServiceKey::new("goalE2eRuntime");

struct E2eSpine {
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    llm: Arc<LlmRuntime>,
    prompt: Arc<SystemPrompt>,
    tools: Arc<ToolRuntime>,
}

struct E2eRuntime {
    spine: Arc<E2eSpine>,
    agent_loop: AgentLoop,
}

#[derive(Debug)]
struct ToolThenAnswerAdapter {
    called: AtomicBool,
}

#[async_trait]
impl LlmAdapter for ToolThenAnswerAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        if !self.called.swap(true, Ordering::AcqRel) {
            return AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("goal-e2e-tool-call"),
                        name: "todo_write".to_owned(),
                        arguments: json!({
                            "todos": [{
                                "content": "prove the persisted goal domain",
                                "status": "completed",
                            }],
                        })
                        .to_string(),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ]));
        }
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "CLI tool round trip complete: persisted goal domain".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

fn spine_plugin() -> Plugin {
    Plugin::new(
        "goal-e2e-spine",
        std::iter::empty::<&str>(),
        |context, _| {
            Box::pin(async move {
                let sessions = SessionStore::install(&context)?;
                let agents = Arc::new(AgentRegistry::new(context.clone()));
                agents.provide(&context)?;
                let llm = LlmRuntime::install(&context)?;
                llm.register_adapter(
                    &["mock".to_owned()],
                    Arc::new(ToolThenAnswerAdapter {
                        called: AtomicBool::new(false),
                    }),
                )?;
                let prompt = install_system_prompt(
                    &context,
                    SystemPromptConfig {
                        persona: "Test the persisted goal domain.".to_owned(),
                        ..SystemPromptConfig::default()
                    },
                )?;
                let tools = install_tools(&context, &prompt, ToolRuntimeConfig::default())?;
                install_request_invariant(&context, &llm, sessions.clone())?;
                context.provide(
                    E2E_SPINE,
                    Arc::new(E2eSpine {
                        sessions,
                        agents,
                        llm,
                        prompt,
                        tools,
                    }),
                )?;
                Ok(())
            })
        },
    )
}

fn todo_plugin() -> Plugin {
    Plugin::new("goal-e2e-todo", ["tools"], |context, _| {
        Box::pin(async move {
            install_todo(
                &context,
                TodoConfig {
                    allow_parallel_in_progress: true,
                },
            )?;
            Ok(())
        })
    })
}

fn loop_plugin() -> Plugin {
    Plugin::new(
        "goal-e2e-loop",
        ["goalE2eSpine", "sessionPersistence"],
        |context, _| {
            Box::pin(async move {
                let spine = context
                    .get(E2E_SPINE)
                    .ok_or_else(|| anyhow::anyhow!("goal e2e loop lost its spine"))?;
                let persistence: Arc<SessionPersistenceService> = context
                    .get(SESSION_PERSISTENCE)
                    .ok_or_else(|| anyhow::anyhow!("goal e2e loop lost persistence"))?;
                let agent_loop = AgentLoop::new(
                    context.clone(),
                    spine.sessions.clone(),
                    (*spine.agents).clone(),
                    AgentLoopServices {
                        llm: spine.llm.clone(),
                        system_prompt: spine.prompt.clone(),
                        tools: spine.tools.clone(),
                        max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
                    },
                )?;
                agent_loop.set_persistence(persistence.persistence())?;
                spine
                    .agents
                    .register_factory(&context, Arc::new(agent_loop.clone()))?;
                context.provide(
                    E2E_RUNTIME,
                    Arc::new(E2eRuntime {
                        spine: spine.clone(),
                        agent_loop,
                    }),
                )?;
                Ok(())
            })
        },
    )
}

fn seed_goal_plugin() -> Plugin {
    Plugin::new("seed-goal", ["goals"], |context, _| {
        Box::pin(async move {
            let goals = context
                .get(GOAL)
                .ok_or_else(|| anyhow::anyhow!("seed-goal lost goals"))?;
            context.events().on_waterfall(
                &context,
                "agent/pre-step",
                move |_, args, next| {
                    let goals = goals.clone();
                    Box::pin(async move {
                        let event = args
                            .get::<AgentEvent<AgentPreStepEvent>>(0)
                            .ok_or_else(|| anyhow::anyhow!("agent/pre-step lacks its event"))?;
                        if goals.get(&event.agent)?.is_none() {
                            goals.create(
                                &event.agent,
                                &CreateGoalRequest {
                                    objective:
                                        "Prove the composed goal survives in the session log"
                                            .to_owned(),
                                    max_goal_rounds: Some(7),
                                },
                            )?;
                        }
                        next.run().await
                    })
                },
                EventOptions::default(),
            )?;
            Ok(())
        })
    })
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn real_yaml_headless_tool_round_trip_persists_one_round_zero_goal_snapshot() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let persistence_root = temporary.path().join("sessions");
    std::fs::create_dir(&workspace).expect("workspace");
    let config_path = temporary.path().join("cordis.yml");
    let root_json = serde_json::to_string(&persistence_root.to_string_lossy()).unwrap();
    std::fs::write(
        &config_path,
        format!(
            concat!(
                "- id: spine\n",
                "  name: goal-e2e-spine\n",
                "- id: persistence\n",
                "  name: '@seekdeep-ai/seekdeep-session-persistence-jsonl'\n",
                "  config:\n",
                "    root: {}\n",
                "    compression: none\n",
                "- id: goal\n",
                "  name: '@seekdeep-ai/seekdeep-goal'\n",
                "  config:\n",
                "    defaultMaxGoalRounds: 11\n",
                "- id: seed-goal\n",
                "  name: seed-goal\n",
                "- id: todo\n",
                "  name: goal-e2e-todo\n",
                "- id: loop\n",
                "  name: goal-e2e-loop\n",
            ),
            root_json,
        ),
    )
    .expect("write cordis.yml");

    let catalog = PluginCatalog::new();
    catalog
        .register_named("goal-e2e-spine", spine_plugin())
        .unwrap();
    catalog
        .register_named(
            "@seekdeep-ai/seekdeep-session-persistence-jsonl",
            jsonl_plugin(),
        )
        .unwrap();
    catalog
        .register_named("@seekdeep-ai/seekdeep-goal", seekdeep_goal::plugin())
        .unwrap();
    catalog
        .register_named("seed-goal", seed_goal_plugin())
        .unwrap();
    catalog
        .register_named("goal-e2e-todo", todo_plugin())
        .unwrap();
    catalog
        .register_named("goal-e2e-loop", loop_plugin())
        .unwrap();

    let context = Context::new();
    let composition = catalog
        .load_file(&context, &config_path)
        .await
        .expect("load real cordis.yml");
    assert_eq!(composition.fibers().len(), 6);
    let runtime = context.get(E2E_RUNTIME).expect("assembled runtime");
    let persistence = context
        .get(SESSION_PERSISTENCE)
        .expect("assembled persistence");
    let runner = HeadlessRunner::new(
        runtime.spine.agents.clone(),
        runtime.spine.sessions.clone(),
        runtime.spine.prompt.clone(),
        ModelSelection {
            provider: ProviderId::new("mock"),
            model: ModelId::new("model"),
            reasoning_effort: None,
        },
        workspace.to_string_lossy(),
    )
    .expect("headless runner");
    let result = runner.run("prove the persisted goal domain").await;
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert!(result.stderr.is_empty());
    assert!(result.stdout.contains("CLI tool round trip complete"));
    let session_id = result.session_id.expect("session id");
    let session = runtime
        .spine
        .sessions
        .get(&session_id)
        .expect("live session");
    let events = session.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/end")
            .count(),
        1
    );
    let changes = events
        .iter()
        .filter(|event| event.event_type == "goal/change")
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 1);
    let change = decode_goal_change(&changes[0].data)
        .expect("decode goal change")
        .expect("goal change kind");
    let GoalChangeMeta::Snapshot(change) = change else {
        panic!("create must persist a snapshot")
    };
    assert_eq!(change.operation, GoalOperation::Create);
    assert_eq!(
        change.goal.objective,
        "Prove the composed goal survives in the session log"
    );
    assert_eq!(change.goal.max_goal_rounds, 7);
    assert_eq!(change.rounds_started, 0);
    assert!(!changes[0].data.to_string().contains("activation"));
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.event_type == "user/message"
                    && event.data.pointer("/source/kind").and_then(Value::as_str) == Some("goal")
                    && event
                        .data
                        .pointer("/source/round")
                        .and_then(Value::as_u64)
                        .is_some_and(|round| round > 0)
            })
            .count(),
        0
    );

    let location = persistence
        .persistence()
        .locate(session.header())
        .expect("JSONL location");
    assert!(
        location.path.exists(),
        "headless flush did not materialize the log"
    );
    let durable = persistence
        .persistence()
        .inspect(&session_id, None)
        .await
        .expect("durable inspection");
    assert_eq!(durable.events, events);

    runtime.agent_loop.dispose().await.unwrap();
    runtime.spine.agents.dispose_initiators().await;
    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();

    let reopened_context = Context::new();
    let reopened_sessions = SessionStore::install(&reopened_context).unwrap();
    let reopened = JsonlSessionPersistence::new(
        reopened_sessions,
        seekdeep_session_persistence_jsonl::JsonlConfig {
            root: persistence_root,
            pack_chunks: true,
            compression: seekdeep_session_persistence_jsonl::JsonlCompression::None,
            write_batch_max_delay_ms: 10,
            prepared_session_cache_size: 5,
        },
    )
    .unwrap();
    assert_eq!(
        reopened.inspect(&session_id, None).await.unwrap().events,
        events
    );
    reopened_context.fiber().dispose().await.unwrap();
}
