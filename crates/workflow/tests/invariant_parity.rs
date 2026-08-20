//! Behavioral mirror of packages/workflow/workflow/tests/invariant.spec.ts.

use std::{any::Any, sync::Arc};

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::session::SessionId;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_workflow::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowAgentOutcome, WorkflowMeta,
    WorkflowResultInfo, WorkflowRunId, WorkflowRunInfo, WorkflowStopReason,
    invariant::register_invariant,
};

async fn setup() -> Context {
    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("register");
    registration.await_ready().await.expect("ready");
    context
}

fn run() -> WorkflowRunInfo {
    WorkflowRunInfo {
        id: WorkflowRunId::new("workflow-1"),
        meta: WorkflowMeta {
            name: "review".to_owned(),
            description: "Review a change".to_owned(),
            when_to_use: None,
            phases: None,
        },
    }
}

fn agent(seq: u64) -> WorkflowAgentInfo {
    WorkflowAgentInfo {
        seq,
        label: "reviewer".to_owned(),
        phase: None,
        child_id: SessionId::new("child-1"),
    }
}

fn agent_end(seq: u64, child_id: &str) -> WorkflowAgentEndInfo {
    WorkflowAgentEndInfo {
        info: WorkflowAgentInfo {
            seq,
            label: "reviewer".to_owned(),
            phase: None,
            child_id: SessionId::new(child_id),
        },
        outcome: WorkflowAgentOutcome::Completed,
    }
}

fn result(
    stop_reason: WorkflowStopReason,
    error: Option<String>,
    agents_started: u64,
) -> WorkflowResultInfo {
    WorkflowResultInfo {
        stop_reason,
        error,
        agents_started,
    }
}

fn emit1<A: Any + Send + Sync>(context: &Context, name: &str, a: A) -> anyhow::Result<()> {
    context
        .events()
        .emit(context, name, &EventArgs::from_values(vec![Arc::new(a)]))
}

fn emit2<A: Any + Send + Sync, B: Any + Send + Sync>(
    context: &Context,
    name: &str,
    a: A,
    b: B,
) -> anyhow::Result<()> {
    context.events().emit(
        context,
        name,
        &EventArgs::from_values(vec![Arc::new(a), Arc::new(b)]),
    )
}

#[tokio::test]
async fn accepts_a_complete_workflow_and_child_lifecycle() {
    let ctx = setup().await;
    let run = run();
    emit1(&ctx, "workflow/start", run.clone()).expect("start");
    emit2(&ctx, "workflow/phase", run.clone(), "inspect".to_owned()).expect("phase");
    emit2(&ctx, "workflow/log", run.clone(), "working".to_owned()).expect("log");
    emit2(&ctx, "workflow/agent-start", run.clone(), agent(1)).expect("agent-start");
    emit2(
        &ctx,
        "workflow/agent-end",
        run.clone(),
        agent_end(1, "child-1"),
    )
    .expect("agent-end");
    emit2(
        &ctx,
        "workflow/end",
        run.clone(),
        result(WorkflowStopReason::Completed, None, 1),
    )
    .expect("end");
    emit1(&ctx, "tools/change", ()).expect("unrelated");
}

#[tokio::test]
async fn rejects_invalid_run_identity_and_enclosure() {
    let ctx = setup().await;

    let empty = WorkflowRunInfo {
        id: WorkflowRunId::new(""),
        ..run()
    };
    let error = emit1(&ctx, "workflow/start", empty).expect_err("empty id");
    assert!(error.to_string().contains("must be non-empty"));

    let run = run();
    emit1(&ctx, "workflow/start", run.clone()).expect("start");
    let error = emit1(&ctx, "workflow/start", run.clone()).expect_err("repeated");
    assert!(error.to_string().contains("repeated run id"));

    let diverged = WorkflowRunInfo {
        meta: WorkflowMeta {
            name: "other".to_owned(),
            description: "x".to_owned(),
            when_to_use: None,
            phases: None,
        },
        ..run.clone()
    };
    let error = emit2(&ctx, "workflow/log", diverged, "x".to_owned()).expect_err("meta");
    assert!(error.to_string().contains("meta diverges"));

    let fresh = setup().await;
    let error = emit2(&fresh, "workflow/log", run.clone(), "x".to_owned()).expect_err("missing");
    assert!(error.to_string().contains("no matching workflow/start"));
}

#[tokio::test]
async fn rejects_malformed_and_unpaired_child_lifecycles() {
    let ctx = setup().await;
    let run = run();
    emit1(&ctx, "workflow/start", run.clone()).expect("start");

    let error = emit2(&ctx, "workflow/agent-start", run.clone(), agent(0)).expect_err("seq");
    assert!(error.to_string().contains("seq must be positive"));

    emit2(&ctx, "workflow/agent-start", run.clone(), agent(1)).expect("agent-start");
    let error = emit2(&ctx, "workflow/agent-start", run.clone(), agent(1)).expect_err("repeat");
    assert!(error.to_string().contains("repeated seq"));

    let error = emit2(
        &ctx,
        "workflow/agent-end",
        run.clone(),
        agent_end(2, "child-1"),
    )
    .expect_err("unpaired");
    assert!(error.to_string().contains("no matching start"));

    let error = emit2(
        &ctx,
        "workflow/agent-end",
        run.clone(),
        agent_end(1, "other"),
    )
    .expect_err("identity");
    assert!(error.to_string().contains("identity diverges"));
}

#[tokio::test]
async fn rejects_inconsistent_terminal_results() {
    let ctx = setup().await;
    emit1(&ctx, "workflow/start", run()).expect("start");
    emit2(&ctx, "workflow/agent-start", run(), agent(1)).expect("agent-start");
    let error = emit2(
        &ctx,
        "workflow/end",
        run(),
        result(WorkflowStopReason::Completed, None, 1),
    )
    .expect_err("open agent");
    assert!(error.to_string().contains("without workflow/agent-end"));

    let ctx = setup().await;
    emit1(&ctx, "workflow/start", run()).expect("start");
    emit2(&ctx, "workflow/agent-start", run(), agent(1)).expect("agent-start");
    emit2(&ctx, "workflow/agent-end", run(), agent_end(1, "child-1")).expect("agent-end");
    let error = emit2(
        &ctx,
        "workflow/end",
        run(),
        result(WorkflowStopReason::Completed, None, 0),
    )
    .expect_err("count");
    assert!(
        error
            .to_string()
            .contains("covering every observed agent start")
    );

    let ctx = setup().await;
    emit1(&ctx, "workflow/start", run()).expect("start");
    let error = emit2(
        &ctx,
        "workflow/end",
        run(),
        result(
            WorkflowStopReason::Completed,
            Some("unexpected".to_owned()),
            0,
        ),
    )
    .expect_err("completed with error");
    assert!(error.to_string().contains("absent exactly for completed"));

    let ctx = setup().await;
    emit1(&ctx, "workflow/start", run()).expect("start");
    let error = emit2(
        &ctx,
        "workflow/end",
        run(),
        result(WorkflowStopReason::Error, None, 0),
    )
    .expect_err("error without error");
    assert!(error.to_string().contains("absent exactly for completed"));
}
