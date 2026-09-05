//! Approval state, Host-before-Client ordering, settlement, failure, and joining parity.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::*;
use seekdeep_identity::SessionId;
use serde_json::json;

#[test]
fn ui_activity_and_failure_snapshots_keep_exact_browser_shapes() {
    let activity = CordisRunActivity::AwaitingApproval {
        request_id: ApprovalRequestId::new("approval-1"),
        agent_id: SessionId::new("session-a"),
        package_id: CordisDynamicPackageId::new("pkg-1"),
        mode: DynamicCordisRunMode::Update,
        name: "Clock".to_owned(),
        purpose: "show time".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(activity).unwrap(),
        json!({
            "phase": "awaiting-approval",
            "requestId": "approval-1",
            "agentId": "session-a",
            "packageId": "pkg-1",
            "mode": "update",
            "name": "Clock",
            "purpose": "show time"
        })
    );
    let failure = CordisRunFailure {
        package_id: CordisDynamicPackageId::new("pkg-1"),
        reason: CordisPageFailureReason::ClientHalfFailed,
        message: "broken".to_owned(),
        stack: Some("stack".to_owned()),
    };
    assert_eq!(
        serde_json::to_value(failure).unwrap(),
        json!({
            "packageId": "pkg-1",
            "reason": "client-half-failed",
            "message": "broken",
            "stack": "stack"
        })
    );
}

async fn eventually(mut condition: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[derive(Debug)]
struct TokioSpawner;

impl ClientTaskSpawner for TokioSpawner {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        tokio::spawn(future);
    }
}

struct FakeRunner {
    events: Arc<Mutex<Vec<String>>>,
    result: Mutex<ClientLoadResult>,
}

impl DynamicCordisPackageRunner for FakeRunner {
    fn load(
        &self,
        request: ClientLoadRequest,
    ) -> BoxFuture<'static, Result<ClientLoadResult, ClientMountRejection>> {
        self.events
            .lock()
            .push(format!("load:{}", request.plugin_run_id));
        let result = self.result.lock().clone();
        Box::pin(async move { Ok(result) })
    }
}

struct FakeHost {
    events: Arc<Mutex<Vec<String>>>,
    run_result: Mutex<DynamicCordisHostHalfResult>,
    source_result: Mutex<Result<DynamicCordisClientSource, String>>,
    settle_result: Mutex<DynamicCordisRunResponse>,
    resolutions: Arc<Mutex<Vec<(ApprovalRequestId, DynamicCordisRunResolution)>>>,
    resolve_error: Mutex<Option<String>>,
    gate: Option<Arc<tokio::sync::Notify>>,
    run_calls: AtomicUsize,
}

