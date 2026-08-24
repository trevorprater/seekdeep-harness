//! Real `AgentLoop` integration parity for plan-mode boundary application.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentHandle, AgentOptions, CreateAgentOptions, RequestErrorAction};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{SessionEvent, SessionId, derive_event_message};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure,
    MessageSource, ModelId, ProviderId, StreamChunk, UserMessage,
};
use seekdeep_plan_mode::{PlanModeConfig, PlanModeController, fold_plan_mode};
use seekdeep_tools::{ContentToolFixtureOptions, define_content_tool_fixture};
use serde_json::{Value, json};

const PLAN_SECTION: &str = "Test plan mode instructions.";

enum Reply {
    Text(String),
    ToolCall { id: String, name: String },
    Error(String),
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
            Reply::ToolCall { id, name } => AdapterStream::new(stream::iter(vec![
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new(id),
                        name,
                        arguments: "{}".to_owned(),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ])),
            Reply::Error(message) => AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                reason: FinishReason::Error {
                    failure: LlmFailure {
                        message,
                        code: "SERVER".to_owned(),
                        status: Some(503),
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                replay_state: None,
            })])),
        }
    }
}

struct Harness {
    context: Context,
    _dependencies: AgentLoopTestDependencies,
    adapter: Arc<ScriptedAdapter>,
    controller: Arc<PlanModeController>,
    agent: AgentHandle,
}

impl Harness {
    async fn new(replies: impl IntoIterator<Item = Reply>, id: &str) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .expect("dependencies");
        let adapter = Arc::new(ScriptedAdapter {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        });
        dependencies
            .llm
            .register_adapter(&["mock".to_owned()], adapter.clone())
            .expect("adapter");
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
        let controller = PlanModeController::install(
            &context,
            &PlanModeConfig {
                section: PLAN_SECTION.to_owned(),
            },
        )
        .expect("plan mode");
        for name in ["read", "write"] {
            let tool_name = name.to_owned();
            let definition = define_content_tool_fixture(ContentToolFixtureOptions::new(
                name,
                format!("test tool {name}"),
                json!({}),
                Arc::new(move |_args: Value, _run| {
                    let tool_name = tool_name.clone();
                    Box::pin(async move {
                        Ok(vec![ContentBlock::Text {
                            text: format!("ran {tool_name}"),
                        }])
                    })
                }),
            ))
            .expect("tool fixture");
            dependencies
                .tools
                .register(&context, definition)
                .expect("register tool");
        }
        let mut options = CreateAgentOptions::new(SessionId::new(id));
        options.agent_options = AgentOptions {
            provider: Some(ProviderId::new("mock")),
            model: Some(ModelId::new("mock")),
            max_tokens: None,
            subagent_depth: None,
        };
        let agent = dependencies.agents.create(options).await.expect("agent");
        Self {
            context,
            _dependencies: dependencies,
            adapter,
            controller,
            agent,
        }
    }

    async fn turn(&self, text: &str) {
        self.agent.agent.followup(user(text)).expect("followup");
        self.agent
            .agent
            .when_idle()
            .expect("controller")
            .await
            .expect("idle");
    }

    fn requests(&self) -> Vec<GenerateOptions> {
        self.adapter.requests.lock().clone()
    }

    fn events(&self) -> Vec<SessionEvent> {
        self.agent.agent.session().events()
    }

    async fn dispose(&self) {
        self.agent.dispose().await.expect("dispose agent");
        self.context
            .root_fiber()
            .dispose()
            .await
            .expect("dispose context");
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

fn tool_names(request: &GenerateOptions) -> Vec<&str> {
    request
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect()
}

fn plugin_notices(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(derive_event_message)
        .filter(|message| message.source().kind == "plugin")
        .flat_map(|message| {
            message
                .content()
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn idle_pre_turn_selection_shapes_the_first_request_without_gating_write() {
    let harness = Harness::new(
        [
            Reply::ToolCall {
                id: "call-1".to_owned(),
                name: "write".to_owned(),
            },
            Reply::Text("Noted in the plan.".to_owned()),
        ],
        "it-plan-seed",
    )
    .await;
    harness.controller.set(&harness.agent.agent, true).unwrap();
    harness.turn("explore the repo").await;

    let requests = harness.requests();
    assert_eq!(
        tool_names(&requests[0]),
        ["exit_plan_mode", "read", "write"]
    );
    assert!(
        requests[0]
            .system
            .as_deref()
            .unwrap_or_default()
            .contains(PLAN_SECTION)
    );
    let events = harness.events();
    let plan = events
        .iter()
        .find(|event| event.event_type == "plan/mode")
        .unwrap();
    let header = events
        .iter()
        .find(|event| event.event_type == "request/header")
        .unwrap();
    assert!(plan.seq < header.seq);
    let result = events
        .iter()
        .find(|event| event.event_type == "tool/result")
        .and_then(derive_event_message)
        .expect("tool result message");
    assert!(result.content().iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolResult {
                is_error: Some(false),
                ..
            }
        )
    }));
    assert!(fold_plan_mode(&events, events.len()));
    assert!(plugin_notices(&events).is_empty());
    harness.dispose().await;
}

#[tokio::test]
async fn between_turn_flip_adds_one_notice_and_keeps_tool_schemas_stable() {
    let harness = Harness::new(
        [
            Reply::Text("First turn, default mode.".to_owned()),
            Reply::Text("Second turn, plan mode.".to_owned()),
        ],
        "it-plan-flip",
    )
    .await;
    harness.turn("hello").await;
    let first = harness.requests()[0].clone();
    assert!(!fold_plan_mode(&harness.events(), harness.events().len()));
    harness.controller.set(&harness.agent.agent, true).unwrap();
    harness.turn("now plan").await;

    let requests = harness.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].tools, first.tools);
    assert!(
        requests[1]
            .system
            .as_deref()
            .unwrap_or_default()
            .contains(PLAN_SECTION)
    );
    let events = harness.events();
    assert!(fold_plan_mode(&events, events.len()));
    assert_eq!(
        plugin_notices(&events),
        ["The user switched this session to plan mode."]
    );
    harness.dispose().await;
}

