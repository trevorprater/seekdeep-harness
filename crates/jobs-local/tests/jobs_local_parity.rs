//! Behavioral parity tests for the process-local job registry.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentOptions, AgentRegistry, Inbox, InboxNotifications, NoopInboxNotifications,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_jobs::{
    JobHooks, JobKillOutcome, JobOutcome, JobRegistry, JobStart, JobTerminalStatus,
};
use seekdeep_jobs_local::{Config, LocalJobRegistry};
use seekdeep_scope::{Scope, ScopeKey, create_scope};
use seekdeep_util::abort::AbortSignal;
use serde_json::json;
use tokio::sync::oneshot;

fn config(limit: u64) -> Config {
    Config {
        max_concurrent_jobs_per_owner: Some(limit),
    }
}

fn stub_agent(context: &Context, id: &str, preset: Option<ScopeKey>) -> (Arc<Agent>, Scope) {
    let session_id = SessionId::new(id);
    let session = Session::create(&session_id, None, None).expect("session");
    let notifications: Arc<dyn InboxNotifications> = Arc::new(NoopInboxNotifications);
    let inbox = Arc::new(Inbox::new(session.clone(), notifications).expect("inbox"));
    let key = ScopeKey::new();
    let scope = create_scope(context, key, preset).expect("scope");
    let agent = Arc::new(Agent::new(
        session_id,
        AgentOptions::default(),
        session,
        inbox,
        scope.context.clone(),
        key,
    ));
    (agent, scope)
}

/// A controllable producer whose `done` resolves through a oneshot channel.
struct TestHooks {
    cancels: Arc<Mutex<Vec<Option<String>>>>,
    outcome_rx: Mutex<Option<oneshot::Receiver<Result<JobOutcome, String>>>>,
    cancel_tx: Mutex<Option<oneshot::Sender<Result<JobOutcome, String>>>>,
    chunks: Option<Arc<Mutex<VecDeque<String>>>>,
}

impl JobHooks for TestHooks {
    fn cancel(&self, reason: Option<&str>) {
        self.cancels.lock().push(reason.map(str::to_owned));
        if let Some(tx) = self.cancel_tx.lock().take() {
            let _ = tx.send(Ok(JobOutcome {
                status: JobTerminalStatus::Killed,
                detail: None,
                output: None,
            }));
        }
    }

    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let rx = self
            .outcome_rx
            .lock()
            .take()
            .expect("done() must be called exactly once");
        Box::pin(async move {
            rx.await.map_or_else(
                |_| Err(anyhow::anyhow!("producer dropped")),
                |result| result.map_err(anyhow::Error::msg),
            )
        })
    }

    fn read_output(&self) -> Option<String> {
        self.chunks
            .as_ref()
            .map(|chunks| chunks.lock().pop_front().unwrap_or_default())
    }
}

/// The start spec plus the test-side control handles.
struct Producer {
    spec: JobStart,
    settle: Option<oneshot::Sender<Result<JobOutcome, String>>>,
    cancels: Arc<Mutex<Vec<Option<String>>>>,
}

fn producer(
    owner: Option<Arc<Agent>>,
    kind: &str,
    label: &str,
    settle_on_cancel: bool,
    output_limit_bytes: Option<u64>,
    chunks: Option<Vec<String>>,
) -> Producer {
    let (tx, rx) = oneshot::channel();
    let cancels = Arc::new(Mutex::new(Vec::new()));
    let cancels_clone = cancels.clone();
    let chunks =
        chunks.map(|chunks| Arc::new(Mutex::new(chunks.into_iter().collect::<VecDeque<_>>())));
    let (settle, cancel_tx) = if settle_on_cancel {
        (None, Some(tx))
    } else {
        (Some(tx), None)
    };
    let spec = JobStart {
        kind: kind.to_owned(),
        label: label.to_owned(),
        output_limit_bytes,
        owner,
        run: Box::new(move || {
            Box::new(TestHooks {
                cancels: cancels_clone,
                outcome_rx: Mutex::new(Some(rx)),
                cancel_tx: Mutex::new(cancel_tx),
                chunks,
            })
        }),
    };
    Producer {
        spec,
        settle,
        cancels,
    }
}