impl CordisRunHostSeam for FakeHost {
    fn run_host_half(
        &self,
        plan: CordisUserRunRequest,
        request_id: Option<ApprovalRequestId>,
        _approve_future_versions: bool,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisHostHalfResult>> {
        self.run_calls.fetch_add(1, Ordering::AcqRel);
        self.events.lock().push(format!(
            "host:{}/{:?}",
            plan.plugin_id,
            request_id.as_ref().map(ApprovalRequestId::as_str)
        ));
        let result = self.run_result.lock().clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            Ok(result)
        })
    }

    fn get_client_code(
        &self,
        _agent_id: SessionId,
        _plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisClientSource>> {
        self.events.lock().push(format!("source:{plugin_run_id}"));
        let result = self.source_result.lock().clone();
        Box::pin(async move { result.map_err(anyhow::Error::msg) })
    }

    fn resolve_request_run(
        &self,
        request_id: ApprovalRequestId,
        resolution: DynamicCordisRunResolution,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisResolveAck>> {
        self.events.lock().push(format!("answer:{request_id}"));
        self.resolutions.lock().push((request_id, resolution));
        let error = self.resolve_error.lock().take();
        Box::pin(async move {
            match error {
                Some(error) => Err(anyhow::Error::msg(error)),
                None => Ok(DynamicCordisResolveAck { accepted: true }),
            }
        })
    }

    fn settle_user_run(
        &self,
        _agent_id: SessionId,
        plugin_id: CordisDynamicPluginId,
        _resolution: DynamicCordisRunResolution,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisRunResponse>> {
        self.events.lock().push(format!("settle:{plugin_id}"));
        let result = self.settle_result.lock().clone();
        Box::pin(async move { Ok(result) })
    }
}

struct Harness {
    orchestrator: Arc<CordisRunOrchestrator>,
    host: Arc<FakeHost>,
    runner: Arc<FakeRunner>,
    events: Arc<Mutex<Vec<String>>>,
    logs: Arc<Mutex<Vec<ClientOrchestratorLog>>>,
}

fn plugin() -> CordisDynamicPluginId {
    CordisDynamicPluginId::new("panel-1")
}

fn package() -> CordisDynamicPackageId {
    CordisDynamicPackageId::new("pkg-1")
}

fn run_id() -> CordisDynamicPluginRunId {
    CordisDynamicPluginRunId::new("run-1")
}

fn request(requires_approval: bool) -> CordisRunRequest {
    CordisRunRequest {
        request_id: ApprovalRequestId::new("approval-1"),
        agent_id: SessionId::new("session-a"),
        plugin_id: plugin(),
        package_id: package(),
        mode: DynamicCordisRunMode::Run,
        name: "panel".to_owned(),
        purpose: "render panel".to_owned(),
        requires_approval,
    }
}

fn user(has_client_half: bool) -> CordisUserRunRequest {
    CordisUserRunRequest {
        agent_id: SessionId::new("session-a"),
        plugin_id: plugin(),
        package_id: package(),
        mode: DynamicCordisRunMode::Run,
        has_client_half,
    }
}

fn harness(gate: Option<Arc<tokio::sync::Notify>>) -> Harness {
    let events = Arc::new(Mutex::new(Vec::new()));
    let host = Arc::new(FakeHost {
        events: events.clone(),
        run_result: Mutex::new(DynamicCordisHostHalfResult::Success {
            plugin_id: plugin(),
            package_id: package(),
            plugin_run_id: run_id(),
            waiting_for: Vec::new(),
            started_here: true,
        }),
        source_result: Mutex::new(Ok(DynamicCordisClientSource {
            code: "return { apply() {} }".to_owned(),
            name: "panel".to_owned(),
            plugin_id: plugin(),
            package_id: package(),
            plugin_run_id: run_id(),
        })),
        settle_result: Mutex::new(DynamicCordisRunResponse::Success {
            status: DynamicCordisRunSuccessStatus::Running,
            plugin_id: plugin(),
            package_id: package(),
            plugin_run_id: run_id(),
            waiting_for: Vec::new(),
            client_waiting_for: None,
            current_package_id: Some(package()),
            next_package_id: None,
            mode: DynamicCordisRunMode::Run,
        }),
        resolutions: Arc::new(Mutex::new(Vec::new())),
        resolve_error: Mutex::new(None),
        gate,
        run_calls: AtomicUsize::new(0),
    });
    let runner = Arc::new(FakeRunner {
        events: events.clone(),
        result: Mutex::new(ClientLoadResult::Success {
            plugin_run_id: run_id(),
            waiting_for: Some(vec!["slots".to_owned()]),
        }),
    });
    let logs = Arc::new(Mutex::new(Vec::new()));
    let observed_logs = logs.clone();
    let orchestrator = CordisRunOrchestrator::new_with_logger(
        runner.clone(),
        host.clone(),
        Arc::new(TokioSpawner),
        Arc::new(move |entry| observed_logs.lock().push(entry)),
    );
    Harness {
        orchestrator,
        host,
        runner,
        events,
        logs,
    }
}

#[test]
fn open_close_snapshots_and_notifications_preserve_the_waiting_request() {
    let harness = harness(None);
    let notices = Arc::new(AtomicUsize::new(0));
    let observed = notices.clone();
    let subscription = harness.orchestrator.subscribe(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel);
    }));
    harness.orchestrator.open(request(true));
    let first = harness.orchestrator.active_runs();
    let second = harness.orchestrator.active_runs();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(matches!(
        first.get(&plugin()),
        Some(CordisRunActivity::AwaitingApproval { name, purpose, .. })
            if name == "panel" && purpose == "render panel"
    ));
    harness
        .orchestrator
        .close(&ApprovalRequestId::new("approval-1"));
    assert!(harness.orchestrator.active_runs().is_empty());
    assert_eq!(notices.load(Ordering::Acquire), 2);
    subscription.dispose();
}

