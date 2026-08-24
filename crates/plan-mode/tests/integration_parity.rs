//! Real `AgentLoop` integration parity for plan-mode boundary application.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{
    AgentEvents, AgentHandle, AgentOptions, CreateAgentOptions, PreStepDecision,
    RequestErrorAction, assemble_context_for,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_code_runtime::{CodeRunRequest, CodeRunResult, CodeRuntime, CodeRuntimeBackend};
use seekdeep_cordis::{Context, EventOptions, EventReply, Fiber};
use seekdeep_core::session::{
    AppendOptions, SessionEvent, SessionId, SurfaceOp, derive_event_message,
};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure,
    MessageSource, ModelId, ProviderId, StreamChunk, ToolSchema, UserMessage,
};
use seekdeep_plan_mode::{PlanModeConfig, PlanModeController, fold_plan_mode};
use seekdeep_tools::{
    ContentToolFixtureOptions, ToolExecutionInput, ToolPresentationMode,
    define_content_tool_fixture,
};
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

struct FakeCodeBackend;

#[async_trait]
impl CodeRuntimeBackend for FakeCodeBackend {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn isolation(&self) -> &'static str {
        "fake"
    }

    async fn run(&self, _request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
        Ok(CodeRunResult::default())
    }
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
    dependencies: AgentLoopTestDependencies,
    adapter: Arc<ScriptedAdapter>,
    controller: Arc<PlanModeController>,
    plan_fiber: Arc<Fiber>,
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
        let plan_fiber = Fiber::active_child(format!("plan-mode-{id}"));
        let plan_context = context.with_fiber(plan_fiber.clone());
        let controller = PlanModeController::install(
            &plan_context,
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
            dependencies,
            adapter,
            controller,
            plan_fiber,
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

    async fn boundary(&self) -> PreStepDecision {
        let message = user("boundary probe");
        let decision = AgentEvents::new(self.context.clone(), self.agent.agent.clone())
            .waterfall(
                "agent/pre-step",
                AgentPreStepEvent {
                    messages: vec![message.clone()],
                    turn: 1,
                    step: 1,
                    signal: seekdeep_llm::AbortSignal::default(),
                },
                move || async move {
                    Ok(PreStepDecision::Enter {
                        messages: vec![message],
                    })
                },
            )
            .await
            .expect("pre-step");
        if let PreStepDecision::Enter { messages } = &decision {
            for message in messages.iter().skip(1) {
                self.agent
                    .agent
                    .session()
                    .append(
                        "user/message",
                        serde_json::to_value(message).unwrap(),
                        AppendOptions {
                            surface_op: Some(SurfaceOp::append()),
                            ..AppendOptions::default()
                        },
                    )
                    .unwrap();
            }
        }
        decision
    }

    async fn assembly(&self) -> seekdeep_system_prompt::PromptAssembly {
        self.dependencies()
            .system_prompt
            .assemble(assemble_context_for(&self.agent.agent, None))
            .await
            .expect("assembly")
    }

    fn dependencies(&self) -> &AgentLoopTestDependencies {
        &self.dependencies
    }

    async fn dispose(&self) {
        self.plan_fiber.dispose().await.ok();
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

fn open_turn(agent: &Arc<seekdeep_agent::Agent>, turn: u64) {
    agent
        .session()
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .unwrap();
}

fn close_turn(agent: &Arc<seekdeep_agent::Agent>, turn: u64) {
    agent
        .session()
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
}

fn header(agent: &Arc<seekdeep_agent::Agent>) {
    agent
        .session()
        .append(
            "request/header",
            json!({
                "header": {"config": {"provider": "test", "model": "test-model"}},
                "reason": "initial"
            }),
            AppendOptions::default(),
        )
        .unwrap();
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

async fn code_mode_assemblies(
    mode: ToolPresentationMode,
) -> (
    seekdeep_system_prompt::PromptAssembly,
    seekdeep_system_prompt::PromptAssembly,
    seekdeep_system_prompt::PromptAssembly,
) {
    let context = Context::new();
    let mut options = AgentLoopTestDependenciesOptions::default();
    options.tools.mode = mode;
    let dependencies = mount_agent_loop_test_dependencies(&context, options).unwrap();
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
    .unwrap();
    dependencies.agents.set_factory(Arc::new(factory)).unwrap();
    let code_runtime = Arc::new(CodeRuntime::new(Arc::new(FakeCodeBackend)));
    code_runtime.provide(&context).unwrap();
    for name in ["read", "write"] {
        let definition = define_content_tool_fixture(ContentToolFixtureOptions::new(
            name,
            format!("test tool {name}"),
            json!({}),
            Arc::new(|_args: Value, _run| Box::pin(async { Ok(Vec::new()) })),
        ))
        .unwrap();
        dependencies.tools.register(&context, definition).unwrap();
    }
    let agent = dependencies
        .agents
        .create(CreateAgentOptions::new(SessionId::new(format!(
            "code-mode-{mode:?}"
        ))))
        .await
        .unwrap();
    let bare = dependencies
        .system_prompt
        .assemble(assemble_context_for(&agent.agent, None))
        .await
        .unwrap();
    let controller = PlanModeController::install(
        &context,
        &PlanModeConfig {
            section: PLAN_SECTION.to_owned(),
        },
    )
    .unwrap();
    let default = dependencies
        .system_prompt
        .assemble(assemble_context_for(&agent.agent, None))
        .await
        .unwrap();
    controller.set(&agent.agent, true).unwrap();
    let planning = dependencies
        .system_prompt
        .assemble(assemble_context_for(&agent.agent, None))
        .await
        .unwrap();
    agent.dispose().await.unwrap();
    context.root_fiber().dispose().await.unwrap();
    (bare, default, planning)
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

#[tokio::test]
async fn controller_get_set_noops_immediate_commits_and_pending_reversal_match_source() {
    let harness = Harness::new([], "controller-state").await;
    assert_eq!(
        harness.controller.get(&harness.agent.agent),
        seekdeep_plan_mode::PlanGetResult {
            active: false,
            pending: None,
        }
    );
    open_turn(&harness.agent.agent, 1);
    assert_eq!(
        harness.controller.set(&harness.agent.agent, false).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Noop
    );
    assert_eq!(
        harness.controller.set(&harness.agent.agent, true).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Queued
    );
    assert_eq!(
        harness.controller.set(&harness.agent.agent, true).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Noop
    );
    assert_eq!(
        harness.controller.get(&harness.agent.agent).pending,
        Some(true)
    );
    close_turn(&harness.agent.agent, 1);
    assert_eq!(
        harness.controller.set(&harness.agent.agent, false).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Cancelled
    );
    assert!(
        !harness
            .events()
            .iter()
            .any(|event| event.event_type == "plan/mode")
    );

    assert_eq!(
        harness.controller.set(&harness.agent.agent, true).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Committed
    );
    assert_eq!(
        harness.controller.set(&harness.agent.agent, false).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Committed
    );
    let before = harness
        .events()
        .iter()
        .filter(|event| event.event_type == "plan/mode")
        .count();
    let _ = harness.boundary().await;
    let after = harness
        .events()
        .iter()
        .filter(|event| event.event_type == "plan/mode")
        .count();
    assert_eq!(before, 2);
    assert_eq!(after, before);
    harness.dispose().await;
}

#[tokio::test]
async fn boundary_flush_nets_zero_and_emits_only_required_notices() {
    let harness = Harness::new([], "boundary-flush").await;
    open_turn(&harness.agent.agent, 1);
    harness.controller.set(&harness.agent.agent, true).unwrap();
    let _ = harness.boundary().await;
    assert!(fold_plan_mode(&harness.events(), harness.events().len()));
    assert!(plugin_notices(&harness.events()).is_empty());
    close_turn(&harness.agent.agent, 1);

    let net = Harness::new([], "boundary-net-zero").await;
    open_turn(&net.agent.agent, 1);
    net.controller.set(&net.agent.agent, true).unwrap();
    assert_eq!(
        net.controller.set(&net.agent.agent, false).unwrap(),
        seekdeep_plan_mode::PlanSetOutcome::Cancelled
    );
    let _ = net.boundary().await;
    assert!(
        !net.events()
            .iter()
            .any(|event| event.event_type == "plan/mode")
    );
    assert!(plugin_notices(&net.events()).is_empty());

    let noticed = Harness::new([], "boundary-notice").await;
    header(&noticed.agent.agent);
    open_turn(&noticed.agent.agent, 1);
    noticed.controller.set(&noticed.agent.agent, true).unwrap();
    let _ = noticed.boundary().await;
    assert_eq!(
        plugin_notices(&noticed.events()),
        ["The user switched this session to plan mode."]
    );
    let _ = noticed.boundary().await;
    assert_eq!(plugin_notices(&noticed.events()).len(), 1);

    harness.dispose().await;
    net.dispose().await;
    noticed.dispose().await;
}

#[tokio::test]
async fn boundary_append_failure_is_contained_and_keeps_intent_pending_for_retry() {
    let harness = Harness::new([], "boundary-append-failure").await;
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let fail_once = fail.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "internal/dispatch",
            move |_, args| {
                let name = args.get::<String>(1);
                let event_args = args.get::<seekdeep_cordis::EventArgs>(2);
                let plan_mode = name.as_deref().map(String::as_str) == Some("session/event")
                    && event_args
                        .and_then(|args| args.get::<SessionEvent>(1))
                        .is_some_and(|event| event.event_type == "plan/mode");
                if plan_mode && fail_once.swap(false, std::sync::atomic::Ordering::AcqRel) {
                    anyhow::bail!("backend gone");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    open_turn(&harness.agent.agent, 1);
    harness.controller.set(&harness.agent.agent, true).unwrap();
    let decision = harness.boundary().await;
    assert!(matches!(decision, PreStepDecision::Enter { .. }));
    assert_eq!(
        harness.controller.get(&harness.agent.agent),
        seekdeep_plan_mode::PlanGetResult {
            active: false,
            pending: Some(true),
        }
    );
    assert!(
        !harness
            .events()
            .iter()
            .any(|event| event.event_type == "plan/mode")
    );
    let _ = harness.boundary().await;
    assert_eq!(
        harness.controller.get(&harness.agent.agent),
        seekdeep_plan_mode::PlanGetResult {
            active: true,
            pending: None,
        }
    );
    harness.dispose().await;
}

#[tokio::test]
async fn soft_layer_keeps_schemas_stable_and_plan_is_guidance_not_execution_gate() {
    let harness = Harness::new([], "soft-layer").await;
    let default = harness.assembly().await;
    assert_eq!(
        default
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["exit_plan_mode", "read", "write"]
    );
    assert_eq!(
        default
            .sections
            .iter()
            .find(|section| section.name == "plan:policy")
            .map(|section| section.text.as_str()),
        Some("")
    );
    let agentless = harness
        .dependencies()
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("soft-agentless-write"),
            "write",
            json!({}),
            seekdeep_llm::AbortSignal::default(),
        ))
        .await;
    assert!(!agentless.is_error(), "{:?}", agentless.error());
    let mut default_write = ToolExecutionInput::new(
        CallId::new("soft-default-write"),
        "write",
        json!({}),
        seekdeep_llm::AbortSignal::default(),
    );
    default_write.agent = Some(harness.agent.agent.clone());
    default_write.agent_session = Some(harness.agent.agent.session().clone());
    let default_write = harness.dependencies().tools.execute(default_write).await;
    assert!(!default_write.is_error(), "{:?}", default_write.error());
    harness.controller.set(&harness.agent.agent, true).unwrap();
    let planning = harness.assembly().await;
    assert_eq!(planning.tools, default.tools);
    assert_eq!(
        planning
            .sections
            .iter()
            .find(|section| section.name == "plan:policy")
            .map(|section| section.text.as_str()),
        Some(PLAN_SECTION)
    );
    let agentless = harness
        .dependencies()
        .system_prompt
        .assemble(seekdeep_system_prompt::AssembleContext::default())
        .await
        .unwrap();
    assert_eq!(
        agentless
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["exit_plan_mode", "read", "write"]
    );
    for name in ["read", "write"] {
        let mut input = ToolExecutionInput::new(
            CallId::new(format!("soft-{name}")),
            name,
            json!({}),
            seekdeep_llm::AbortSignal::default(),
        );
        input.agent = Some(harness.agent.agent.clone());
        input.agent_session = Some(harness.agent.agent.session().clone());
        let result = harness.dependencies().tools.execute(input).await;
        assert!(!result.is_error(), "{name}: {:?}", result.error());
    }
    harness.dispose().await;
}

#[tokio::test]
async fn code_and_both_modes_keep_the_sdk_byte_stable_with_the_exit_binding() {
    let (code_bare, code_default, code_plan) =
        code_mode_assemblies(ToolPresentationMode::Code).await;
    assert_eq!(
        code_plan
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        [seekdeep_tools::RUN_CODE_NAME]
    );
    let default_sdk = code_default
        .sections
        .iter()
        .find(|section| section.name == "tools:sdk")
        .map(|section| section.text.as_str())
        .unwrap_or_default();
    let plan_sdk = code_plan
        .sections
        .iter()
        .find(|section| section.name == "tools:sdk")
        .map(|section| section.text.as_str())
        .unwrap_or_default();
    assert_eq!(plan_sdk, default_sdk);
    let bare_sdk = code_bare
        .sections
        .iter()
        .find(|section| section.name == "tools:sdk")
        .map(|section| section.text.as_str())
        .unwrap_or_default();
    assert!(!bare_sdk.contains("exit_plan_mode"));
    assert_ne!(default_sdk, bare_sdk);
    for binding in ["exit_plan_mode", "read", "write"] {
        assert!(plan_sdk.contains(binding), "missing {binding} in SDK");
    }

    let (_, _, both) = code_mode_assemblies(ToolPresentationMode::Both).await;
    let mut names = both
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["exit_plan_mode", "read", "run_code", "write"]);
    let sdk = both
        .sections
        .iter()
        .find(|section| section.name == "tools:sdk")
        .map(|section| section.text.as_str())
        .unwrap_or_default();
    for binding in ["exit_plan_mode", "read", "write"] {
        assert!(sdk.contains(binding), "missing {binding} in both-mode SDK");
    }
}

#[tokio::test]
async fn foreign_assembly_additions_survive_in_default_and_plan_mode() {
    let harness = Harness::new([], "foreign-assembly").await;
    harness
        .dependencies()
        .system_prompt
        .on_assemble(
            &harness.context,
            |_assembly, _context, next| async move {
                let mut final_assembly = next.run().await?;
                final_assembly.tools.push(ToolSchema {
                    name: "added-later".to_owned(),
                    description: "added after next()".to_owned(),
                    parameters: serde_json::Map::new(),
                });
                Ok(final_assembly)
            },
            EventOptions::default(),
        )
        .unwrap();
    for active in [false, true] {
        if active {
            harness.controller.set(&harness.agent.agent, true).unwrap();
        }
        assert_eq!(
            harness
                .assembly()
                .await
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["exit_plan_mode", "read", "write", "added-later"]
        );
    }
    harness.dispose().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Intermediate states share one service instance.
async fn optional_command_service_mounts_unmounts_and_rebinds_with_its_own_lifecycle() {
    let harness = Harness::new([], "optional-commands").await;
    let first_fiber = Fiber::active_child("commands-first");
    let first_context = harness.context.with_fiber(first_fiber.clone());
    let first = seekdeep_commands::install(&first_context).unwrap();
    assert_eq!(
        first
            .list(&harness.agent.agent)
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["plan"]
    );
    let execution = first
        .execute(
            harness.agent.agent.clone(),
            "/plan",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .expect("plan command");
    assert_eq!(
        execution.result.text(),
        Some("Plan mode on. Use /plan off to leave.")
    );
    assert!(fold_plan_mode(&harness.events(), harness.events().len()));
    let off = first
        .execute(
            harness.agent.agent.clone(),
            "/plan off",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(off.result.text(), Some("Plan mode off."));
    let inactive = first
        .execute(
            harness.agent.agent.clone(),
            "/plan off",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        inactive.result.text(),
        Some("Plan mode is already inactive.")
    );

    open_turn(&harness.agent.agent, 1);
    let entering = first
        .execute(
            harness.agent.agent.clone(),
            "/plan",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        entering
            .result
            .text()
            .unwrap()
            .starts_with("Entering plan mode")
    );
    let cancelled = first
        .execute(
            harness.agent.agent.clone(),
            "/plan off",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.result.text(), Some("Plan mode entry cancelled."));
    close_turn(&harness.agent.agent, 1);
    let _ = harness.boundary().await;
    assert!(!fold_plan_mode(&harness.events(), harness.events().len()));

    harness.controller.set(&harness.agent.agent, true).unwrap();
    open_turn(&harness.agent.agent, 2);
    let leaving = first
        .execute(
            harness.agent.agent.clone(),
            "/plan off",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        leaving
            .result
            .text()
            .unwrap()
            .starts_with("Leaving plan mode")
    );
    let repeated = first
        .execute(
            harness.agent.agent.clone(),
            "/plan off",
            seekdeep_llm::AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        repeated
            .result
            .text()
            .unwrap()
            .starts_with("Leaving plan mode")
    );
    let _ = harness.boundary().await;
    assert!(!fold_plan_mode(&harness.events(), harness.events().len()));
    first_fiber.dispose().await.unwrap();
    assert!(first.list(&harness.agent.agent).is_empty());

    let second_fiber = Fiber::active_child("commands-second");
    let second_context = harness.context.with_fiber(second_fiber.clone());
    let second = seekdeep_commands::install(&second_context).unwrap();
    assert_eq!(
        second
            .list(&harness.agent.agent)
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["plan"]
    );
    second_fiber.dispose().await.unwrap();
    harness.dispose().await;
}

#[tokio::test]
async fn plan_fiber_disposal_removes_service_listener_prompt_tool_and_pending_flush() {
    let harness = Harness::new([], "plan-hmr").await;
    open_turn(&harness.agent.agent, 1);
    harness.controller.set(&harness.agent.agent, true).unwrap();
    assert!(harness.context.get(seekdeep_plan_mode::PLAN_MODE).is_some());
    assert!(
        harness
            .dependencies()
            .tools
            .get(seekdeep_plan_mode::EXIT_PLAN_MODE, None)
            .is_some()
    );
    harness.plan_fiber.dispose().await.unwrap();
    assert!(harness.context.get(seekdeep_plan_mode::PLAN_MODE).is_none());
    assert!(
        harness
            .dependencies()
            .tools
            .get(seekdeep_plan_mode::EXIT_PLAN_MODE, None)
            .is_none()
    );
    let assembly = harness
        .dependencies()
        .system_prompt
        .assemble(assemble_context_for(&harness.agent.agent, None))
        .await
        .unwrap();
    assert!(
        assembly
            .sections
            .iter()
            .all(|section| section.name != "plan:policy")
    );
    let _ = harness.boundary().await;
    assert!(
        !harness
            .events()
            .iter()
            .any(|event| event.event_type == "plan/mode")
    );
    harness.agent.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}
