//! Assembled Ralph parity over the real workflow engine and fresh-child stack.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentHandle, AgentOptions, CreateAgentOptions};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    MessageSource, ModelId, ProviderId, StreamChunk, UserMessage,
};
use seekdeep_subagent::SubagentRuntime;
use seekdeep_subagent_in_process_driver::STRUCTURED_OUTPUT_TOOL;
use seekdeep_subagent_spawn_in_process::Config as SpawnConfig;
use seekdeep_tool_ralph::Config as RalphConfig;
use seekdeep_tools::{ToolExecutionInput, ToolExecutionResult};
use seekdeep_workflow::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowEngineService, WorkflowEventName,
};
use seekdeep_workflow_worker_thread::{Config as WorkflowConfig, WorkerThreadWorkflowEngine};
use serde_json::{Value, json};

enum Reply {
    Text(String),
    Structured(Value),
    MaxTokens(String),
    Hang,
}

struct ScriptedAdapter {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options.clone());
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
            Reply::MaxTokens(text) => AdapterStream::new(stream::iter(vec![
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::MaxTokens,
                    replay_state: None,
                }),
            ])),
            Reply::Hang => {
                let signal = options.signal.expect("agent-loop signal");
                AdapterStream::new(async_stream::stream! {
                    signal.cancelled().await;
                    if false {
                        yield Ok(StreamChunk::Finish {
                            reason: FinishReason::Stop,
                            replay_state: None,
                        });
                    }
                })
            }
        }
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    adapter: Arc<ScriptedAdapter>,
    parent: AgentHandle,
}

impl Harness {
    async fn new(replies: impl IntoIterator<Item = Reply>, config: RalphConfig) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .expect("agent-loop dependencies");
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
        let workflow = WorkerThreadWorkflowEngine::new(&context, WorkflowConfig::default())
            .expect("workflow engine");
        WorkflowEngineService::new(workflow)
            .provide(&context)
            .expect("workflow service");
        seekdeep_tool_ralph::apply(&context, &config).expect("Ralph tool");