#[tokio::test]
async fn direct_run_orders_host_source_load_and_settlement_and_carries_waiting_services() {
    let harness = harness(None);
    harness.orchestrator.start_user_run(user(true)).await;
    assert_eq!(
        *harness.events.lock(),
        [
            "host:panel-1/None",
            "source:run-1",
            "load:run-1",
            "settle:panel-1",
        ]
    );
    assert!(harness.orchestrator.active_runs().is_empty());
    assert!(harness.orchestrator.last_run_error().is_empty());
}

#[tokio::test]
async fn approved_request_answers_success_after_host_and_client_and_rejection_touches_neither() {
    let harness = harness(None);
    harness.orchestrator.open(request(true));
    harness
        .orchestrator
        .approve(&ApprovalRequestId::new("approval-1"), true)
        .await;
    let resolutions = harness.host.resolutions.lock().clone();
    assert!(matches!(
        &resolutions[0].1,
        DynamicCordisRunResolution::Success { waiting_for, .. }
            if waiting_for.as_deref() == Some(&["slots".to_owned()][..])
    ));

    let second = request(true);
    harness.orchestrator.open(second.clone());
    harness.orchestrator.decline(&second.request_id).await;
    assert!(matches!(
        harness.host.resolutions.lock().last().unwrap().1,
        DynamicCordisRunResolution::Failure {
            reason: DynamicCordisRunFailureReason::Rejected,
            ..
        }
    ));
}

#[tokio::test]
async fn host_and_client_failures_short_circuit_and_publish_the_exact_page_failure() {
    let harness = harness(None);
    *harness.host.run_result.lock() = DynamicCordisHostHalfResult::Failure(CordisErrorDetails {
        message: "host broke".to_owned(),
        stack: None,
    });
    harness.orchestrator.start_user_run(user(true)).await;
    let failure = harness.orchestrator.last_run_error();
    assert_eq!(
        failure[&plugin()].reason,
        CordisPageFailureReason::HostHalfFailed
    );
    assert_eq!(failure[&plugin()].message, "host broke");
    assert_eq!(*harness.events.lock(), ["host:panel-1/None"]);

    *harness.host.run_result.lock() = DynamicCordisHostHalfResult::Success {
        plugin_id: plugin(),
        package_id: package(),
        plugin_run_id: run_id(),
        waiting_for: Vec::new(),
        started_here: true,
    };
    *harness.runner.result.lock() = ClientLoadResult::Failure {
        cause: ClientLoadErrorCause::Evaluate,
        error: CordisErrorDetails {
            message: "client broke".to_owned(),
            stack: None,
        },
    };
    harness.events.lock().clear();
    harness.orchestrator.start_user_run(user(true)).await;
    let failure = harness.orchestrator.last_run_error();
    assert_eq!(
        failure[&plugin()].reason,
        CordisPageFailureReason::ClientHalfFailed
    );
    assert_eq!(failure[&plugin()].message, "evaluate: client broke");
    assert!(matches!(
        harness.logs.lock().last(),
        Some(ClientOrchestratorLog::ClientActivationFailed { message, .. })
            if message == "evaluate: client broke"
    ));
    assert_eq!(
        *harness.events.lock(),
        [
            "host:panel-1/None",
            "source:run-1",
            "load:run-1",
            "settle:panel-1",
        ]
    );
}

#[tokio::test]
async fn refused_answer_is_logged_but_local_orchestration_still_settles() {
    let harness = harness(None);
    *harness.host.resolve_error.lock() = Some("transport refused".to_owned());
    harness.orchestrator.open(request(true));
    harness
        .orchestrator
        .approve(&ApprovalRequestId::new("approval-1"), false)
        .await;
    assert!(harness.orchestrator.active_runs().is_empty());
    assert!(matches!(
        harness.logs.lock().last(),
        Some(ClientOrchestratorLog::AnswerFailed { request_id, message })
            if request_id.as_str() == "approval-1" && message == "transport refused"
    ));
}