#[tokio::test]
async fn retry_settlement_defers_flip_until_the_following_step() {
    let harness = Harness::new(
        [
            Reply::Error("temporarily unavailable".to_owned()),
            Reply::Text("Recovered with the original step assembly.".to_owned()),
            Reply::Text("Entered plan mode on the next step.".to_owned()),
        ],
        "it-plan-retry-flip",
    )
    .await;
    let controller = harness.controller.clone();
    let target = harness.agent.agent.clone();
    harness
        .context
        .events()
        .on_waterfall(
            &harness.context,
            "agent/request-error",
            move |_, _, _| {
                let controller = controller.clone();
                let target = target.clone();
                Box::pin(async move {
                    controller.set(&target, true)?;
                    Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    harness.turn("plan after the transient failure").await;
    let requests = harness.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .system
            .as_deref()
            .is_none_or(|system| !system.contains(PLAN_SECTION))
    );
    assert!(
        requests[1]
            .system
            .as_deref()
            .is_none_or(|system| !system.contains(PLAN_SECTION))
    );
    assert_eq!(requests[1].tools, requests[0].tools);
    assert_eq!(
        harness.controller.get(&harness.agent.agent).pending,
        Some(true)
    );
    assert!(
        !harness
            .events()
            .iter()
            .any(|event| event.event_type == "plan/mode")
    );

    harness.turn("continue with the plan").await;
    let requests = harness.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .system
            .as_deref()
            .unwrap_or_default()
            .contains(PLAN_SECTION)
    );
    assert_eq!(requests[2].tools, requests[0].tools);
    let events = harness.events();
    let plan = events
        .iter()
        .find(|event| event.event_type == "plan/mode")
        .unwrap();
    let first_end = events.iter().find(|event| {
        event.event_type == "step/end" && event.data["turn"] == 1 && event.data["step"] == 1
    });
    let next_start = events.iter().find(|event| {
        event.event_type == "step/start" && event.data["turn"] == 2 && event.data["step"] == 1
    });
    assert!(first_end.unwrap().seq < plan.seq);
    assert!(plan.seq < next_start.unwrap().seq);
    assert_eq!(
        plugin_notices(&events),
        ["The user switched this session to plan mode."]
    );
    harness.dispose().await;
}
