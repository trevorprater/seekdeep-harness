//! Behavioral mirror of the fixed Ralph tool source suite.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SubagentCapabilities, SubagentProvider, SubagentRun,
    SubagentRuntime,
};
use seekdeep_system_prompt::{AssembleContext, SystemPrompt, SystemPromptConfig};
use seekdeep_tool_ralph::{Config, apply};
use seekdeep_tools::{
    GenericCallView, GenericResultView, ToolCallView, ToolExecutionInput, ToolExecutionResult,
    ToolResult, ToolResultView, ToolRuntime, ToolRuntimeConfig,
};
use seekdeep_workflow::{
    WorkflowEngine, WorkflowEngineService, WorkflowMeta, WorkflowResult, WorkflowRun,
    WorkflowRunId, WorkflowStartRequest, WorkflowStopReason,
};
use serde_json::{Value, json};
use tokio::{sync::oneshot, task::JoinHandle};

const TOOL_NAME: &str = "ralph";

fn continue_report() -> Value {
    json!({
        "status": "continue",
        "summary": "Implemented the first slice.",
        "evidence": ["Focused tests pass."],
        "nextSteps": ["Implement the second slice."],
        "blocker": ""
    })
}

fn complete_report() -> Value {
    json!({
        "status": "complete",
        "summary": "The objective is complete.",
        "evidence": ["All required gates pass."],
        "nextSteps": [],
        "blocker": ""
    })
}

fn blocked_report() -> Value {
    json!({
        "status": "blocked",
        "summary": "No local work can progress.",
        "evidence": ["The required remote service is unavailable."],
        "nextSteps": ["Retry after service recovery."],
        "blocker": "The required remote service is unavailable."
    })
}

struct StubRun {
    id: WorkflowRunId,
    meta: WorkflowMeta,
    receiver: Mutex<Option<oneshot::Receiver<WorkflowResult>>>,
    settled: Mutex<Option<WorkflowResult>>,
    sender: Mutex<Option<oneshot::Sender<WorkflowResult>>>,
    cancels: Arc<Mutex<Vec<String>>>,
    disposed: Arc<AtomicUsize>,
}

impl StubRun {
    fn new(
        id: WorkflowRunId,
        meta: WorkflowMeta,
        cancels: Arc<Mutex<Vec<String>>>,
        disposed: Arc<AtomicUsize>,
    ) -> Arc<Self> {
        let (sender, receiver) = oneshot::channel();
        Arc::new(Self {
            id,
            meta,
            receiver: Mutex::new(Some(receiver)),
            settled: Mutex::new(None),
            sender: Mutex::new(Some(sender)),
            cancels,
            disposed,
        })
    }

    fn settle(&self, result: WorkflowResult) {
        *self.settled.lock() = Some(result.clone());
        if let Some(sender) = self.sender.lock().take() {
            let _ = sender.send(result);
        }
    }
}

impl WorkflowRun for StubRun {
    fn id(&self) -> &WorkflowRunId {
        &self.id
    }

    fn meta(&self) -> &WorkflowMeta {
        &self.meta
    }

    fn result(&self) -> BoxFuture<'static, WorkflowResult> {
        if let Some(settled) = self.settled.lock().clone() {
            return futures::future::ready(settled).boxed();
        }
        let receiver = self
            .receiver
            .lock()
            .take()
            .expect("StubRun::result is requested only once before settlement");
        async move { receiver.await.expect("test settles every started run") }.boxed()
    }

    fn cancel(&self, reason: Option<&str>) {
        let reason = reason.unwrap_or("cancelled").to_owned();
        self.cancels.lock().push(reason.clone());
        self.settle(WorkflowResult {
            value: Value::Null,
            stop_reason: WorkflowStopReason::Cancelled,
            error: Some(reason),
            agents_started: 0,
        });
    }

    fn dispose(&self) -> BoxFuture<'static, ()> {
        let disposed = Arc::clone(&self.disposed);
        async move {
            disposed.fetch_add(1, Ordering::SeqCst);
        }
        .boxed()
    }
}

#[derive(Default)]
struct StubEngine {
    requests: Mutex<Vec<WorkflowStartRequest>>,
    runs: Mutex<Vec<Arc<StubRun>>>,
    cancels: Arc<Mutex<Vec<String>>>,
    disposed: Arc<AtomicUsize>,
    start_error: Mutex<Option<String>>,
    abort_on_start: AtomicBool,
}