fn settle_producer(
    handle: Option<oneshot::Sender<Result<JobOutcome, String>>>,
    outcome: JobOutcome,
) {
    handle
        .expect("settle-on-cancel producer has no external settle handle")
        .send(Ok(outcome))
        .expect("outcome receiver dropped");
}

async fn tick() {
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-string panic".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

#[tokio::test]
async fn start_refuses_while_no_controller_serves_the_owner() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    let producer = producer(None, "bash", "sleep 60", false, None, None);
    let result = catch_unwind(AssertUnwindSafe(|| registry.start(producer.spec)));
    let message = panic_message(&result.expect_err("must refuse"));
    assert!(
        message.contains("no job controller serves this agent"),
        "{message}"
    );
}

#[tokio::test]
async fn start_validates_kind_label_and_output_limit() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");

    for (spec, expected) in [
        (
            producer(None, "", "x", false, None, None).spec,
            "invalid job kind",
        ),
        (
            producer(None, "bash", "", false, None, None).spec,
            "invalid job label",
        ),
        (
            producer(None, "bash", "x", false, Some(0), None).spec,
            "outputLimitBytes",
        ),
    ] {
        let result = catch_unwind(AssertUnwindSafe(|| registry.start(spec)));
        let message = panic_message(&result.expect_err("must reject"));
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn config_schema_rejects_invalid_and_fills_default() {
    let schema = seekdeep_jobs_local::config_schema();
    for invalid in [
        json!({ "maxConcurrentJobsPerOwner": 0 }),
        json!({ "maxConcurrentJobsPerOwner": -1 }),
        json!({ "maxConcurrentJobsPerOwner": 1.5 }),
        json!({ "maxConcurrentJobsPerOwner": 9_007_199_254_740_992_u64 }),
    ] {
        assert!(schema.resolve(&invalid).is_err(), "{invalid} must reject");
    }
    for valid in [
        json!({ "maxConcurrentJobsPerOwner": 1 }),
        json!({ "maxConcurrentJobsPerOwner": 9_007_199_254_740_991_u64 }),
    ] {
        assert!(schema.resolve(&valid).is_ok(), "{valid} must accept");
    }
    let defaulted = schema.resolve(&json!({})).expect("defaults");
    assert_eq!(defaulted["maxConcurrentJobsPerOwner"], json!(10));
}

#[tokio::test]
async fn start_limits_each_owner_bucket() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, config(1)).expect("registry");
    registry.attach_controller("test");
    let first = producer(None, "bash", "first", false, None, None);
    assert_eq!(registry.start(first.spec).as_str(), "bash-1");

    let blocked = producer(None, "bash", "blocked", false, None, None);
    let result = catch_unwind(AssertUnwindSafe(|| registry.start(blocked.spec)));
    let message = panic_message(&result.expect_err("must block"));
    assert!(message.contains("(limit: 1)"), "{message}");
}

#[tokio::test]
async fn issues_kind_prefixed_ids_from_per_kind_counters() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    assert_eq!(
        registry
            .start(producer(None, "bash", "a", false, None, None).spec)
            .as_str(),
        "bash-1"
    );
    assert_eq!(
        registry
            .start(producer(None, "bash", "b", false, None, None).spec)
            .as_str(),
        "bash-2"
    );
    assert_eq!(
        registry
            .start(producer(None, "subagent", "c", false, None, None).spec)
            .as_str(),
        "subagent-1"
    );
}

#[tokio::test]
async fn list_shows_owned_plus_unowned_jobs() {
    let ctx = Context::new();
    let agents = Arc::new(AgentRegistry::new(ctx.clone()));
    agents.provide(&ctx).expect("agents");
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");

    let (alice, alice_scope) = stub_agent(&ctx, "alice", None);
    let (bob, bob_scope) = stub_agent(&ctx, "bob", None);
    agents.register(&ctx, &alice, None).expect("alice");
    agents.register(&ctx, &bob, None).expect("bob");

    let alice_task =
        registry.start(producer(Some(alice.clone()), "bash", "a", false, None, None).spec);
    let bob_task = registry.start(producer(Some(bob.clone()), "bash", "b", false, None, None).spec);
    let open_task = registry.start(producer(None, "subagent", "open", false, None, None).spec);

    let alice_ids: Vec<_> = registry
        .list(Some(&alice))
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(alice_ids, vec![alice_task.clone(), open_task.clone()]);
    let bob_ids: Vec<_> = registry
        .list(Some(&bob))
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(bob_ids, vec![bob_task, open_task.clone()]);
    let open_ids: Vec<_> = registry.list(None).into_iter().map(|s| s.id).collect();
    assert_eq!(open_ids, vec![open_task]);

    alice_scope.dispose().await.expect("alice scope");
    bob_scope.dispose().await.expect("bob scope");
}

