//! Reviewed `exit_plan_mode` behavior over real services and a scripted UI provider.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{AgentEvents, AgentHandle, AgentOptions, CreateAgentOptions, PreStepDecision};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, AgentPreStepEvent, DEFAULT_MAX_PARALLEL_TOOL_CALLS,
};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_code_runtime::{CodeRunRequest, CodeRunResult, CodeRuntime, CodeRuntimeBackend};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session::{
    AppendOptions, SessionEvent, SessionId, SurfaceOp, derive_event_message,
};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, MessageSource, UserMessage};
use seekdeep_plan_mode::{
    APPROVE_LABEL, EXIT_PLAN_MODE, KEEP_PLANNING_LABEL, PlanModeConfig, PlanModeController,
    fold_plan_mode,
};
use seekdeep_tools::{
    GenericCallView, GenericResultView, RUN_CODE_NAME, ToolCallKind, ToolCallView,
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolResult, ToolResultView,
};
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionRequest, UserQuestionError,
    UserQuestionProvider, UserQuestionService,
};
use serde_json::json;
use tokio::sync::oneshot;

enum Response {
    Answer(AskUserQuestionAnswer),
    Error {
        message: String,
        code: Option<String>,
    },
    Pending(oneshot::Receiver<AskUserQuestionAnswer>),
}

struct Provider {
    response: Mutex<Option<Response>>,
    seen: Mutex<Vec<AskUserQuestionRequest>>,
}

struct ExitCodeBackend {
    plan: String,
}

#[async_trait]
impl CodeRuntimeBackend for ExitCodeBackend {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn isolation(&self) -> &'static str {
        "fake"
    }

    async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
        let function = request
            .bindings
            .iter()
            .find(|namespace| namespace.global == "tools")
            .and_then(|namespace| namespace.functions.get(EXIT_PLAN_MODE))
            .ok_or_else(|| anyhow::anyhow!("missing exit_plan_mode binding"))?;
        let value = function(json!({"plan": self.plan})).await?;
        Ok(CodeRunResult {
            value: Some(value),
            logs: Vec::new(),
            error: None,
        })
    }
}