impl StubEngine {
    fn request(&self, index: usize) -> WorkflowStartRequest {
        self.requests.lock()[index].clone()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().len()
    }
}

impl WorkflowEngine for StubEngine {
    fn start(&self, request: WorkflowStartRequest) -> anyhow::Result<Arc<dyn WorkflowRun>> {
        if let Some(message) = self.start_error.lock().clone() {
            anyhow::bail!(message);
        }
        let index = self.requests.lock().len() + 1;
        let run = StubRun::new(
            WorkflowRunId::new(format!("ralph-{index}")),
            request.meta.clone(),
            Arc::clone(&self.cancels),
            Arc::clone(&self.disposed),
        );
        self.requests.lock().push(request.clone());
        self.runs.lock().push(Arc::clone(&run));
        if self.abort_on_start.load(Ordering::SeqCst) {
            request.signal.as_ref().expect("call signal").abort();
        }
        Ok(run)
    }
}

struct StubProvider {
    capabilities: SubagentCapabilities,
    inherits_parent_context: bool,
}

impl StubProvider {
    fn fresh() -> Arc<Self> {
        Arc::new(Self {
            capabilities: SubagentCapabilities {
                output_schema: true,
                depth_limit: true,
                tool_filter: true,
                persona: true,
            },
            inherits_parent_context: false,
        })
    }
}

#[async_trait]
impl SubagentProvider for StubProvider {
    fn name(&self) -> &'static str {
        "fresh"
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        self.inherits_parent_context
    }

    async fn start(
        &self,
        _request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        anyhow::bail!("StubProvider::start must not be reached behind StubEngine")
    }
}

struct Harness {
    context: Context,
    prompt: Arc<SystemPrompt>,
    tools: Arc<ToolRuntime>,
    engine: Arc<StubEngine>,
    fiber: Arc<Fiber>,
}

fn setup(mut config: Config, provider: Option<Arc<StubProvider>>) -> Harness {
    if config.subagent_provider == Config::default().subagent_provider {
        "fresh".clone_into(&mut config.subagent_provider);
    }
    let context = Context::new();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).expect("prompt");
    prompt.provide(&context).expect("provide prompt");
    let tools =
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).expect("tools");
    let subagents = SubagentRuntime::install(&context).expect("subagents");
    if let Some(provider) = provider {
        subagents.register_provider(provider).expect("provider");
    }
    let engine = Arc::new(StubEngine::default());
    WorkflowEngineService::new(engine.clone())
        .provide(&context)
        .expect("workflow engine");
    let fiber = Fiber::active_child("tool-ralph-test");
    let child = context.with_fiber(fiber.clone());
    apply(&child, &config).expect("apply Ralph");
    Harness {
        context,
        prompt,
        tools,
        engine,
        fiber,
    }
}

fn parent(id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let session = Session::create(&id, None, None).expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ))
}

fn input(
    call: &str,
    arguments: Value,
    agent: Option<Arc<Agent>>,
    signal: AbortSignal,
) -> ToolExecutionInput {
    let mut input = ToolExecutionInput::new(CallId::new(call), TOOL_NAME, arguments, signal);
    input.agent.clone_from(&agent);
    input.agent_session = agent.map(|agent| agent.session().clone());
    input
}

fn text(result: &ToolExecutionResult) -> &str {
    match result.content().first() {
        Some(ContentBlock::Text { text }) => text,
        other => panic!("expected text content, got {other:?}"),
    }
}

async fn started_call(
    harness: &Harness,
    arguments: Value,
    agent: Arc<Agent>,
    signal: AbortSignal,
) -> (JoinHandle<ToolExecutionResult>, Arc<StubRun>) {
    let next = harness.engine.request_count() + 1;
    let tools = Arc::clone(&harness.tools);
    let task = tokio::spawn(async move {
        tools
            .execute(input("ralph-call", arguments, Some(agent), signal))
            .await
    });
    let run = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(run) = harness.engine.runs.lock().get(next - 1).cloned() {
                return run;
            }
            assert!(!task.is_finished(), "tool finished before starting a run");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("workflow run started");
    (task, run)
}

async fn completed_call(
    harness: &Harness,
    arguments: Value,
    agent: Arc<Agent>,
    value: Value,
    agents_started: u64,
) -> ToolExecutionResult {
    let (task, run) = started_call(harness, arguments, agent, AbortSignal::default()).await;
    run.settle(WorkflowResult {
        value,
        stop_reason: WorkflowStopReason::Completed,
        error: None,
        agents_started,
    });
    task.await.expect("tool task")
}