#[tokio::test]
async fn read_streams_delta_and_marks_terminal_reported() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let Producer { spec, settle, .. } = producer(
        None,
        "bash",
        "sleep 60",
        false,
        Some(64),
        Some(vec!["first".to_owned(), String::new(), "rest".to_owned()]),
    );
    let id = registry.start(spec);

    let first = registry.read(&id, None).expect("read");
    assert_eq!(first.text, "first");
    assert!(!first.snapshot.reported);
    assert_eq!(registry.read(&id, None).expect("read").text, "");

    settle_producer(
        settle,
        JobOutcome {
            status: JobTerminalStatus::Completed,
            detail: Some("exit code: 0".to_owned()),
            output: None,
        },
    );
    tick().await;

    let read = registry.read(&id, None).expect("read");
    assert_eq!(read.text, "rest");
    assert!(read.snapshot.reported);
    assert_eq!(read.snapshot.output_limit_bytes, Some(64));
}

#[tokio::test]
async fn final_output_jobs_read_idempotently_after_settlement() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let Producer { spec, settle, .. } = producer(None, "subagent", "research", false, None, None);
    let id = registry.start(spec);

    assert_eq!(registry.read(&id, None).expect("read").text, "");
    settle_producer(
        settle,
        JobOutcome {
            status: JobTerminalStatus::Completed,
            detail: None,
            output: Some("final answer".to_owned()),
        },
    );
    tick().await;
    assert_eq!(registry.read(&id, None).expect("read").text, "final answer");
    assert_eq!(registry.read(&id, None).expect("read").text, "final answer");
}

#[tokio::test]
async fn unknown_job_id_fails_loud() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let id = seekdeep_jobs::JobId::new("bash-99");
    let error = registry.get(&id, None).expect_err("unknown");
    assert!(error.to_string().contains("unknown job bash-99"), "{error}");
}

#[tokio::test]
async fn kill_cancels_with_reason_and_reports_already_finished() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let Producer {
        spec,
        settle,
        cancels,
    } = producer(None, "bash", "sleep 60", false, None, None);
    let id = registry.start(spec);

    assert_eq!(
        registry
            .kill(&id, None, Some("no longer needed"))
            .expect("kill"),
        JobKillOutcome::Requested
    );
    assert_eq!(*cancels.lock(), vec![Some("no longer needed".to_owned())]);
    assert!(registry.get(&id, None).expect("get").reported);

    settle_producer(
        settle,
        JobOutcome {
            status: JobTerminalStatus::Killed,
            detail: None,
            output: None,
        },
    );
    tick().await;
    assert_eq!(
        registry.kill(&id, None, None).expect("kill"),
        JobKillOutcome::AlreadyFinished
    );
}

#[tokio::test]
async fn throwing_cancel_leaves_state_unchanged() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");

    let broken = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let broken_hooks = broken.clone();
    let (tx, rx) = oneshot::channel::<Result<JobOutcome, String>>();
    let spec = JobStart {
        kind: "bash".to_owned(),
        label: "flaky cancel".to_owned(),
        output_limit_bytes: None,
        owner: None,
        run: Box::new(move || {
            Box::new(FlakyHooks {
                broken: broken_hooks,
                rx: Mutex::new(Some(rx)),
            })
        }),
    };
    let id = registry.start(spec);

    let error = registry.kill(&id, None, None).expect_err("cancel throws");
    assert!(error.to_string().contains("cancel boom"), "{error}");
    let snapshot = registry.get(&id, None).expect("get");
    assert!(!snapshot.reported);
    assert!(!snapshot.status.is_terminal());

    broken.store(false, std::sync::atomic::Ordering::Release);
    tx.send(Ok(JobOutcome {
        status: JobTerminalStatus::Completed,
        detail: None,
        output: None,
    }))
    .expect("settle");
    tick().await;
    assert_eq!(
        registry.kill(&id, None, None).expect("kill"),
        JobKillOutcome::AlreadyFinished
    );
}

