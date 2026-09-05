//! Production JSONL restart mirror of the source schedule restart suite.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, ResumeAgentOptions};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, PluginFiber};
use seekdeep_core::{
    session::{AppendOptions, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AdapterStream, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, ModelId, ProviderId,
    StreamChunk,
};
use seekdeep_schedule::{
    ScheduleId, ScheduleRecord, create_after_schedule_record, fold_schedule_events,
    plugin as schedule_plugin,
};
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionPersistence};
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, JsonlSessionPersistence};
use serde_json::{Value, json};

#[derive(Debug, Default)]
struct RecordingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for RecordingAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Text {
                    text: "Reminder acknowledged.".to_owned(),
                },
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

struct RuntimeMount {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    agent_loop: AgentLoop,
    factory: seekdeep_agent::AgentFactoryRegistration,
    persistence: Arc<PluginFiber>,
    schedule: Arc<PluginFiber>,
    adapter: Arc<RecordingAdapter>,
}

impl RuntimeMount {
    async fn new(root: &std::path::Path) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        let persistence = seekdeep_session_persistence_jsonl::install(
            &context,
            JsonlConfig {
                root: root.to_owned(),
                pack_chunks: true,
                compression: JsonlCompression::None,
                write_batch_max_delay_ms: 10,
                prepared_session_cache_size: 5,
            },
        )
        .unwrap();
        persistence.await_settled().await.unwrap();
        let adapter = Arc::new(RecordingAdapter::default());
        dependencies
            .llm
            .register_adapter(&["mock".to_owned()], adapter.clone())
            .unwrap();
        let agent_loop = AgentLoop::new(
            context.clone(),
            dependencies.sessions.clone(),
            (*dependencies.agents).clone(),
            AgentLoopServices {
                llm: dependencies.llm.clone(),
                system_prompt: dependencies.system_prompt.clone(),
                tools: dependencies.tools.clone(),
                max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            },
        )
        .unwrap();
        let persistence_service = context.get(SESSION_PERSISTENCE).unwrap();
        agent_loop
            .set_persistence(persistence_service.persistence())
            .unwrap();
        let factory = dependencies
            .agents
            .set_factory(Arc::new(agent_loop.clone()))
            .unwrap();
        let schedule = context.plugin(schedule_plugin(), Value::Null).unwrap();
        schedule.await_settled().await.unwrap();
        Self {
            context,
            dependencies,
            agent_loop,
            factory,
            persistence,
            schedule,
            adapter,
        }
    }

    async fn dispose(self) {
        let _ = self.schedule.dispose().await;
        let _ = self.agent_loop.dispose().await;
        let _ = self.factory.dispose().await;
        self.dependencies.agents.dispose_initiators().await;
        let _ = self.persistence.dispose().await;
        let _ = self.context.fiber().dispose().await;
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if predicate() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition timed out");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn resumes_one_overdue_reminder_exactly_once_across_fresh_runtime_mounts() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let session_id = SessionId::new("schedule-jsonl-restart");

    let first_context = Context::new();
    let first_sessions = SessionStore::install(&first_context).unwrap();
    let first_persistence = seekdeep_session_persistence_jsonl::install(
        &first_context,
        JsonlConfig {
            root: root.to_owned(),
            pack_chunks: true,
            compression: JsonlCompression::None,
            write_batch_max_delay_ms: 10,
            prepared_session_cache_size: 5,
        },
    )
    .unwrap();
    first_persistence.await_settled().await.unwrap();
    let pending = first_sessions
        .create(
            &first_context,
            Some(session_id.clone()),
            CreateSessionOptions {
                cwd: Some("/tmp".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let pending_record = ScheduleRecord::After(
        create_after_schedule_record(
            ScheduleId::new("schedule-1"),
            "restart reminder",
            1,
            now - 60_000,
        )
        .unwrap(),
    );
    pending
        .append(
            "schedule/change",
            json!({"version": 1, "operation": "create", "schedule": pending_record}),
            AppendOptions::default(),
        )
        .unwrap();
    assert!(first_sessions.flush(&pending).await.unwrap());
    first_persistence.dispose().await.unwrap();
    first_context.fiber().dispose().await.unwrap();

    let restarted = RuntimeMount::new(root).await;
    let mut resume = ResumeAgentOptions::new(session_id.clone());
    resume.agent_options = AgentOptions {
        provider: Some(ProviderId::new("mock")),
        model: Some(ModelId::new("mock")),
        max_tokens: None,
        subagent_depth: None,
    };
    let handle = restarted.dependencies.agents.resume(resume).await.unwrap();
    wait_until(|| {
        handle.agent.session().events().iter().any(|event| {
            event.event_type == "schedule/change" && event.data["operation"] == "dispatch"
        })
    })
    .await;
    handle.agent.when_idle().unwrap().await.unwrap();
    restarted
        .dependencies
        .sessions
        .flush(handle.agent.session())
        .await
        .unwrap();
    let persistence = restarted.context.get(SESSION_PERSISTENCE).unwrap();
    let stored = persistence
        .persistence()
        .inspect(&session_id, None)
        .await
        .unwrap();
    assert!(
        fold_schedule_events(
            &stored.events,
            usize::try_from(stored.meta.seed_length.unwrap_or(0)).unwrap_or(usize::MAX)
        )
        .unwrap()
        .active
        .is_empty()
    );
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| {
                event.event_type == "schedule/change" && event.data["operation"] == "dispatch"
            })
            .count(),
        1
    );
    assert_eq!(restarted.adapter.requests.lock().len(), 1);
    handle.dispose().await.unwrap();
    restarted.dispose().await;

    let replayed = RuntimeMount::new(root).await;
    let mut resume = ResumeAgentOptions::new(session_id.clone());
    resume.agent_options = AgentOptions {
        provider: Some(ProviderId::new("mock")),
        model: Some(ModelId::new("mock")),
        max_tokens: None,
        subagent_depth: None,
    };
    let replay_handle = replayed.dependencies.agents.resume(resume).await.unwrap();
    replay_handle.agent.when_idle().unwrap().await.unwrap();
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    replayed
        .dependencies
        .sessions
        .flush(replay_handle.agent.session())
        .await
        .unwrap();
    assert!(replayed.adapter.requests.lock().is_empty());
    assert_eq!(
        replay_handle
            .agent
            .session()
            .events()
            .iter()
            .filter(|event| {
                event.event_type == "schedule/change" && event.data["operation"] == "dispatch"
            })
            .count(),
        1
    );
    let persistence = replayed.context.get(SESSION_PERSISTENCE).unwrap();
    let stored = persistence
        .persistence()
        .inspect(&session_id, None)
        .await
        .unwrap();
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| {
                event.event_type == "schedule/change" && event.data["operation"] == "dispatch"
            })
            .count(),
        1
    );
    replay_handle.dispose().await.unwrap();
    replayed.dispose().await;

    let cold_sessions = SessionStore::install(&Context::new()).unwrap();
    let cold = JsonlSessionPersistence::new(
        cold_sessions,
        JsonlConfig {
            root: root.to_owned(),
            pack_chunks: true,
            compression: JsonlCompression::None,
            write_batch_max_delay_ms: 10,
            prepared_session_cache_size: 5,
        },
    )
    .unwrap();
    assert_eq!(
        cold.inspect(&session_id, None)
            .await
            .unwrap()
            .events
            .iter()
            .filter(|event| {
                event.event_type == "schedule/change" && event.data["operation"] == "dispatch"
            })
            .count(),
        1
    );
}