#[tokio::test]
async fn starts_the_fixed_workflow_and_renders_a_bounded_completion() {
    let harness = setup(
        Config {
            max_rounds: 9,
            max_handoff_chars: 9000,
            ..Config::default()
        },
        Some(StubProvider::fresh()),
    );
    let owner = parent("caller");
    let result = completed_call(
        &harness,
        json!({"objective": "  Finish the migration.  ", "maxRounds": 4}),
        owner.clone(),
        json!({"status": "complete", "roundsStarted": 1, "report": complete_report()}),
        1,
    )
    .await;
    assert!(!result.is_error(), "{}", text(&result));
    let request = harness.engine.request(0);
    assert_eq!(request.meta.name, "ralph-loop");
    assert_eq!(
        request.args,
        Some(json!({
            "objective": "Finish the migration.",
            "maxRounds": 4,
            "maxHandoffChars": 9000
        }))
    );
    assert_eq!(request.subagent_provider.as_deref(), Some("fresh"));
    assert_eq!(request.max_total_agents, Some(4));
    assert!(Arc::ptr_eq(&request.parent, &owner));
    assert!(request.script.contains("status: 'budget-limited'"));
    assert_eq!(
        result.value(),
        Some(&json!({
            "runId": "ralph-1",
            "agentsStarted": 1,
            "result": {
                "status": "complete",
                "roundsStarted": 1,
                "report": complete_report()
            }
        }))
    );
    assert!(text(&result).contains("Ralph worker reported completion after 1 round."));
    assert!(text(&result).contains("All required gates pass."));
    assert_eq!(harness.engine.disposed.load(Ordering::SeqCst), 1);

    let bounded = setup(
        Config {
            max_result_chars: 160,
            ..Config::default()
        },
        Some(StubProvider::fresh()),
    );
    let mut report = complete_report();
    report["evidence"] = json!(["x".repeat(500)]);
    let result = completed_call(
        &bounded,
        json!({"objective": "Ship it."}),
        parent("bounded"),
        json!({"status": "complete", "roundsStarted": 1, "report": report}),
        1,
    )
    .await;
    assert_eq!(text(&result).chars().count(), 160);
    assert!(text(&result).ends_with("… [truncated]"));

    let tiny = setup(
        Config {
            max_result_chars: 5,
            ..Config::default()
        },
        Some(StubProvider::fresh()),
    );
    let result = completed_call(
        &tiny,
        json!({"objective": "Ship it."}),
        parent("tiny"),
        json!({"status": "complete", "roundsStarted": 1, "report": complete_report()}),
        1,
    )
    .await;
    assert_eq!(text(&result), "\n… [t");
}