struct FlakyHooks {
    broken: Arc<std::sync::atomic::AtomicBool>,
    rx: Mutex<Option<oneshot::Receiver<Result<JobOutcome, String>>>>,
}

impl JobHooks for FlakyHooks {
    fn cancel(&self, _reason: Option<&str>) {
        assert!(
            !self.broken.load(std::sync::atomic::Ordering::Acquire),
            "cancel boom"
        );
    }

    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let rx = self.rx.lock().take().expect("done once");
        Box::pin(async move {
            rx.await.map_or_else(
                |_| Err(anyhow::anyhow!("dropped")),
                |r| r.map_err(anyhow::Error::msg),
            )
        })
    }
}

#[tokio::test]
async fn wait_resolves_on_settlement_and_times_out_live() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");

    let Producer { spec, settle, .. } = producer(None, "bash", "settle", false, None, None);
    let settle_id = registry.start(spec);
    let wait = registry.wait(&settle_id, 5_000.0, None, None);
    settle_producer(
        settle,
        JobOutcome {
            status: JobTerminalStatus::Completed,
            detail: Some("exit code: 0".to_owned()),
            output: None,
        },
    );
    let snapshot = wait.await.expect("settled wait");
    assert!(snapshot.reported);
    assert!(snapshot.status.is_terminal());

    let live = producer(None, "bash", "live", false, None, None);
    let live_id = registry.start(live.spec);
    let snapshot = registry
        .wait(&live_id, 5.0, None, None)
        .await
        .expect("timed-out wait");
    assert!(!snapshot.reported);
    assert!(!snapshot.status.is_terminal());
}

#[tokio::test]
async fn wait_rejects_callers_abort_and_pre_aborted_signal() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let producer = producer(None, "bash", "sleep 60", false, None, None);
    let id = registry.start(producer.spec);

    let pre_aborted = AbortSignal::default();
    pre_aborted.abort();
    let error = registry
        .wait(&id, 5_000.0, None, Some(pre_aborted))
        .await
        .expect_err("pre-aborted");
    assert!(error.to_string().contains("wait aborted"), "{error}");

    let signal = AbortSignal::default();
    let wait = registry.wait(&id, 5_000.0, None, Some(signal.clone()));
    signal.abort();
    let error = wait.await.expect_err("aborted");
    assert!(error.to_string().contains("wait aborted"), "{error}");
    assert!(!registry.get(&id, None).expect("get").status.is_terminal());
}

#[tokio::test]
async fn on_job_done_fires_once_and_contains_listener_throws() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let seen = Arc::new(Mutex::new(Vec::new()));
    registry.on_job_done(Arc::new(|_snapshot, _owner| {
        panic!("listener boom");
    }));
    let seen_clone = seen.clone();
    registry.on_job_done(Arc::new(move |snapshot, _owner| {
        seen_clone.lock().push(snapshot.id.clone());
    }));

    let Producer { spec, settle, .. } = producer(None, "bash", "sleep 60", false, None, None);
    let id = registry.start(spec);
    settle_producer(
        settle,
        JobOutcome {
            status: JobTerminalStatus::Completed,
            detail: Some("exit code: 0".to_owned()),
            output: None,
        },
    );
    tick().await;

    assert_eq!(*seen.lock(), vec![id]);
}

#[tokio::test]
async fn on_jobs_changed_fires_on_registration_and_settlement() {
    let ctx = Context::new();
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");
    let seen = Arc::new(Mutex::new(Vec::new()));
    registry.on_jobs_changed(Arc::new(|_owner| {
        panic!("observer boom");
    }));
    let seen_clone = seen.clone();
    registry.on_jobs_changed(Arc::new(move |owner| {
        seen_clone.lock().push(owner.is_some());
    }));

    let Producer { spec, settle, .. } = producer(None, "bash", "sleep 60", false, None, None);
    let id = registry.start(spec);
    assert_eq!(id.as_str(), "bash-1");
    assert_eq!(*seen.lock(), vec![false]);

    settle_producer(
        settle,
        JobOutcome {
            status: JobTerminalStatus::Completed,
            detail: None,
            output: None,
        },
    );
    tick().await;
    assert_eq!(*seen.lock(), vec![false, false]);
}