#[tokio::test]
async fn concurrent_same_plugin_calls_join_one_in_flight_activation() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let harness = harness(Some(gate.clone()));
    let first = harness.orchestrator.start_user_run(user(false));
    let second = harness.orchestrator.start_user_run(user(false));
    let first_task = tokio::spawn(first);
    let second_task = tokio::spawn(second);
    tokio::task::yield_now().await;
    assert_eq!(harness.host.run_calls.load(Ordering::Acquire), 1);
    gate.notify_one();
    first_task.await.unwrap();
    second_task.await.unwrap();
    assert_eq!(harness.host.run_calls.load(Ordering::Acquire), 1);
}

fn inventory(status: CordisRunStatus, requires_approval: bool) -> DynamicCordisInventoryRow {
    DynamicCordisInventoryRow {
        plugin_id: plugin(),
        agent_id: SessionId::new("session-a"),
        packages: vec![DynamicCordisInventoryPackage {
            package_id: package(),
            name: "panel".to_owned(),
            purpose: "render panel".to_owned(),
            has_host_half: true,
            has_client_half: true,
        }],
        current_package_id: None,
        next_package_id: Some(package()),
        active_run: None,
        latest_run: Some(DynamicCordisRunAttempt {
            plugin_run_id: run_id(),
            package_id: package(),
            mode: DynamicCordisRunMode::Run,
            status,
            approval_request_id: Some(ApprovalRequestId::new("approval-1")),
            requires_approval: Some(requires_approval),
            host: CordisHalfState {
                status: CordisHalfStatus::Pending,
                waiting_for: Vec::new(),
                error: None,
            },
            client: CordisHalfState {
                status: CordisHalfStatus::Pending,
                waiting_for: Vec::new(),
                error: None,
            },
            error: None,
        }),
    }
}

#[tokio::test]
async fn inventory_reconciliation_restores_waiting_and_automatic_requests_then_removes_stale_ones()
{
    let harness = harness(None);
    let notices = Arc::new(AtomicUsize::new(0));
    let observed = notices.clone();
    let _subscription = harness.orchestrator.subscribe(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel);
    }));
    harness
        .orchestrator
        .reconcile_approvals(&[inventory(CordisRunStatus::AwaitingApproval, true)]);
    assert!(matches!(
        harness.orchestrator.active_runs().get(&plugin()),
        Some(CordisRunActivity::AwaitingApproval { .. })
    ));
    let after_first = notices.load(Ordering::Acquire);
    harness
        .orchestrator
        .reconcile_approvals(&[inventory(CordisRunStatus::AwaitingApproval, true)]);
    assert_eq!(notices.load(Ordering::Acquire), after_first);
    harness.orchestrator.reconcile_approvals(&[]);
    assert!(harness.orchestrator.active_runs().is_empty());

    harness
        .orchestrator
        .reconcile_approvals(&[inventory(CordisRunStatus::StartingHost, false)]);
    eventually(
        || !harness.host.resolutions.lock().is_empty(),
        "automatic reconciled request did not settle",
    )
    .await;
    assert!(harness.orchestrator.active_runs().is_empty());
    assert_eq!(harness.host.run_calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn approval_free_open_detaches_work_and_clears_a_previous_failure_on_retry() {
    let harness = harness(None);
    *harness.host.run_result.lock() = DynamicCordisHostHalfResult::Failure(CordisErrorDetails {
        message: "first failed".to_owned(),
        stack: None,
    });
    harness.orchestrator.start_user_run(user(true)).await;
    assert!(!harness.orchestrator.last_run_error().is_empty());
    *harness.host.run_result.lock() = DynamicCordisHostHalfResult::Success {
        plugin_id: plugin(),
        package_id: package(),
        plugin_run_id: run_id(),
        waiting_for: Vec::new(),
        started_here: true,
    };
    harness.orchestrator.open(request(false));
    assert!(harness.orchestrator.last_run_error().is_empty());
    eventually(
        || !harness.host.resolutions.lock().is_empty(),
        "approval-free request did not answer",
    )
    .await;
    assert!(harness.orchestrator.active_runs().is_empty());
}
