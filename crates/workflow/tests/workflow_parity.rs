//! Behavioral mirror of `packages/workflow/workflow/tests/workflow.spec.ts`.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_workflow::{
    WORKFLOW_ENGINE, WorkflowAgentInfo, WorkflowEngine, WorkflowEngineService, WorkflowError,
    WorkflowErrorCode, WorkflowEventName, WorkflowMeta, WorkflowRun, WorkflowRunId,
    WorkflowRunInfo, WorkflowStartRequest, emit_workflow_event, is_fatal_workflow_error,
};

#[derive(Debug)]
struct StubEngine;

impl WorkflowEngine for StubEngine {
    fn start(&self, _request: WorkflowStartRequest) -> Arc<dyn WorkflowRun> {
        panic!("start is not under test")
    }
}

fn info() -> WorkflowRunInfo {
    WorkflowRunInfo {
        id: WorkflowRunId::new("run-1"),
        meta: WorkflowMeta {
            name: "w".to_owned(),
            description: "d".to_owned(),
            when_to_use: None,
            phases: None,
        },
    }
}

fn provide(context: &Context) -> Arc<WorkflowEngineService> {
    let service = WorkflowEngineService::new(Arc::new(StubEngine));
    service.provide(context).expect("provide workflow engine");
    service
}

#[test]
fn run_id_and_error_taxonomy_match_the_public_contract() {
    assert_eq!(WorkflowRunId::new("abc").as_str(), "abc");

    let fatal = WorkflowError::new("cap hit", WorkflowErrorCode::AgentCap);
    assert_eq!(fatal.code, WorkflowErrorCode::AgentCap);
    assert!(fatal.fatal);
    assert_eq!(fatal.name(), "WorkflowError");
    let fatal = anyhow::Error::new(fatal);
    assert!(is_fatal_workflow_error(&fatal));

    let soft = anyhow::Error::new(WorkflowError::with_fatal(
        "advisory",
        WorkflowErrorCode::ItemCap,
        false,
    ));
    assert!(!is_fatal_workflow_error(&soft));
    assert!(!is_fatal_workflow_error(&anyhow::anyhow!("plain")));
}

#[tokio::test]
async fn service_registers_and_unregisters_with_its_owner() {
    let context = Context::new();
    let service = provide(&context);
    assert!(Arc::ptr_eq(
        &context.get(WORKFLOW_ENGINE).expect("workflowEngine"),
        &service
    ));
    context.fiber().dispose().await.expect("dispose");
    assert!(context.get(WORKFLOW_ENGINE).is_none());
}

#[test]
fn workflow_events_dispatch_the_original_payload_tuple_to_every_listener() {
    let context = Context::new();
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen_log = seen.clone();
    context
        .events()
        .on_sync(
            &context,
            WorkflowEventName::Log.as_str(),
            move |_, args| {
                let info = args
                    .get::<WorkflowRunInfo>(0)
                    .ok_or_else(|| anyhow::anyhow!("missing info"))?;
                let message = args
                    .get::<String>(1)
                    .ok_or_else(|| anyhow::anyhow!("missing message"))?;
                seen_log
                    .lock()
                    .push(format!("{}:{message}", info.id.as_str()));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("log listener");
    let seen_agent = seen.clone();
    context
        .events()
        .on_sync(
            &context,
            WorkflowEventName::AgentStart.as_str(),
            move |_, args| {
                let agent = args
                    .get::<WorkflowAgentInfo>(1)
                    .ok_or_else(|| anyhow::anyhow!("missing agent"))?;
                seen_agent.lock().push(agent.label.clone());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("agent listener");

    let info = info();
    emit_workflow_event(
        &context,
        WorkflowEventName::Start,
        &EventArgs::one(info.clone()),
    )
    .expect("start");
    emit_workflow_event(
        &context,
        WorkflowEventName::Log,
        &EventArgs::from_values(vec![Arc::new(info.clone()), Arc::new("hello".to_owned())]),
    )
    .expect("log");
    emit_workflow_event(
        &context,
        WorkflowEventName::AgentStart,
        &EventArgs::from_values(vec![
            Arc::new(info),
            Arc::new(WorkflowAgentInfo {
                seq: 1,
                label: "original".to_owned(),
                phase: None,
                child_id: seekdeep_core::session::SessionId::new("c"),
            }),
        ]),
    )
    .expect("agent start");
    assert_eq!(&*seen.lock(), &["run-1:hello", "original"]);
}

#[tokio::test]
async fn async_rejection_is_contained_without_starving_later_listeners() {
    let context = Context::new();
    context
        .events()
        .on(
            &context,
            WorkflowEventName::AgentStart.as_str(),
            |_, _| {
                Box::pin(async {
                    tokio::task::yield_now().await;
                    anyhow::bail!("async observer failed")
                })
            },
            EventOptions::default(),
        )
        .expect("rejecting listener");
    let reached = Arc::new(AtomicUsize::new(0));
    let reached_listener = reached.clone();
    context
        .events()
        .on_sync(
            &context,
            WorkflowEventName::AgentStart.as_str(),
            move |_, _| {
                reached_listener.fetch_add(1, Ordering::AcqRel);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("later listener");

    emit_workflow_event(&context, WorkflowEventName::AgentStart, &EventArgs::new())
        .expect("contained emit");
    assert_eq!(reached.load(Ordering::Acquire), 1);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    context.fiber().dispose().await.expect("dispose");
}

#[derive(Debug)]
struct UnrenderableError;

impl fmt::Display for UnrenderableError {
    fn fmt(&self, _formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        panic!("coercion trap")
    }
}

impl std::error::Error for UnrenderableError {}

#[test]
fn immediate_failures_are_contained_per_listener_even_when_rendering_panics() {
    let context = Context::new();
    context
        .events()
        .on_sync(
            &context,
            WorkflowEventName::Phase.as_str(),
            |_, _| Err(anyhow::Error::new(UnrenderableError)),
            EventOptions::default(),
        )
        .expect("throwing listener");
    let reached = Arc::new(AtomicUsize::new(0));
    let reached_listener = reached.clone();
    context
        .events()
        .on_sync(
            &context,
            WorkflowEventName::Phase.as_str(),
            move |_, args| {
                assert_eq!(
                    args.get::<String>(1).as_deref().map(String::as_str),
                    Some("Scan")
                );
                reached_listener.fetch_add(1, Ordering::AcqRel);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("later listener");

    emit_workflow_event(
        &context,
        WorkflowEventName::Phase,
        &EventArgs::from_values(vec![Arc::new(info()), Arc::new("Scan".to_owned())]),
    )
    .expect("contained emit");
    assert_eq!(reached.load(Ordering::Acquire), 1);
}
