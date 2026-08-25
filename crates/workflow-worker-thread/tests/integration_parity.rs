//! Real-stack worker integration parity with only the model boundary scripted.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentHandle, AgentOptions, CreateAgentOptions};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS, install_request_invariant,
};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::{invariant::install_session_invariants, session::SessionId};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, ModelId,
    ProviderId, StreamChunk,
};
use seekdeep_subagent::SubagentRuntime;
use seekdeep_subagent_in_process_driver::STRUCTURED_OUTPUT_TOOL;
use seekdeep_subagent_spawn_in_process::Config as SpawnConfig;
use seekdeep_workflow::{
    WorkflowAgentInfo, WorkflowEngine, WorkflowMeta, WorkflowPhase, WorkflowStartRequest,
    WorkflowStopReason,
};
use seekdeep_workflow_worker_thread::{Config, WorkerThreadWorkflowEngine};
use serde_json::{Value, json};

enum Reply {
    Text(String),
    Structured(Value),
}

struct ScriptedAdapter {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        match self.replies.lock().pop_front().expect("scripted reply") {
            Reply::Text(text) => AdapterStream::new(stream::iter(vec![
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ])),
            Reply::Structured(value) => AdapterStream::new(stream::iter(vec![
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("structured-call"),
                        name: STRUCTURED_OUTPUT_TOOL.to_owned(),
                        arguments: serde_json::to_string(&value).expect("structured arguments"),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ])),
        }
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    adapter: Arc<ScriptedAdapter>,
    engine: Arc<WorkerThreadWorkflowEngine>,
    parent: AgentHandle,
}

impl Harness {
    async fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .expect("agent-loop dependencies");
        install_session_invariants(&context, &dependencies.sessions).expect("session invariant");
        install_request_invariant(&context, &dependencies.llm, dependencies.sessions.clone())
            .expect("request invariant");
        let invariants =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
        seekdeep_agent::register_invariant(&invariants)
            .expect("agent invariant")
            .await_ready()
            .await
            .expect("agent invariant ready");

        let adapter = Arc::new(ScriptedAdapter {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        });
        dependencies
            .llm
            .register_adapter(&["mock".to_owned()], adapter.clone())
            .expect("mock adapter");
        let factory = AgentLoop::new(
            context.clone(),
            dependencies.sessions.clone(),
            dependencies.agents.as_ref().clone(),
            AgentLoopServices {
                llm: dependencies.llm.clone(),
                system_prompt: dependencies.system_prompt.clone(),
                tools: dependencies.tools.clone(),
                max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            },
        )
        .expect("agent loop");
        dependencies
            .agents
            .set_factory(Arc::new(factory))
            .expect("agent factory");
        SubagentRuntime::install(&context).expect("subagents");
        seekdeep_subagent_spawn_in_process::apply(&context, SpawnConfig::default())
            .expect("spawn provider");
        let engine = WorkerThreadWorkflowEngine::new(&context, Config::default()).expect("engine");

        let mut options = CreateAgentOptions::new(SessionId::new("workflow-parent"));
        options.agent_options = AgentOptions {
            provider: Some(ProviderId::new("mock")),
            model: Some(ModelId::new("mock")),
            max_tokens: None,
            subagent_depth: None,
        };
        let parent = dependencies
            .agents
            .create(options)
            .await
            .expect("parent agent");
        Self {
            context,
            dependencies,
            adapter,
            engine,
            parent,
        }
    }

    fn request(&self, name: &str, script: &str) -> WorkflowStartRequest {
        WorkflowStartRequest {
            script: script.to_owned(),
            meta: WorkflowMeta {
                name: name.to_owned(),
                description: "two real children through a worker thread: one prose, one structured"
                    .to_owned(),
                when_to_use: None,
                phases: Some(vec![
                    WorkflowPhase {
                        title: "Ask".to_owned(),
                        detail: None,
                        provider: None,
                        model: None,
                    },
                    WorkflowPhase {
                        title: "Judge".to_owned(),
                        detail: None,
                        provider: None,
                        model: None,
                    },
                ]),
            },
            args: None,
            subagent_provider: None,
            max_total_agents: None,
            parent: self.parent.agent.clone(),
            signal: None,
        }
    }

    async fn dispose(&self) {
        self.parent.dispose().await.expect("dispose parent");
        self.context
            .root_fiber()
            .dispose()
            .await
            .expect("dispose context");
    }
}