impl Provider {
    fn new(response: Response) -> Arc<Self> {
        Arc::new(Self {
            response: Mutex::new(Some(response)),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl UserQuestionProvider for Provider {
    async fn ask(&self, request: AskUserQuestionRequest) -> anyhow::Result<AskUserQuestionAnswer> {
        self.seen.lock().push(request);
        let response = self.response.lock().take().expect("one review response");
        match response {
            Response::Answer(answer) => Ok(answer),
            Response::Error { message, code } => match code {
                Some(code) => Err(UserQuestionError::new(message, code).into()),
                None => anyhow::bail!(message),
            },
            Response::Pending(receiver) => receiver
                .await
                .map_err(|_| anyhow::anyhow!("review answer sender dropped")),
        }
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    controller: Arc<PlanModeController>,
    plan_fiber: Arc<Fiber>,
    agent: AgentHandle,
    questions: Option<Arc<UserQuestionService>>,
}

impl Harness {
    async fn new(with_questions: bool, active: bool, id: &str) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
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
        let plan_fiber = Fiber::active_child(format!("plan-review-{id}"));
        let plan_context = context.with_fiber(plan_fiber.clone());
        let controller = PlanModeController::install(
            &plan_context,
            &PlanModeConfig {
                section: "Test plan mode instructions.".to_owned(),
            },
        )
        .unwrap();
        let questions =
            with_questions.then(|| seekdeep_user_questions::install(&context).expect("questions"));
        let mut options = CreateAgentOptions::new(SessionId::new(id));
        options.agent_options = AgentOptions::default();
        let agent = dependencies.agents.create(options).await.unwrap();
        if active {
            controller.set(&agent.agent, true).unwrap();
        }
        Self {
            context,
            dependencies,
            controller,
            plan_fiber,
            agent,
            questions,
        }
    }

    fn register(&self, response: Response) -> Arc<Provider> {
        let provider = Provider::new(response);
        self.questions
            .as_ref()
            .expect("questions service")
            .register_provider(&self.context, provider.clone())
            .unwrap();
        provider
    }

    async fn call(&self, plan: &str, signal: AbortSignal, with_agent: bool) -> ToolExecutionResult {
        let mut input = ToolExecutionInput::new(
            CallId::new(format!("review-{}", self.agent.agent.id())),
            EXIT_PLAN_MODE,
            json!({"plan": plan}),
            signal,
        );
        if with_agent {
            input.agent = Some(self.agent.agent.clone());
            input.agent_session = Some(self.agent.agent.session().clone());
        }
        self.dependencies.tools.execute(input).await
    }

    async fn boundary(&self) {
        let message = UserMessage::new(
            vec![ContentBlock::Text {
                text: "boundary".to_owned(),
            }],
            MessageSource::user(),
        );
        let decision = AgentEvents::new(self.context.clone(), self.agent.agent.clone())
            .waterfall(
                "agent/pre-step",
                AgentPreStepEvent {
                    messages: vec![message.clone()],
                    turn: 1,
                    step: 1,
                    signal: AbortSignal::default(),
                },
                move || async move {
                    Ok(PreStepDecision::Enter {
                        messages: vec![message],
                    })
                },
            )
            .await
            .unwrap();
        if let PreStepDecision::Enter { messages } = decision {
            for message in messages.into_iter().skip(1) {
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
    }

    fn events(&self) -> Vec<SessionEvent> {
        self.agent.agent.session().events()
    }

    async fn dispose(&self) {
        self.plan_fiber.dispose().await.ok();
        self.agent.dispose().await.unwrap();
        self.context.root_fiber().dispose().await.unwrap();
    }
}

fn answer(selected: &[&str], custom: Option<&str>) -> AskUserQuestionAnswer {
    AskUserQuestionAnswer {
        answers: vec![AskUserQuestionAnswerItem {
            id: "plan-review".to_owned(),
            selected: selected.iter().map(|value| (*value).to_owned()).collect(),
            custom: custom.map(str::to_owned),
        }],
    }
}

fn text(result: &ToolExecutionResult) -> &str {
    match result.content().first() {
        Some(ContentBlock::Text { text }) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn notices(events: &[SessionEvent]) -> Vec<String> {
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
async fn schema_guards_and_missing_review_services_fail_closed() {
    let harness = Harness::new(false, false, "review-guards").await;
    let schema = harness
        .dependencies
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == EXIT_PLAN_MODE)
        .unwrap();
    assert!(schema.description.starts_with("Use only in plan mode."));
    assert_eq!(schema.parameters["required"], json!(["plan"]));
    assert_eq!(
        schema.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["plan"]
    );
    let agentless = harness.call("# Plan", AbortSignal::default(), false).await;
    assert!(agentless.is_error());
    assert!(text(&agentless).contains("requires a calling agent"));
    let inactive = harness.call("# Plan", AbortSignal::default(), true).await;
    assert!(inactive.is_error());
    assert!(text(&inactive).contains("only available in plan mode"));

    harness.controller.set(&harness.agent.agent, true).unwrap();
    for invalid in ["", "do things"] {
        let result = harness.call(invalid, AbortSignal::default(), true).await;
        assert!(result.is_error());
        assert!(text(&result).contains("requires a non-empty markdown plan"));
    }
    let missing = harness.call("# Plan", AbortSignal::default(), true).await;
    assert!(missing.is_error());
    assert!(text(&missing).contains("no user-questions channel is available"));
    assert!(fold_plan_mode(&harness.events(), harness.events().len()));
    harness.dispose().await;

    let no_provider = Harness::new(true, true, "review-no-provider").await;
    let result = no_provider
        .call("# Plan", AbortSignal::default(), true)
        .await;
    assert!(result.is_error());
    assert!(text(&result).contains("no user-questions provider is registered"));
    no_provider.dispose().await;
}

#[tokio::test]
async fn runtime_owned_child_is_rejected_before_the_review_provider() {
    let harness = Harness::new(true, false, "review-owner-root").await;
    let provider = harness.register(Response::Answer(answer(&[APPROVE_LABEL], None)));
    let mut options = CreateAgentOptions::new(SessionId::new("review-owned-child"));
    options.owner_agent = Some(harness.agent.agent.clone());
    let child = harness.dependencies.agents.create(options).await.unwrap();
    harness.controller.set(&child.agent, true).unwrap();
    let mut input = ToolExecutionInput::new(
        CallId::new("review-owned-child-call"),
        EXIT_PLAN_MODE,
        json!({"plan": "# Plan"}),
        AbortSignal::default(),
    );
    input.agent = Some(child.agent.clone());
    input.agent_session = Some(child.agent.session().clone());
    let result = harness.dependencies.tools.execute(input).await;
    assert!(result.is_error());
    assert!(text(&result).contains("owned by another live agent"));
    assert!(provider.seen.lock().is_empty());
    assert!(fold_plan_mode(
        &child.agent.session().events(),
        child.agent.session().events().len()
    ));
    child.dispose().await.unwrap();
    harness.dispose().await;
}

#[tokio::test]
async fn exact_approval_sets_silent_pending_exit_and_carries_request_intent() {
    let harness = Harness::new(true, true, "review-approve").await;
    let provider = harness.register(Response::Answer(answer(&[APPROVE_LABEL], None)));
    let signal = AbortSignal::default();
    let result = harness
        .call("# The plan\n\ndo things", signal.clone(), true)
        .await;
    assert!(!result.is_error(), "{}", text(&result));
    assert_eq!(result.value(), Some(&json!({"approved": true})));
    assert!(text(&result).starts_with("Plan approved"));
    assert!(fold_plan_mode(&harness.events(), harness.events().len()));
    assert_eq!(
        harness.controller.get(&harness.agent.agent).pending,
        Some(false)
    );
    {
        let seen = provider.seen.lock();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert!(Arc::ptr_eq(
            request.agent.as_ref().unwrap(),
            &harness.agent.agent
        ));
        signal.abort();
        assert!(request.signal.as_ref().is_some_and(AbortSignal::is_aborted));
        let question = &request.questions[0];
        assert_eq!(question.detail.as_deref(), Some("# The plan\n\ndo things"));
        assert_eq!(
            question
                .options
                .as_ref()
                .unwrap()
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            [APPROVE_LABEL, KEEP_PLANNING_LABEL]
        );
        assert_eq!(question.intent.as_ref().unwrap().kind, "plan-review");
        assert_eq!(question.intent.as_ref().unwrap().approve, APPROVE_LABEL);
    }

    let assembly = harness
        .dependencies
        .system_prompt
        .assemble(seekdeep_agent::assemble_context_for(
            &harness.agent.agent,
            None,
        ))
        .await
        .unwrap();
    assert!(
        assembly
            .tools
            .iter()
            .any(|tool| tool.name == EXIT_PLAN_MODE)
    );
    assert_eq!(
        assembly
            .sections
            .iter()
            .find(|section| section.name == "plan:policy")
            .map(|section| section.text.as_str()),
        Some("")
    );
    harness.boundary().await;
    assert!(!fold_plan_mode(&harness.events(), harness.events().len()));
    assert!(notices(&harness.events()).is_empty());
    harness.dispose().await;
}

#[tokio::test]
async fn code_mode_review_carries_exact_plan_and_logs_nested_dispatch() {
    let harness = Harness::new(true, true, "review-code-mode").await;
    let plan = "# Code Mode plan\n\nUse the existing seam.";
    let provider = harness.register(Response::Answer(answer(&[APPROVE_LABEL], None)));
    let runtime = Arc::new(CodeRuntime::new(Arc::new(ExitCodeBackend {
        plan: plan.to_owned(),
    })));
    runtime.provide(&harness.context).unwrap();
    harness
        .dependencies
        .tools
        .present_as(harness.agent.agent.context(), ToolPresentationMode::Code)
        .unwrap();
    let mut input = ToolExecutionInput::new(
        CallId::new("review-code-mode-call"),
        RUN_CODE_NAME,
        json!({
            "code": "return await tools.exit_plan_mode({ plan: '# Code Mode plan' })",
            "description": "Submit the plan for review"
        }),
        AbortSignal::default(),
    );
    input.agent = Some(harness.agent.agent.clone());
    input.agent_session = Some(harness.agent.agent.session().clone());
    let result = harness.dependencies.tools.execute(input).await;
    assert!(!result.is_error(), "{}", text(&result));
    assert_eq!(
        provider.seen.lock()[0].questions[0].detail.as_deref(),
        Some(plan)
    );
    let dispatch = harness
        .events()
        .into_iter()
        .find(|event| event.event_type == "tool/code-dispatch")
        .expect("nested dispatch");
    assert_eq!(dispatch.data["name"], EXIT_PLAN_MODE);
    assert_eq!(dispatch.data["arguments"]["plan"], plan);
    assert_eq!(dispatch.data["isError"], false);
    assert_eq!(
        harness.controller.get(&harness.agent.agent).pending,
        Some(false)
    );
    harness.dispose().await;
}

#[tokio::test]
async fn every_non_exact_consent_stays_in_plan_and_returns_corrective_feedback() {
    let cases = [
        (
            vec![KEEP_PLANNING_LABEL],
            Some("consider the resume path"),
            "their feedback: consider the resume path",
        ),
        (
            vec![KEEP_PLANNING_LABEL],
            None,
            "revise the plan and present it again",
        ),
        (
            Vec::new(),
            Some("add tests first"),
            "their feedback: add tests first",
        ),
        (
            vec![APPROVE_LABEL, KEEP_PLANNING_LABEL],
            None,
            "revise the plan and present it again",
        ),
        (
            vec![APPROVE_LABEL],
            Some("change the tests"),
            "their feedback: change the tests",
        ),
    ];
    for (index, (selected, custom, fragment)) in cases.into_iter().enumerate() {
        let harness = Harness::new(true, true, &format!("review-reject-{index}")).await;
        let answer = AskUserQuestionAnswer {
            answers: vec![AskUserQuestionAnswerItem {
                id: "plan-review".to_owned(),
                selected: selected.into_iter().map(str::to_owned).collect(),
                custom: custom.map(str::to_owned),
            }],
        };
        harness.register(Response::Answer(answer));
        let result = harness.call("# Plan", AbortSignal::default(), true).await;
        assert!(result.is_error());
        assert!(
            text(&result).contains(fragment),
            "case {index}: {}",
            text(&result)
        );
        assert!(fold_plan_mode(&harness.events(), harness.events().len()));
        harness.dispose().await;
    }

    for (index, answers) in [
        Vec::new(),
        vec![
            AskUserQuestionAnswerItem {
                id: "plan-review".to_owned(),
                selected: vec![APPROVE_LABEL.to_owned()],
                custom: None,
            },
            AskUserQuestionAnswerItem {
                id: "plan-review".to_owned(),
                selected: vec![KEEP_PLANNING_LABEL.to_owned()],
                custom: None,
            },
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let harness = Harness::new(true, true, &format!("review-items-{index}")).await;
        harness.register(Response::Answer(AskUserQuestionAnswer { answers }));
        let result = harness.call("# Plan", AbortSignal::default(), true).await;
        assert!(result.is_error());
        assert!(text(&result).contains("revise the plan"));
        harness.dispose().await;
    }
}

#[tokio::test]
async fn review_errors_and_hmr_while_awaiting_preserve_plan_mode() {
    for (index, response, fragment) in [
        (
            0,
            Response::Error {
                message: "the user cancelled ask_user_question".to_owned(),
                code: Some("ASK_CANCELLED".to_owned()),
            },
            "dismissed the plan review to speak instead",
        ),
        (
            1,
            Response::Error {
                message: "ask_user_question was aborted before the user answered".to_owned(),
                code: Some("ASK_ABORTED".to_owned()),
            },
            "ask_user_question was aborted before the user answered",
        ),
        (
            2,
            Response::Error {
                message: "review aborted".to_owned(),
                code: None,
            },
            "review aborted",
        ),
    ] {
        let harness = Harness::new(true, true, &format!("review-error-{index}")).await;
        harness.register(response);
        let result = harness.call("# Plan", AbortSignal::default(), true).await;
        assert!(result.is_error());
        assert!(text(&result).contains(fragment));
        assert!(fold_plan_mode(&harness.events(), harness.events().len()));
        harness.dispose().await;
    }

    let harness = Harness::new(true, true, "review-hmr").await;
    let (send, receive) = oneshot::channel();
    let provider = harness.register(Response::Pending(receive));
    let tools = harness.dependencies.tools.clone();
    let agent = harness.agent.agent.clone();
    let pending = tokio::spawn(async move {
        let mut input = ToolExecutionInput::new(
            CallId::new("review-hmr-call"),
            EXIT_PLAN_MODE,
            json!({"plan": "# Plan"}),
            AbortSignal::default(),
        );
        input.agent = Some(agent.clone());
        input.agent_session = Some(agent.session().clone());
        tools.execute(input).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while provider.seen.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("review entered");
    harness.plan_fiber.dispose().await.unwrap();
    assert!(send.send(answer(&[APPROVE_LABEL], None)).is_ok());
    let result = pending.await.unwrap();
    assert!(result.is_error());
    assert!(text(&result).contains("service was reloaded"));
    assert!(fold_plan_mode(&harness.events(), harness.events().len()));
    harness.agent.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}

#[test]
fn replay_safe_call_and_result_presenters_match_generic_review_cards() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let harness = Harness::new(false, false, "review-present").await;
        let definition = harness
            .dependencies
            .tools
            .get(EXIT_PLAN_MODE, None)
            .unwrap();
        assert_eq!(
            definition.present_call.as_ref().unwrap()(
                &json!({"plan": "## Fix the flake\n\nsteps"})
            ),
            Some(ToolCallView::Generic(GenericCallView {
                title: "Fix the flake".to_owned(),
                kind: Some(ToolCallKind::Other),
                raw_input: None,
                content: Some(vec![ContentBlock::Text {
                    text: "## Fix the flake\n\nsteps".to_owned(),
                }]),
                locations: None,
            }))
        );
        assert_eq!(
            definition.present_result.as_ref().unwrap()(
                &json!({"plan": "# P"}),
                &ToolResult {
                    content: vec![ContentBlock::Text {
                        text: "ok".to_owned(),
                    }],
                    is_error: false,
                    meta: None,
                },
            ),
            Some(ToolResultView::Generic(GenericResultView {
                title: Some("Plan review".to_owned()),
                content: Some(vec![ContentBlock::Text {
                    text: "ok".to_owned(),
                }]),
            }))
        );
        harness.dispose().await;
    });
}
