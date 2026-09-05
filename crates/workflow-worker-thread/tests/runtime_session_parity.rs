//! Worker-session semantics over the Rust-owned JavaScript execution core.

use std::{collections::VecDeque, sync::Arc};

use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_workflow::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowAgentOutcome, WorkflowMeta, WorkflowResult,
    WorkflowStopReason,
};
use seekdeep_workflow_worker_thread::{
    ChildHandle, ChildPort, ChildResult, ChildStartRequest, ExecutionObserver, WorkerLimits,
    WorkflowExecution,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

#[derive(Default)]
struct Observed {
    phases: Mutex<Vec<String>>,
    logs: Mutex<Vec<String>>,
    starts: Mutex<Vec<WorkflowAgentInfo>>,
    ends: Mutex<Vec<WorkflowAgentEndInfo>>,
}

impl ExecutionObserver for Observed {
    fn phase(&self, title: &str) {
        self.phases.lock().push(title.to_owned());
    }

    fn log(&self, message: &str) {
        self.logs.lock().push(message.to_owned());
    }

    fn agent_start(&self, info: &WorkflowAgentInfo) {
        self.starts.lock().push(info.clone());
    }

    fn agent_end(&self, info: &WorkflowAgentEndInfo) {
        self.ends.lock().push(info.clone());
    }
}

type ChildSettlement = Result<ChildResult, String>;

struct ChildState {
    id: String,
    result: futures::future::Shared<BoxFuture<'static, ChildSettlement>>,
    disposed: std::sync::atomic::AtomicBool,
    dispose_count: std::sync::atomic::AtomicUsize,
}

impl ChildState {
    fn immediate(id: &str, result: ChildSettlement) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_owned(),
            result: futures::future::ready(result).boxed().shared(),
            disposed: std::sync::atomic::AtomicBool::new(false),
            dispose_count: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn pending(id: &str) -> (Arc<Self>, oneshot::Sender<ChildSettlement>) {
        let (sender, receiver) = oneshot::channel();
        let result = async move {
            receiver
                .await
                .unwrap_or_else(|_| Err("test settlement sender dropped".to_owned()))
        }
        .boxed()
        .shared();
        (
            Arc::new(Self {
                id: id.to_owned(),
                result,
                disposed: std::sync::atomic::AtomicBool::new(false),
                dispose_count: std::sync::atomic::AtomicUsize::new(0),
            }),
            sender,
        )
    }

    fn dispose_once(&self) {
        if !self
            .disposed
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.dispose_count
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
}

struct TestChildHandle(Arc<ChildState>);

impl Drop for TestChildHandle {
    fn drop(&mut self) {
        self.0.dispose_once();
    }
}

impl ChildHandle for TestChildHandle {
    fn id(&self) -> &str {
        &self.0.id
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<ChildResult>> {
        let result = self.0.result.clone();
        async move { result.await.map_err(|error| anyhow::anyhow!(error)) }.boxed()
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        let state = self.0.clone();
        async move {
            state.dispose_once();
            Ok(())
        }
        .boxed()
    }
}

enum StartBehavior {
    Child(Arc<ChildState>),
    Refuse(String),
    Pending(oneshot::Receiver<Result<Arc<ChildState>, String>>),
}

#[derive(Default)]
struct ScriptedChildren {
    behaviors: Mutex<VecDeque<StartBehavior>>,
    starts: Mutex<Vec<ChildStartRequest>>,
    changed: Notify,
}

impl ScriptedChildren {
    fn new(behaviors: impl IntoIterator<Item = StartBehavior>) -> Arc<Self> {
        Arc::new(Self {
            behaviors: Mutex::new(behaviors.into_iter().collect()),
            ..Self::default()
        })
    }

    async fn wait_for_starts(&self, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let notified = self.changed.notified();
                if self.starts.lock().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("child start");
    }
}

impl ChildPort for ScriptedChildren {
    fn start_agent(
        &self,
        request: ChildStartRequest,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn ChildHandle>>> {
        self.starts.lock().push(request);
        self.changed.notify_waiters();
        let behavior = self
            .behaviors
            .lock()
            .pop_front()
            .unwrap_or_else(|| StartBehavior::Refuse("no scripted child".to_owned()));
        async move {
            let state = match behavior {
                StartBehavior::Child(state) => state,
                StartBehavior::Refuse(error) => anyhow::bail!(error),
                StartBehavior::Pending(receiver) => receiver
                    .await
                    .map_err(|_| anyhow::anyhow!("pending start sender dropped"))?
                    .map_err(|error| anyhow::anyhow!(error))?,
            };
            Ok(Arc::new(TestChildHandle(state)) as Arc<dyn ChildHandle>)
        }
        .boxed()
    }
}

fn text_result(text: &str) -> ChildResult {
    ChildResult {
        output: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        structured: None,
        stop_reason: "completed".to_owned(),
    }
}

fn failed_result(reason: &str) -> ChildResult {
    ChildResult {
        output: Vec::new(),
        structured: None,
        stop_reason: reason.to_owned(),
    }
}

fn structured_result(value: Value) -> ChildResult {
    ChildResult {
        output: Vec::new(),
        structured: Some(value),
        stop_reason: "completed".to_owned(),
    }
}

fn limits() -> WorkerLimits {
    WorkerLimits {
        max_concurrent_agents: 8,
        max_total_agents: 1000,
        max_items_per_call: 4096,
        sync_timeout_ms: 5000,
    }
}

fn meta() -> WorkflowMeta {
    WorkflowMeta {
        name: "test-flow".to_owned(),
        description: "a test workflow".to_owned(),
        when_to_use: None,
        phases: None,
    }
}

fn make_execution(
    body: &str,
    args: Option<Value>,
    limits: WorkerLimits,
    observer: Arc<Observed>,
    children: Arc<ScriptedChildren>,
) -> Arc<WorkflowExecution> {
    Arc::new(
        WorkflowExecution::new(&meta(), body.to_owned(), args, limits, observer, children)
            .expect("execution"),
    )
}

async fn run(
    body: &str,
    args: Option<Value>,
    children: Arc<ScriptedChildren>,
    observer: Arc<Observed>,
) -> WorkflowResult {
    make_execution(body, args, limits(), observer, children)
        .drive()
        .await
}

#[tokio::test]
async fn runs_pipeline_args_phase_log_and_agent_lifecycles_end_to_end() {
    let children = ScriptedChildren::new([
        StartBehavior::Child(ChildState::immediate(
            "child-0",
            Ok(text_result("answer-0")),
        )),
        StartBehavior::Child(ChildState::immediate(
            "child-1",
            Ok(text_result("answer-1")),
        )),
    ]);
    let observed = Arc::new(Observed::default());
    let result = run(
        r"phase('Scan')
log('starting with ' + args.files.length + ' files')
const answers = await pipeline(args.files, (prev, item) => agent('read ' + item))
return { answers }",
        Some(json!({"files": ["a.rs", "b.rs"]})),
        children.clone(),
        observed.clone(),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.agents_started, 2);
    assert_eq!(result.value, json!({"answers": ["answer-0", "answer-1"]}));
    assert_eq!(observed.phases.lock().as_slice(), ["Scan"]);
    assert_eq!(observed.logs.lock().as_slice(), ["starting with 2 files"]);
    assert_eq!(
        observed
            .starts
            .lock()
            .iter()
            .map(|info| info.child_id.as_str())
            .collect::<Vec<_>>(),
        ["child-0", "child-1"]
    );
    assert!(
        observed
            .ends
            .lock()
            .iter()
            .all(|info| info.outcome == WorkflowAgentOutcome::Completed)
    );
}

#[tokio::test]
async fn forwards_schema_provider_model_and_returns_structured_or_plain_results() {
    let children = ScriptedChildren::new([
        StartBehavior::Child(ChildState::immediate(
            "structured",
            Ok(structured_result(json!({"files": ["x.rs"]}))),
        )),
        StartBehavior::Child(ChildState::immediate("routed", Ok(text_result("route-ok")))),
    ]);
    let result = run(
        r"const found = await agent('list files', { schema: { type: 'object', properties: { files: { type: 'array', items: { type: 'string' } } } }, model: 'deepseek-v4-pro' })
const routed = await agent('route me', { provider: 'openai' })
return { first: found.files[0], routed }",
        None,
        children.clone(),
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.value, json!({"first": "x.rs", "routed": "route-ok"}));
    let starts = children.starts.lock();
    assert_eq!(starts[0].model.as_deref(), Some("deepseek-v4-pro"));
    assert!(starts[0].schema.is_some());
    assert_eq!(starts[1].provider.as_deref(), Some("openai"));
    assert_eq!(starts[1].model, None);
}

#[tokio::test]
async fn maps_child_outcomes_and_infrastructure_failures_exactly() {
    let missing = ScriptedChildren::new([StartBehavior::Child(ChildState::immediate(
        "missing-structured",
        Ok(text_result("prose")),
    ))]);
    let observed = Arc::new(Observed::default());
    let result = run(
        "return await agent('p', { schema: { type: 'object' } })",
        None,
        missing,
        observed.clone(),
    )
    .await;
    assert_eq!(result.value, Value::Null);
    assert_eq!(
        observed.ends.lock()[0].outcome,
        WorkflowAgentOutcome::Failed
    );

    let mixed = ScriptedChildren::new([
        StartBehavior::Child(ChildState::immediate("failed", Ok(failed_result("error")))),
        StartBehavior::Child(ChildState::immediate("ok", Ok(text_result("ok")))),
    ]);
    let result = run(
        "return await parallel([() => agent('one'), () => agent('two')])",
        None,
        mixed,
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.value, json!([null, "ok"]));

    let refused = ScriptedChildren::new([StartBehavior::Refuse("no provider here".to_owned())]);
    let result = run(
        "return await pipeline([1], () => agent('p'))",
        None,
        refused,
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    let error = result.error.unwrap();
    assert!(error.contains("agent() could not start a child"));
    assert!(error.contains("no provider here"));

    let broken = ScriptedChildren::new([StartBehavior::Child(ChildState::immediate(
        "broken",
        Err("backend exploded".to_owned()),
    ))]);
    let observed = Arc::new(Observed::default());
    let result = run("return await agent('p')", None, broken, observed.clone()).await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    assert!(result.error.unwrap().contains("child agent run failed"));
    assert_eq!(
        observed.ends.lock()[0].outcome,
        WorkflowAgentOutcome::Failed
    );

    let caught = ScriptedChildren::new([StartBehavior::Child(ChildState::immediate(
        "caught-broken",
        Err("backend exploded".to_owned()),
    ))]);
    let result = run(
        r"try {
  await agent('p')
  return 'unreachable'
} catch (error) {
  return { name: error.name, code: error.code, fatal: error.fatal }
}",
        None,
        caught,
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(
        result.value,
        json!({"name": "WorkflowError", "code": "AGENT_RESULT", "fatal": true})
    );
}

#[tokio::test]
async fn cancellation_before_drive_and_during_a_child_is_first_reason_wins_and_quiescent() {
    let children = ScriptedChildren::new([]);
    let observed = Arc::new(Observed::default());
    let execution = make_execution(
        "log('ran'); return 123",
        None,
        limits(),
        observed.clone(),
        children,
    );
    execution.cancel("aborted before start");
    execution.cancel("later reason");
    let result = execution.drive().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    let error = result.error.unwrap();
    assert!(error.contains("aborted before start"));
    assert!(!error.contains("later reason"));
    assert!(observed.logs.lock().is_empty());

    let (child, settle) = ChildState::pending("pending-child");
    let children = ScriptedChildren::new([StartBehavior::Child(child.clone())]);
    let observed = Arc::new(Observed::default());
    let execution = make_execution(
        "phase('before'); await agent('x'); phase('after'); return 'done'",
        None,
        limits(),
        observed.clone(),
        children.clone(),
    );
    let task = tokio::spawn({
        let execution = execution.clone();
        async move { execution.drive().await }
    });
    children.wait_for_starts(1).await;
    execution.cancel("stop everything");
    settle.send(Ok(failed_result("aborted"))).unwrap();
    let result = task.await.unwrap();
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert!(result.error.unwrap().contains("stop everything"));
    assert_eq!(observed.phases.lock().as_slice(), ["before"]);
    assert_eq!(
        observed.ends.lock()[0].outcome,
        WorkflowAgentOutcome::Cancelled
    );
    assert_eq!(
        child
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
}

#[tokio::test]
async fn cancellation_rejects_a_queued_slot_without_starting_another_child() {
    let (first, _settle) = ChildState::pending("first");
    let children = ScriptedChildren::new([StartBehavior::Child(first)]);
    let mut constrained = limits();
    constrained.max_concurrent_agents = 1;
    let execution = make_execution(
        "return await parallel([() => agent('a'), () => agent('b')])",
        None,
        constrained,
        Arc::new(Observed::default()),
        children.clone(),
    );
    let task = tokio::spawn({
        let execution = execution.clone();
        async move { execution.drive().await }
    });
    children.wait_for_starts(1).await;
    execution.cancel("raced");
    let result = task.await.unwrap();
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert_eq!(children.starts.lock().len(), 1);
}

#[tokio::test]
async fn cancellation_wins_start_refusal_and_child_result_rejection_races() {
    let (start_sender, start_receiver) = oneshot::channel();
    let children = ScriptedChildren::new([StartBehavior::Pending(start_receiver)]);
    let execution = make_execution(
        "return await agent('pending start')",
        None,
        limits(),
        Arc::new(Observed::default()),
        children.clone(),
    );
    let task = tokio::spawn({
        let execution = execution.clone();
        async move { execution.drive().await }
    });
    children.wait_for_starts(1).await;
    execution.cancel("stopping");
    assert!(
        start_sender
            .send(Err("workflow run cancelled: stopping".to_owned()))
            .is_ok()
    );
    let result = task.await.unwrap();
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert!(result.error.unwrap().contains("stopping"));

    let (child, settle) = ChildState::pending("doomed");
    let children = ScriptedChildren::new([StartBehavior::Child(child)]);
    let observed = Arc::new(Observed::default());
    let execution = make_execution(
        "return await agent('doomed')",
        None,
        limits(),
        observed.clone(),
        children.clone(),
    );
    let task = tokio::spawn({
        let execution = execution.clone();
        async move { execution.drive().await }
    });
    children.wait_for_starts(1).await;
    while observed.starts.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    execution.cancel("user aborted");
    settle
        .send(Err("backend crashed on abort".to_owned()))
        .unwrap();
    let result = task.await.unwrap();
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert_eq!(
        observed.ends.lock()[0].outcome,
        WorkflowAgentOutcome::Cancelled
    );
}

#[tokio::test]
async fn dropped_agent_promises_do_not_hold_the_root_and_are_disposed_once() {
    let (stray, _settle) = ChildState::pending("stray");
    let children = ScriptedChildren::new([StartBehavior::Child(stray.clone())]);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run(
            "agent('stray, never awaited'); return 'done without awaiting'",
            None,
            children,
            Arc::new(Observed::default()),
        ),
    )
    .await
    .expect("root settlement");
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.value, json!("done without awaiting"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while stray
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stray disposed");
    assert_eq!(
        stray
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
}

#[tokio::test]
async fn parked_script_and_pending_start_remain_cancellable() {
    let parked = make_execution(
        "await new Promise(() => {}); return 'unreachable'",
        None,
        limits(),
        Arc::new(Observed::default()),
        ScriptedChildren::new([]),
    );
    let task = tokio::spawn({
        let parked = parked.clone();
        async move { parked.drive().await }
    });
    tokio::task::yield_now().await;
    parked.cancel("parked cancelled");
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("parked settlement")
        .unwrap();
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);

    let (start_sender, start_receiver) = oneshot::channel();
    let children = ScriptedChildren::new([StartBehavior::Pending(start_receiver)]);
    let starting = make_execution(
        "return await agent('pending start')",
        None,
        limits(),
        Arc::new(Observed::default()),
        children.clone(),
    );
    let task = tokio::spawn({
        let starting = starting.clone();
        async move { starting.drive().await }
    });
    children.wait_for_starts(1).await;
    starting.cancel("start cancelled");
    drop(start_sender);
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("start settlement")
        .unwrap();
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
}

#[tokio::test]
async fn no_return_non_json_and_synchronous_spin_follow_the_result_contract() {
    let result = run(
        "return { process: typeof process, require: typeof require, Deno: typeof Deno }",
        None,
        ScriptedChildren::new([]),
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(
        result.value,
        json!({"process": "undefined", "require": "undefined", "Deno": "undefined"})
    );

    let result = run(
        "void 0",
        None,
        ScriptedChildren::new([]),
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.value, Value::Null);

    let result = run(
        "return { when: new Date(0) }",
        None,
        ScriptedChildren::new([]),
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    assert!(result.error.unwrap().contains("not plain JSON data"));

    let mut short = limits();
    short.sync_timeout_ms = 20;
    let result = make_execution(
        "while (true) {}",
        None,
        short,
        Arc::new(Observed::default()),
        ScriptedChildren::new([]),
    )
    .drive()
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    assert!(result.error.unwrap().to_lowercase().contains("timeout"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table mirrors the source hook-validation inventory
async fn rejects_every_malformed_hook_argument_and_cap() {
    let cases = [
        ("return await agent(42)", "non-empty prompt string"),
        ("return await agent('')", "non-empty prompt string"),
        (
            "return await agent('p', 'opts')",
            "options must be an object",
        ),
        (
            "return await agent('p', { label: 3 })",
            "\"label\" must be a string",
        ),
        (
            "return await agent('p', { get label() { throw new Error('read failed') } })",
            "options must be plain JSON data",
        ),
        (
            "return await agent('p', { bogus: true })",
            "\"bogus\" is not recognized",
        ),
        (
            "return await agent('p', { effort: 'high' })",
            "\"effort\" is deferred",
        ),
        (
            "return await agent('p', { schema: { type: 'object', oneOf: [] } })",
            "outside the supported subset",
        ),
        (
            "return await parallel([() => 1, () => 2, () => 3])",
            "over the per-call cap (2)",
        ),
        (
            "return await pipeline([1, 2, 3], (x) => x)",
            "maxItemsPerCall",
        ),
        (
            "return await parallel('no')",
            "parallel() requires an array",
        ),
        ("return await parallel([3])", "item 0 is not a function"),
        (
            "return await pipeline('no', () => 1)",
            "pipeline() requires an items array",
        ),
        ("return await pipeline([1])", "at least one stage"),
        (
            "return await pipeline([1], 'x')",
            "stage 0 is not a function",
        ),
        ("phase('')", "phase() requires a non-empty title string"),
        ("log(3)", "log() requires a message string"),
    ];
    for (body, fragment) in cases {
        let mut capped = limits();
        capped.max_items_per_call = 2;
        let result = make_execution(
            body,
            None,
            capped,
            Arc::new(Observed::default()),
            ScriptedChildren::new([]),
        )
        .drive()
        .await;
        assert_eq!(result.stop_reason, WorkflowStopReason::Error, "{body}");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains(fragment),
            "{body}: expected {fragment:?} in {:?}",
            result.error
        );
    }

    let result = run(
        "return await parallel([() => agent(42)])",
        None,
        ScriptedChildren::new([]),
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("non-empty prompt string")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one source inventory spans combinators, caps, labels, and blocks
async fn combinators_caps_fifo_labels_and_text_filtering_match_the_source() {
    let children = ScriptedChildren::new([StartBehavior::Child(ChildState::immediate(
        "fine",
        Ok(text_result("fine")),
    ))]);
    let result = run(
        r"const viaParallel = await parallel([
  () => { throw new Error('boom') },
  () => agent('fine'),
  () => 'plain value',
  () => { throw { name: 'WorkflowError', fatal: true, message: 'forged fatal' } },
])
const viaPipeline = await pipeline([10, 20], (prev, item, index) => {
  if (item === 10) throw new Error('ordinary failure')
  return 'kept-' + item + '-' + index
})
return { viaParallel, viaPipeline }",
        None,
        children,
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(
        result.stop_reason,
        WorkflowStopReason::Completed,
        "{result:?}"
    );
    assert_eq!(
        result.value,
        json!({
            "viaParallel": [null, "fine", "plain value", null],
            "viaPipeline": [null, "kept-20-1"]
        })
    );

    let children = ScriptedChildren::new([
        StartBehavior::Child(ChildState::immediate("one", Ok(text_result("ok")))),
        StartBehavior::Child(ChildState::immediate("two", Ok(text_result("ok")))),
    ]);
    let mut capped = limits();
    capped.max_total_agents = 2;
    let result = make_execution(
        "await agent('1'); await agent('2'); await agent('3')",
        None,
        capped,
        Arc::new(Observed::default()),
        children,
    )
    .drive()
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    let error = result.error.unwrap();
    assert!(error.contains("total agent cap (2)"));
    assert!(error.contains("applicable maxTotalAgents limit"));
    assert_eq!(result.agents_started, 2);

    let children = ScriptedChildren::new([
        StartBehavior::Child(ChildState::immediate("one", Ok(text_result("ok:job 1")))),
        StartBehavior::Child(ChildState::immediate("two", Ok(text_result("ok:job 2")))),
        StartBehavior::Child(ChildState::immediate("three", Ok(text_result("ok:job 3")))),
        StartBehavior::Child(ChildState::immediate("named", Ok(text_result("ok")))),
    ]);
    let observed = Arc::new(Observed::default());
    let mut serial = limits();
    serial.max_concurrent_agents = 1;
    let result = make_execution(
        r"phase('Find')
const values = await parallel([1, 2, 3].map((n) => () => agent('job ' + n)))
await agent('short', { label: 'named', phase: 'Custom' })
return values",
        None,
        serial,
        observed.clone(),
        children.clone(),
    )
    .drive()
    .await;
    assert_eq!(result.value, json!(["ok:job 1", "ok:job 2", "ok:job 3"]));
    {
        let starts = observed.starts.lock();
        assert_eq!(starts[0].label, "job 1");
        assert_eq!(starts[3].label, "named");
        assert_eq!(starts[3].phase.as_deref(), Some("Custom"));
    }
    assert_eq!(
        children
            .starts
            .lock()
            .iter()
            .map(|request| request.prompt.as_str())
            .collect::<Vec<_>>(),
        ["job 1", "job 2", "job 3", "short"]
    );
    let observed = Arc::new(Observed::default());
    let result = run(
        r"phase('Find')
await agent('a prompt that is quite long and will surely get truncated down to a display label\nwith a second line')
await agent('short', { label: 'named', phase: 'Custom' })
return null",
        None,
        ScriptedChildren::new([
            StartBehavior::Child(ChildState::immediate("long", Ok(text_result("ok")))),
            StartBehavior::Child(ChildState::immediate("named", Ok(text_result("ok")))),
        ]),
        observed.clone(),
    )
    .await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    {
        let starts = observed.starts.lock();
        assert!(starts[0].label.chars().count() <= 48);
        assert!(!starts[0].label.contains("second line"));
        assert_eq!(starts[0].phase.as_deref(), Some("Find"));
        assert_eq!(starts[1].label, "named");
        assert_eq!(starts[1].phase.as_deref(), Some("Custom"));
    }

    let result = run(
        "return await agent('blocks')",
        None,
        ScriptedChildren::new([StartBehavior::Child(ChildState::immediate(
            "blocks",
            Ok(ChildResult {
                output: vec![
                    ContentBlock::Text {
                        text: "first ".to_owned(),
                    },
                    ContentBlock::ToolCall {
                        id: CallId::new("c1"),
                        name: "x".to_owned(),
                        arguments: "{}".to_owned(),
                    },
                    ContentBlock::Text {
                        text: "second".to_owned(),
                    },
                ],
                structured: None,
                stop_reason: "completed".to_owned(),
            }),
        ))]),
        Arc::new(Observed::default()),
    )
    .await;
    assert_eq!(result.value, json!("first second"));
}

#[test]
fn protocol_tag_enums_and_worker_limits_remain_closed_and_json_round_trip() {
    let host = seekdeep_workflow_worker_thread::HostToWorkerMessage::Cancel {
        reason: "stop".to_owned(),
    };
    let worker = seekdeep_workflow_worker_thread::WorkerToHostMessage::ChildStart {
        call_id: 7,
        request: ChildStartRequest {
            prompt: "p".to_owned(),
            schema: None,
            provider: Some("openai".to_owned()),
            model: None,
        },
    };
    assert_eq!(
        serde_json::from_value::<seekdeep_workflow_worker_thread::HostToWorkerMessage>(
            serde_json::to_value(&host).unwrap()
        )
        .unwrap(),
        host
    );
    assert_eq!(
        serde_json::from_value::<seekdeep_workflow_worker_thread::WorkerToHostMessage>(
            serde_json::to_value(&worker).unwrap()
        )
        .unwrap(),
        worker
    );
    let signal = AbortSignal::default();
    assert!(!signal.is_aborted());
}