#[tokio::test]
async fn owner_disposal_cancels_live_jobs_and_drops_snapshots() {
    let ctx = Context::new();
    let agents = Arc::new(AgentRegistry::new(ctx.clone()));
    agents.provide(&ctx).expect("agents");
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");

    let (owner, scope) = stub_agent(&ctx, "owner", None);
    agents.register(&ctx, &owner, None).expect("register");
    let Producer { spec, cancels, .. } = producer(
        Some(owner.clone()),
        "subagent",
        "long research",
        true,
        None,
        None,
    );
    let id = registry.start(spec);
    assert_eq!(id.as_str(), "subagent-1");

    scope.dispose().await.expect("dispose owner scope");

    assert_eq!(*cancels.lock(), vec![Some("owner disposed".to_owned())]);
    assert!(registry.list(Some(&owner)).is_empty());
}

#[tokio::test]
async fn service_disposal_cancels_live_jobs_and_silences_listeners() {
    let ctx = Context::new();
    let agents = Arc::new(AgentRegistry::new(ctx.clone()));
    agents.provide(&ctx).expect("agents");
    let registry = LocalJobRegistry::new(&ctx, Config::default()).expect("registry");
    registry.attach_controller("test");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen.clone();
    registry.on_job_done(Arc::new(move |snapshot, _owner| {
        seen_clone.lock().push(snapshot.id.clone());
    }));

    let Producer { spec, cancels, .. } = producer(None, "bash", "sleep 600", true, None, None);
    registry.start(spec);

    ctx.fiber().restart().await.expect("service disposal");

    assert_eq!(
        *cancels.lock(),
        vec![Some("jobs service disposed".to_owned())]
    );
    assert!(seen.lock().is_empty());
}

#[tokio::test]
async fn scoped_controller_serves_exactly_its_composed_agents() {
    let ctx = Context::new();
    let agents = Arc::new(AgentRegistry::new(ctx.clone()));
    agents.provide(&ctx).expect("agents");
    let preset = ScopeKey::new();
    let mount = create_scope(&ctx, preset, None).expect("mount");
    let registry = LocalJobRegistry::new(&mount.context, Config::default()).expect("registry");
    registry.attach_controller("tool-jobs");

    let (served, served_scope) = stub_agent(&ctx, "served", Some(preset));
    let (unserved, unserved_scope) = stub_agent(&ctx, "unserved", None);
    agents.register(&ctx, &served, None).expect("served");
    agents.register(&ctx, &unserved, None).expect("unserved");

    let served_id =
        registry.start(producer(Some(served.clone()), "bash", "served", false, None, None).spec);
    assert_eq!(served_id.as_str(), "bash-1");

    let result = catch_unwind(AssertUnwindSafe(|| {
        registry.start(
            producer(
                Some(unserved.clone()),
                "bash",
                "unserved",
                false,
                None,
                None,
            )
            .spec,
        )
    }));
    let message = panic_message(&result.expect_err("unserved must refuse"));
    assert!(
        message.contains("no job controller serves this agent"),
        "{message}"
    );

    served_scope.dispose().await.expect("served scope");
    unserved_scope.dispose().await.expect("unserved scope");
    mount.dispose().await.expect("mount scope");
}

#[tokio::test]
async fn invariant_companion_registers_once() {
    let ctx = Context::new();
    let config = InvariantConfig::default();
    let registry = Arc::new(InvariantRegistry::new(&ctx, &config).expect("invariants"));
    let _registration = seekdeep_jobs_local::register_invariant(&registry).expect("register");
    assert!(registry.is_registered("@deepseek-ai/seekdeep-jobs-local"));
    assert!(seekdeep_jobs_local::register_invariant(&registry).is_err());
}