#[tokio::test]
async fn plain_then_schema_child_crosses_the_real_spawn_and_structured_runtime() {
    let harness = Harness::new([
        Reply::Text("The answer is 4.".to_owned()),
        Reply::Structured(json!({"containsFour": true, "confidence": 0.99})),
    ])
    .await;
    let child_ids = Arc::new(Mutex::new(Vec::<SessionId>::new()));
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    for name in [
        "workflow/start",
        "workflow/phase",
        "workflow/log",
        "workflow/agent-start",
        "workflow/agent-end",
        "workflow/end",
    ] {
        let seen_ids = child_ids.clone();
        let seen_events = events.clone();
        let agents = harness.dependencies.agents.clone();
        harness
            .context
            .events()
            .on_sync(
                &harness.context,
                name,
                move |_, args| {
                    seen_events.lock().push(name.to_owned());
                    if name == "workflow/agent-start" {
                        let info = args
                            .get::<WorkflowAgentInfo>(1)
                            .ok_or_else(|| anyhow::anyhow!("missing agent info"))?;
                        anyhow::ensure!(
                            agents.get(&info.child_id).is_some(),
                            "workflow announced an unpublished child"
                        );
                        seen_ids.lock().push(info.child_id.clone());
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("observer");
    }
    let run = harness
        .engine
        .start(harness.request(
            "integration",
            r#"phase('Ask')
log('asking the prose child')
const prose = await agent('Reply with exactly one short sentence: what is 2 + 2?')
phase('Judge')
const judged = await agent(
  'Here is an answer to the question "what is 2+2": ' + prose
  + ' — report whether it contains the number 4 and your confidence between 0 and 1.',
  { schema: { type: 'object', properties: { containsFour: { type: 'boolean' }, confidence: { type: 'number' } }, required: ['containsFour'] } },
)
return { prose, containsFour: judged === null ? null : judged.containsFour }"#,
        ))
        .expect("start");
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(
        result.value,
        json!({"prose": "The answer is 4.", "containsFour": true})
    );
    assert_eq!(result.agents_started, 2);
    run.dispose().await;
    let ids = child_ids.lock().clone();
    assert_eq!(ids.len(), 2);
    assert!(
        ids.iter()
            .all(|id| harness.dependencies.agents.get(id).is_none())
    );
    {
        let events = events.lock();
        assert_eq!(events.first().map(String::as_str), Some("workflow/start"));
        assert_eq!(events.last().map(String::as_str), Some("workflow/end"));
        assert_eq!(
            events
                .iter()
                .filter(|name| name.as_str() == "workflow/phase")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|name| name.as_str() == "workflow/agent-start")
                .count(),
            2
        );
    }
    assert_eq!(harness.adapter.requests.lock().len(), 2);
    harness.dispose().await;
}

#[tokio::test]
async fn schema_child_without_a_committed_structured_value_reaches_the_script_as_null() {
    let harness = Harness::new([
        Reply::Text("prose only".to_owned()),
        Reply::Text("still prose after the nudge".to_owned()),
    ])
    .await;
    let run = harness
        .engine
        .start(harness.request(
            "null-path",
            r"const judged = await agent('judge it', { schema: { type: 'object', properties: { v: { type: 'string' } } } })
return { got: judged === null ? 'null' : 'value' }",
        ))
        .expect("start");
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.value, json!({"got": "null"}));
    run.dispose().await;
    harness.dispose().await;
}