        let mut options = CreateAgentOptions::new(SessionId::new("ralph-parent"));
        options.meta.cwd = Some("/tmp/ralph-shared-workspace".to_owned());
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
            parent,
        }
    }

    async fn call(&self, call: &str, arguments: Value, signal: AbortSignal) -> ToolExecutionResult {
        let mut input = ToolExecutionInput::new(CallId::new(call), "ralph", arguments, signal);
        input.agent = Some(self.parent.agent.clone());
        input.agent_session = Some(self.parent.agent.session().clone());
        self.dependencies.tools.execute(input).await
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

fn text(result: &ToolExecutionResult) -> &str {
    match result.content().first() {
        Some(ContentBlock::Text { text }) => text,
        other => panic!("expected text result, got {other:?}"),
    }
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn continue_report() -> Value {
    json!({
        "status": "continue",
        "summary": "ROUND_ONE_HANDOFF",
        "evidence": ["Created migration-a.rs."],
        "nextSteps": ["Finish migration-b.rs."],
        "blocker": ""
    })
}

fn complete_report() -> Value {
    json!({
        "status": "complete",
        "summary": "Both migration slices are complete.",
        "evidence": ["Focused migration tests pass."],
        "nextSteps": [],
        "blocker": ""
    })
}

struct Observations {
    children: Arc<Mutex<Vec<Arc<Agent>>>>,
    phases: Arc<Mutex<Vec<String>>>,
    outcomes: Arc<Mutex<Vec<String>>>,
}

fn observe_children(harness: &Harness) -> Observations {
    let children = Arc::new(Mutex::new(Vec::new()));
    let phases = Arc::new(Mutex::new(Vec::new()));
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let seen_children = children.clone();
    let agents = harness.dependencies.agents.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            WorkflowEventName::AgentStart.as_str(),
            move |_, args| {
                let info = args
                    .get::<WorkflowAgentInfo>(1)
                    .ok_or_else(|| anyhow::anyhow!("missing workflow agent info"))?;
                let child = agents
                    .get(&info.child_id)
                    .ok_or_else(|| anyhow::anyhow!("child was not published"))?;
                seen_children.lock().push(child);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("child observer");
    let seen_phases = phases.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            WorkflowEventName::Phase.as_str(),
            move |_, args| {
                let title = args
                    .get::<String>(1)
                    .ok_or_else(|| anyhow::anyhow!("missing workflow phase"))?;
                seen_phases.lock().push((*title).clone());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("phase observer");
    let seen_outcomes = outcomes.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            WorkflowEventName::AgentEnd.as_str(),
            move |_, args| {
                let info = args
                    .get::<WorkflowAgentEndInfo>(1)
                    .ok_or_else(|| anyhow::anyhow!("missing workflow agent end"))?;
                seen_outcomes
                    .lock()
                    .push(format!("{:?}", info.outcome).to_lowercase());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("outcome observer");
    Observations {
        children,
        phases,
        outcomes,
    }
}

#[tokio::test]
async fn fresh_rounds_use_distinct_empty_children_shared_cwd_and_only_the_prior_handoff() {
    let harness = Harness::new(
        [
            Reply::Text("PARENT_HISTORY_MARKER".to_owned()),
            Reply::Structured(continue_report()),
            Reply::Structured(complete_report()),
        ],
        RalphConfig {
            max_rounds: 2,
            ..RalphConfig::default()
        },
    )
    .await;
    harness
        .parent
        .agent
        .followup(user("PARENT_PROMPT_MARKER"))
        .expect("parent followup");
    harness
        .parent
        .agent
        .when_idle()
        .expect("parent controller")
        .await
        .expect("parent idle");
    let observed = observe_children(&harness);

    let result = harness
        .call(
            "ralph-integration",
            json!({"objective": "Complete both migration slices.", "maxRounds": 2}),
            AbortSignal::default(),
        )
        .await;
    assert!(!result.is_error(), "{}", text(&result));
    assert!(text(&result).contains("Ralph worker reported completion after 2 rounds."));
    assert_eq!(observed.phases.lock().as_slice(), ["Fresh-agent rounds"]);
    let children = observed.children.lock().clone();
    assert_eq!(children.len(), 2);
    assert_ne!(children[0].id(), children[1].id());
    for child in &children {
        let header = child.session().header();
        assert_eq!(header.cwd.as_deref(), Some("/tmp/ralph-shared-workspace"));
        assert_eq!(
            header.parent_session.as_ref(),
            Some(harness.parent.agent.id())
        );
        assert_eq!(header.seed_length, None);
        assert!(harness.dependencies.agents.get(child.id()).is_none());
    }

    let (first_child, second_child) = {
        let requests = harness.adapter.requests.lock();
        assert_eq!(requests.len(), 3);
        (
            serde_json::to_string(&requests[1].messages).expect("first messages"),
            serde_json::to_string(&requests[2].messages).expect("second messages"),
        )
    };
    assert!(!first_child.contains("PARENT_PROMPT_MARKER"));
    assert!(!first_child.contains("PARENT_HISTORY_MARKER"));
    assert!(!first_child.contains("ROUND_ONE_HANDOFF"));
    assert!(!second_child.contains("PARENT_PROMPT_MARKER"));
    assert!(!second_child.contains("PARENT_HISTORY_MARKER"));
    assert!(second_child.contains("ROUND_ONE_HANDOFF"));
    harness.dispose().await;
}

#[tokio::test]
async fn failed_round_reports_the_last_good_handoff_and_disposes_every_child() {
    let harness = Harness::new(
        [
            Reply::Structured(continue_report()),
            Reply::MaxTokens("unfinished child output".to_owned()),
        ],
        RalphConfig {
            max_rounds: 2,
            ..RalphConfig::default()
        },
    )
    .await;
    let observed = observe_children(&harness);
    let result = harness
        .call(
            "ralph-child-failure",
            json!({"objective": "Complete both migration slices.", "maxRounds": 2}),
            AbortSignal::default(),
        )
        .await;
    assert!(result.is_error());
    assert!(text(&result).contains("Ralph round 2 child failed"));
    assert!(text(&result).contains("Last successful handoff:"));
    assert!(text(&result).contains("ROUND_ONE_HANDOFF"));
    let children = observed.children.lock().clone();
    assert_eq!(children.len(), 2);
    assert!(
        children
            .iter()
            .all(|child| harness.dependencies.agents.get(child.id()).is_none())
    );
    harness.dispose().await;
}

#[tokio::test]
async fn real_fixed_script_enforces_terminal_and_report_semantics() {
    let cases = vec![
        (
            json!({
                "status": "blocked",
                "summary": "External authorization is required.",
                "evidence": ["The local implementation is ready."],
                "nextSteps": ["Continue after authorization."],
                "blocker": "The required external authorization is unavailable."
            }),
            2,
            16_384,
            false,
            "Ralph worker reported a blocker after 1 round.",
        ),
        (
            json!({
                "status": "continue",
                "summary": "One slice is complete.",
                "evidence": ["The first focused test passes."],
                "nextSteps": ["Implement the remaining slice."],
                "blocker": ""
            }),
            1,
            16_384,
            false,
            "Ralph reached its 1 round limit; the worker reported work remaining.",
        ),
        (
            json!({
                "status": "continue",
                "summary": " padded summary ",
                "evidence": ["A focused test passes."],
                "nextSteps": ["Continue implementation."],
                "blocker": ""
            }),
            1,
            16_384,
            true,
            "summary must be non-empty and normalized",
        ),
        (
            json!({
                "status": "continue",
                "summary": "Work remains.",
                "evidence": ["A focused test passes."],
                "nextSteps": [],
                "blocker": ""
            }),
            1,
            16_384,
            true,
            "a continuing Ralph report needs nextSteps and an empty blocker",
        ),
        (
            json!({
                "status": "continue",
                "summary": "x".repeat(300),
                "evidence": ["A focused test passes."],
                "nextSteps": ["Continue implementation."],
                "blocker": ""
            }),
            1,
            100,
            true,
            "Ralph round report exceeds maxHandoffChars",
        ),
    ];
    for (index, (report, max_rounds, max_handoff_chars, error, fragment)) in
        cases.into_iter().enumerate()
    {
        let harness = Harness::new(
            [Reply::Structured(report)],
            RalphConfig {
                max_rounds,
                max_handoff_chars,
                ..RalphConfig::default()
            },
        )
        .await;
        let result = harness
            .call(
                &format!("script-enforcement-{index}"),
                json!({"objective": "Complete the scoped work.", "maxRounds": max_rounds}),
                AbortSignal::default(),
            )
            .await;
        assert_eq!(result.is_error(), error, "case {index}: {}", text(&result));
        assert!(
            text(&result).contains(fragment),
            "case {index}: expected {fragment:?} in {:?}",
            text(&result)
        );
        harness.dispose().await;
    }
}

#[tokio::test]
async fn cancellation_quiesces_the_real_workflow_and_fresh_child() {
    let harness = Harness::new(
        [Reply::Hang],
        RalphConfig {
            max_rounds: 2,
            ..RalphConfig::default()
        },
    )
    .await;
    let observed = observe_children(&harness);
    let signal = AbortSignal::default();
    let tools = harness.dependencies.tools.clone();
    let parent = harness.parent.agent.clone();
    let mut task = tokio::spawn({
        let signal = signal.clone();
        async move {
            let mut input = ToolExecutionInput::new(
                CallId::new("ralph-real-cancel"),
                "ralph",
                json!({"objective": "Keep working until cancelled.", "maxRounds": 2}),
                signal,
            );
            input.agent = Some(parent.clone());
            input.agent_session = Some(parent.session().clone());
            tools.execute(input).await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while observed.children.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child started");
    signal.abort();
    let result = if let Ok(result) =
        tokio::time::timeout(std::time::Duration::from_secs(20), &mut task).await
    {
        result.expect("tool task")
    } else {
        let snapshots = observed
            .children
            .lock()
            .iter()
            .map(|child| {
                (
                    child.id().to_string(),
                    format!("{:?}", child.status()),
                    child
                        .session()
                        .events()
                        .iter()
                        .map(|event| event.event_type.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        task.abort();
        panic!(
            "cancellation stalled: signal_aborted={}, outcomes={:?}, children={snapshots:?}",
            signal.is_aborted(),
            observed.outcomes.lock().as_slice()
        );
    };
    assert!(result.is_error());
    assert!(text(&result).contains("Ralph workflow was cancelled"));
    assert_eq!(observed.outcomes.lock().as_slice(), ["cancelled"]);
    let child = observed.children.lock()[0].clone();
    assert!(harness.dependencies.agents.get(child.id()).is_none());
    harness.dispose().await;
}