#[tokio::test]
async fn blocked_budget_limited_and_round_failed_outcomes_match_the_source() {
    let harness = setup(
        Config {
            max_rounds: 2,
            ..Config::default()
        },
        Some(StubProvider::fresh()),
    );
    let owner = parent("outcomes");
    let blocked = completed_call(
        &harness,
        json!({"objective": "Ship it."}),
        owner.clone(),
        json!({"status": "blocked", "roundsStarted": 2, "report": blocked_report()}),
        2,
    )
    .await;
    assert!(!blocked.is_error());
    assert!(text(&blocked).contains("Ralph worker reported a blocker after 2 rounds."));

    let limited = completed_call(
        &harness,
        json!({"objective": "Ship it."}),
        owner.clone(),
        json!({"status": "budget-limited", "roundsStarted": 2, "report": continue_report()}),
        2,
    )
    .await;
    assert!(!limited.is_error());
    assert!(
        text(&limited)
            .contains("Ralph reached its 2 rounds limit; the worker reported work remaining.")
    );

    let first = completed_call(
        &harness,
        json!({"objective": "Ship it.", "maxRounds": 2}),
        owner.clone(),
        json!({"status": "round-failed", "roundsStarted": 1, "lastReport": null}),
        1,
    )
    .await;
    assert!(first.is_error());
    assert!(text(&first).contains("Ralph round 1 child failed"));
    assert!(text(&first).contains("No previous handoff was available."));

    let later = completed_call(
        &harness,
        json!({"objective": "Ship it.", "maxRounds": 2}),
        owner,
        json!({
            "status": "round-failed",
            "roundsStarted": 2,
            "lastReport": continue_report()
        }),
        2,
    )
    .await;
    assert!(later.is_error());
    assert!(text(&later).contains("Ralph round 2 child failed"));
    assert!(text(&later).contains("Implemented the first slice."));
    assert_eq!(harness.engine.disposed.load(Ordering::SeqCst), 4);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one lifecycle test mirrors every source settlement path
async fn workflow_failures_cancellation_and_start_failures_are_contained_and_disposed() {
    let harness = setup(Config::default(), Some(StubProvider::fresh()));
    let owner = parent("failures");
    for (stop_reason, error, fragment) in [
        (
            WorkflowStopReason::Error,
            Some("child report malformed"),
            "Ralph workflow failed: child report malformed",
        ),
        (
            WorkflowStopReason::Error,
            None,
            "Ralph workflow failed: unknown error",
        ),
        (
            WorkflowStopReason::Cancelled,
            Some("user stopped"),
            "Ralph workflow was cancelled (user stopped)",
        ),
        (
            WorkflowStopReason::Cancelled,
            None,
            "Ralph workflow was cancelled",
        ),
    ] {
        let (task, run) = started_call(
            &harness,
            json!({"objective": "Work."}),
            owner.clone(),
            AbortSignal::default(),
        )
        .await;
        run.settle(WorkflowResult {
            value: Value::Null,
            stop_reason,
            error: error.map(str::to_owned),
            agents_started: 0,
        });
        let result = task.await.expect("tool task");
        assert!(result.is_error());
        assert!(text(&result).contains(fragment));
    }
    assert_eq!(harness.engine.disposed.load(Ordering::SeqCst), 4);

    let signal = AbortSignal::default();
    let (task, _run) = started_call(
        &harness,
        json!({"objective": "Work."}),
        owner.clone(),
        signal.clone(),
    )
    .await;
    signal.abort();
    let result = task.await.expect("tool task");
    assert!(result.is_error());
    assert_eq!(
        harness.engine.cancels.lock().as_slice(),
        ["parent step aborted"]
    );

    let before = harness.engine.request_count();
    let aborted = AbortSignal::default();
    aborted.abort();
    let skipped = harness
        .tools
        .execute(input(
            "already-aborted",
            json!({"objective": "Work."}),
            Some(owner.clone()),
            aborted,
        ))
        .await;
    assert_eq!(
        skipped
            .error()
            .and_then(|error| error.info.as_ref())
            .map(|info| info.code.as_str()),
        Some(seekdeep_tools::TOOL_ABORTED_BEFORE_DISPATCH)
    );
    assert_eq!(harness.engine.request_count(), before);

    harness.engine.abort_on_start.store(true, Ordering::SeqCst);
    let started_aborted = harness
        .tools
        .execute(input(
            "abort-during-start",
            json!({"objective": "Work."}),
            Some(owner.clone()),
            AbortSignal::default(),
        ))
        .await;
    assert!(started_aborted.is_error());
    assert_eq!(harness.engine.cancels.lock().len(), 2);
    assert_eq!(harness.engine.disposed.load(Ordering::SeqCst), 6);

    harness.engine.abort_on_start.store(false, Ordering::SeqCst);
    *harness.engine.start_error.lock() = Some("engine refused fixed script".to_owned());
    let start_error = harness
        .tools
        .execute(input(
            "start-error",
            json!({"objective": "Work."}),
            Some(owner),
            AbortSignal::default(),
        ))
        .await;
    assert!(start_error.is_error());
    assert!(text(&start_error).contains("engine refused fixed script"));
    assert_eq!(harness.engine.disposed.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn rejects_invalid_authority_arguments_routes_and_direct_config_before_start() {
    let harness = setup(
        Config {
            max_rounds: 3,
            ..Config::default()
        },
        Some(StubProvider::fresh()),
    );
    let owner = parent("validation");
    for (call, arguments, agent) in [
        ("no-agent", json!({"objective": "Work."}), None),
        ("empty", json!({"objective": "   "}), Some(owner.clone())),
        (
            "zero-rounds",
            json!({"objective": "Work.", "maxRounds": 0}),
            Some(owner.clone()),
        ),
        (
            "fractional-rounds",
            json!({"objective": "Work.", "maxRounds": 1.5}),
            Some(owner.clone()),
        ),
        (
            "over-ceiling",
            json!({"objective": "Work.", "maxRounds": 4}),
            Some(owner.clone()),
        ),
        ("missing-objective", json!({}), Some(owner.clone())),
    ] {
        let result = harness
            .tools
            .execute(input(call, arguments, agent, AbortSignal::default()))
            .await;
        assert!(result.is_error(), "{call} unexpectedly succeeded");
    }
    assert_eq!(harness.engine.request_count(), 0);

    let missing = setup(Config::default(), None);
    let result = missing
        .tools
        .execute(input(
            "missing-provider",
            json!({"objective": "Work."}),
            Some(parent("missing")),
            AbortSignal::default(),
        ))
        .await;
    assert!(text(&result).contains("is not registered"));

    let mut unstructured = StubProvider::fresh();
    Arc::get_mut(&mut unstructured)
        .expect("unique provider")
        .capabilities
        .output_schema = false;
    let harness = setup(Config::default(), Some(unstructured));
    let result = harness
        .tools
        .execute(input(
            "unstructured",
            json!({"objective": "Work."}),
            Some(parent("unstructured")),
            AbortSignal::default(),
        ))
        .await;
    assert!(text(&result).contains("does not support structured output"));

    let mut inherited = StubProvider::fresh();
    Arc::get_mut(&mut inherited)
        .expect("unique provider")
        .inherits_parent_context = true;
    let harness = setup(Config::default(), Some(inherited));
    let result = harness
        .tools
        .execute(input(
            "inherited",
            json!({"objective": "Work."}),
            Some(parent("inherited")),
            AbortSignal::default(),
        ))
        .await;
    assert!(text(&result).contains("inherits parent context"));

    for config in [
        Config {
            subagent_provider: " ".to_owned(),
            ..Config::default()
        },
        Config {
            max_rounds: 0,
            ..Config::default()
        },
        Config {
            max_handoff_chars: 0,
            ..Config::default()
        },
        Config {
            max_result_chars: 0,
            ..Config::default()
        },
    ] {
        let error = apply(&Context::new(), &config).unwrap_err().to_string();
        assert!(error.contains("non-empty normalized") || error.contains("positive safe integer"));
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table mirrors the source malformed-result inventory
async fn rejects_every_malformed_fixed_workflow_terminal_value_and_still_disposes() {
    let cases = vec![
        (Value::Null, 256, 16_384, "malformed terminal result"),
        (
            json!({"status": "complete", "roundsStarted": 0, "report": complete_report()}),
            256,
            16_384,
            "malformed terminal result",
        ),
        (
            json!({"status": "complete", "roundsStarted": 3, "report": complete_report()}),
            2,
            16_384,
            "malformed terminal result",
        ),
        (
            json!({"status": "mystery", "roundsStarted": 1, "report": complete_report()}),
            256,
            16_384,
            "unknown terminal status",
        ),
        (
            json!({"status": "budget-limited", "roundsStarted": 1, "report": continue_report()}),
            2,
            16_384,
            "before the round limit",
        ),
        (
            json!({"status": "complete", "roundsStarted": 1, "report": null}),
            256,
            16_384,
            "malformed round report",
        ),
        (
            json!({"status": "complete", "roundsStarted": 1, "report": complete_report(), "extra": true}),
            256,
            16_384,
            "malformed terminal result",
        ),
        (
            json!({"status": "blocked", "roundsStarted": 1, "report": blocked_report(), "extra": true}),
            256,
            16_384,
            "malformed terminal result",
        ),
        (
            json!({"status": "budget-limited", "roundsStarted": 1, "report": continue_report(), "extra": true}),
            1,
            16_384,
            "malformed terminal result",
        ),
        (
            json!({"status": "complete", "roundsStarted": 1, "report": {
                "status": "continue", "summary": "done", "evidence": ["e"],
                "nextSteps": [], "blocker": ""
            }}),
            256,
            16_384,
            "malformed round report",
        ),
        (
            json!({"status": "budget-limited", "roundsStarted": 1, "report": {
                "status": "continue", "summary": "work", "evidence": ["e"],
                "nextSteps": [], "blocker": ""
            }}),
            1,
            16_384,
            "invalid continuing report",
        ),
        (
            json!({"status": "complete", "roundsStarted": 1, "report": {
                "status": "complete", "summary": "done", "evidence": [],
                "nextSteps": [], "blocker": ""
            }}),
            256,
            16_384,
            "invalid completion report",
        ),
        (
            json!({"status": "blocked", "roundsStarted": 1, "report": {
                "status": "blocked", "summary": "blocked", "evidence": ["e"],
                "nextSteps": ["wait"], "blocker": ""
            }}),
            256,
            16_384,
            "invalid blocked report",
        ),
        (
            json!({"status": "complete", "roundsStarted": 1, "report": {
                "status": "complete", "summary": "x".repeat(500), "evidence": ["e"],
                "nextSteps": [], "blocker": ""
            }}),
            256,
            100,
            "oversized handoff",
        ),
        (
            json!({"status": "round-failed", "roundsStarted": 1}),
            256,
            16_384,
            "malformed terminal result",
        ),
        (
            json!({"status": "round-failed", "roundsStarted": 1, "lastReport": continue_report()}),
            256,
            16_384,
            "invalid first-round failure",
        ),
        (
            json!({"status": "round-failed", "roundsStarted": 2, "lastReport": null}),
            2,
            16_384,
            "without its last handoff",
        ),
        (
            json!({"status": "round-failed", "roundsStarted": 2, "lastReport": {
                "status": "continue", "summary": "work", "evidence": ["e"],
                "nextSteps": [], "blocker": ""
            }}),
            2,
            16_384,
            "invalid continuing report",
        ),
    ];

    for (index, (value, max_rounds, max_handoff_chars, fragment)) in cases.into_iter().enumerate() {
        let harness = setup(
            Config {
                max_rounds,
                max_handoff_chars,
                ..Config::default()
            },
            Some(StubProvider::fresh()),
        );
        let result = completed_call(
            &harness,
            json!({"objective": "Work.", "maxRounds": max_rounds}),
            parent(&format!("malformed-{index}")),
            value,
            1,
        )
        .await;
        assert!(result.is_error(), "case {index} unexpectedly succeeded");
        assert!(
            text(&result).contains(fragment),
            "case {index}: expected {fragment:?} in {:?}",
            text(&result)
        );
        assert_eq!(
            harness.engine.disposed.load(Ordering::SeqCst),
            1,
            "case {index} leaked its run"
        );
    }
}

#[tokio::test]
async fn registers_scoped_guidance_presentation_schema_and_hmr_cleanup() {
    let harness = setup(Config::default(), Some(StubProvider::fresh()));
    let assembly = harness
        .prompt
        .assemble(AssembleContext::default())
        .await
        .expect("assemble");
    let section = assembly
        .sections
        .iter()
        .find(|section| section.name == "tool:ralph")
        .expect("Ralph prompt section");
    assert!(
        section
            .text
            .contains("ONLY when the direct human explicitly asks")
    );
    assert!(
        section
            .text
            .contains("worker reports, not independent evaluation")
    );

    let definition = harness.tools.get(TOOL_NAME, None).expect("Ralph tool");
    assert!(definition.description.contains("worker reports completion"));
    assert_eq!(definition.parameters["required"], json!(["objective"]));
    assert_eq!(
        definition.parameters["properties"]["objective"]["type"],
        "string"
    );
    assert_eq!(
        definition.parameters["properties"]["maxRounds"]["type"],
        "number"
    );
    assert_eq!(
        definition.present_call.as_ref().expect("call presenter")(
            &json!({"objective": "Finish it."})
        ),
        Some(ToolCallView::Generic(GenericCallView {
            title: "ralph".to_owned(),
            kind: None,
            raw_input: Some(json!("Finish it.")),
            content: None,
            locations: None,
        }))
    );
    assert!(
        definition.present_call.as_ref().expect("call presenter")(&json!({"nope": true})).is_none()
    );
    assert_eq!(
        definition
            .present_result
            .as_ref()
            .expect("result presenter")(
            &json!({"objective": "Finish it."}),
            &ToolResult {
                content: Vec::new(),
                is_error: false,
                meta: None,
            },
        ),
        Some(ToolResultView::Generic(GenericResultView::default()))
    );

    harness.fiber.dispose().await.expect("dispose Ralph fiber");
    assert!(harness.tools.get(TOOL_NAME, None).is_none());
    let assembly = harness
        .prompt
        .assemble(AssembleContext::default())
        .await
        .expect("assemble after dispose");
    assert!(
        assembly
            .sections
            .iter()
            .all(|section| section.name != "tool:ralph")
    );
    assert_eq!(seekdeep_tool_ralph::NAME, "tool-ralph");
    assert_eq!(
        seekdeep_tool_ralph::INJECT,
        ["tools", "workflowEngine", "subagents", "systemPrompt"]
    );
    let _ = &harness.context;
}
