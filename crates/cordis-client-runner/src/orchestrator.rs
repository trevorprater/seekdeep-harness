//! Plugin-keyed approval and Host-to-Client activation orchestration.

use std::{collections::BTreeMap, sync::Arc};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{
    ApprovalRequestId, CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    CordisErrorDetails, CordisRunStatus, DynamicCordisClientSource, DynamicCordisHostHalfResult,
    DynamicCordisInventoryRow, DynamicCordisResolveAck, DynamicCordisRunFailureReason,
    DynamicCordisRunMode, DynamicCordisRunResolution, DynamicCordisRunResponse,
};
use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize};

use crate::ClientMountRejection;

/// One Plugin's page-side approval or activation activity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CordisRunActivity {
    /// Waiting for an explicit user decision.
    AwaitingApproval {
        /// Exact approval request.
        request_id: ApprovalRequestId,
        /// Owning Session.
        agent_id: SessionId,
        /// Target immutable Package.
        package_id: CordisDynamicPackageId,
        /// Run or update intent.
        mode: DynamicCordisRunMode,
        /// Package label.
        name: String,
        /// User-facing purpose.
        purpose: String,
    },
    /// Host/Client activation is in flight.
    Orchestrating {
        /// Owning Session.
        agent_id: SessionId,
        /// Target immutable Package.
        package_id: CordisDynamicPackageId,
        /// Run or update intent.
        mode: DynamicCordisRunMode,
    },
}

/// Why this page's latest activation attempt failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisRunFailure {
    /// Target immutable Package.
    pub package_id: CordisDynamicPackageId,
    /// Failed half.
    pub reason: CordisPageFailureReason,
    /// Actionable failure text.
    pub message: String,
    /// Original stack when present.
    pub stack: Option<String>,
}

/// Page-side activation failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CordisPageFailureReason {
    /// Host half could not start.
    HostHalfFailed,
    /// Client source, evaluation, import, or activation failed.
    ClientHalfFailed,
}

/// Forwarded model activation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisRunRequest {
    /// Exact approval request.
    pub request_id: ApprovalRequestId,
    /// Owning Session.
    pub agent_id: SessionId,
    /// Stable Plugin.
    pub plugin_id: CordisDynamicPluginId,
    /// Target Package.
    pub package_id: CordisDynamicPackageId,
    /// Run or update intent.
    pub mode: DynamicCordisRunMode,
    /// Package label.
    pub name: String,
    /// User-facing purpose.
    pub purpose: String,
    /// Whether this page must ask the user.
    pub requires_approval: bool,
}

/// Direct panel activation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisUserRunRequest {
    /// Owning Session.
    pub agent_id: SessionId,
    /// Stable Plugin.
    pub plugin_id: CordisDynamicPluginId,
    /// Target Package.
    pub package_id: CordisDynamicPackageId,
    /// Run or update intent.
    pub mode: DynamicCordisRunMode,
    /// Whether this Package requires Client loading.
    pub has_client_half: bool,
}

#[derive(Clone, Debug)]
struct RunPlan {
    user: CordisUserRunRequest,
    request_id: Option<ApprovalRequestId>,
    approve_future_versions: bool,
}

/// Browser load stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientLoadErrorCause {
    /// Closure evaluation failed.
    Evaluate,
    /// Module-table handoff failed.
    ModuleImport,
    /// Cordis activation failed.
    Activate,
}

impl ClientLoadErrorCause {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Evaluate => "evaluate",
            Self::ModuleImport => "module-import",
            Self::Activate => "activate",
        }
    }
}

/// Page loader result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientLoadResult {
    /// Client Fiber exists, possibly parked on Services.
    Success {
        /// Exact activation.
        plugin_run_id: CordisDynamicPluginRunId,
        /// Missing Client Services when parked.
        waiting_for: Option<Vec<String>>,
    },
    /// Evaluation, import, or activation failure.
    Failure {
        /// Failing stage.
        cause: ClientLoadErrorCause,
        /// Original error fields.
        error: CordisErrorDetails,
    },
}

/// Browser-half source passed to the page loader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientLoadRequest {
    /// Stable Plugin.
    pub plugin_id: CordisDynamicPluginId,
    /// Immutable Package.
    pub package_id: CordisDynamicPackageId,
    /// Exact activation.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Owning Session.
    pub agent_id: SessionId,
    /// Package label.
    pub name: String,
    /// Browser JavaScript body.
    pub code: String,
}

/// Page-local Client package loader.
pub trait DynamicCordisPackageRunner: Send + Sync + 'static {
    /// Loads or converges one exact activation.
    fn load(
        &self,
        request: ClientLoadRequest,
    ) -> BoxFuture<'static, Result<ClientLoadResult, ClientMountRejection>>;
}

/// Folded Host RPC operations consumed by the orchestrator.
pub trait CordisRunHostSeam: Send + Sync + 'static {
    /// Starts or attaches to the Host half.
    fn run_host_half(
        &self,
        plan: CordisUserRunRequest,
        request_id: Option<ApprovalRequestId>,
        approve_future_versions: bool,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisHostHalfResult>>;
    /// Fetches Client source for one exact Host activation.
    fn get_client_code(
        &self,
        agent_id: SessionId,
        plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisClientSource>>;
    /// Settles a model-driven request.
    fn resolve_request_run(
        &self,
        request_id: ApprovalRequestId,
        resolution: DynamicCordisRunResolution,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisResolveAck>>;
    /// Settles a direct panel run.
    fn settle_user_run(
        &self,
        agent_id: SessionId,
        plugin_id: CordisDynamicPluginId,
        resolution: DynamicCordisRunResolution,
    ) -> BoxFuture<'static, anyhow::Result<DynamicCordisRunResponse>>;
}

/// Injected executor for automatic approval-free requests.
pub trait ClientTaskSpawner: Send + Sync + 'static {
    /// Drives one detached orchestrator future.
    fn spawn(&self, future: BoxFuture<'static, ()>);
}

/// Browser-console diagnostic emitted by orchestration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientOrchestratorLog {
    /// Client source, evaluation, import, or activation failed.
    ClientActivationFailed {
        /// Stable Plugin.
        plugin_id: CordisDynamicPluginId,
        /// Target Package.
        package_id: CordisDynamicPackageId,
        /// Exact activation.
        plugin_run_id: CordisDynamicPluginRunId,
        /// Original failure text.
        message: String,
    },
    /// Host settlement transport rejected after local convergence.
    AnswerFailed {
        /// Exact request.
        request_id: ApprovalRequestId,
        /// Transport failure.
        message: String,
    },
}

/// Injected browser log sink.
pub type ClientOrchestratorLogger = Arc<dyn Fn(ClientOrchestratorLog) + Send + Sync>;

type SharedRun = Shared<BoxFuture<'static, ()>>;
type Listener = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct OrchestratorState {
    requests: BTreeMap<ApprovalRequestId, CordisRunRequest>,
    activity: BTreeMap<CordisDynamicPluginId, CordisRunActivity>,
    failures: BTreeMap<CordisDynamicPluginId, CordisRunFailure>,
    in_flight: BTreeMap<CordisDynamicPluginId, SharedRun>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
    activity_cache: Option<Arc<BTreeMap<CordisDynamicPluginId, CordisRunActivity>>>,
    failure_cache: Option<Arc<BTreeMap<CordisDynamicPluginId, CordisRunFailure>>>,
}

/// Drives Host → Client activation and publishes Plugin-keyed state.
pub struct CordisRunOrchestrator {
    runner: Arc<dyn DynamicCordisPackageRunner>,
    host: Arc<dyn CordisRunHostSeam>,
    spawner: Arc<dyn ClientTaskSpawner>,
    logger: ClientOrchestratorLogger,
    state: Mutex<OrchestratorState>,
}

impl std::fmt::Debug for CordisRunOrchestrator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("CordisRunOrchestrator")
            .field("requests", &state.requests.len())
            .field("activity", &state.activity.len())
            .field("failures", &state.failures.len())
            .field("in_flight", &state.in_flight.len())
            .finish_non_exhaustive()
    }
}

impl CordisRunOrchestrator {
    /// Creates one page-global orchestrator.
    #[must_use]
    pub fn new(
        runner: Arc<dyn DynamicCordisPackageRunner>,
        host: Arc<dyn CordisRunHostSeam>,
        spawner: Arc<dyn ClientTaskSpawner>,
    ) -> Arc<Self> {
        Self::new_with_logger(runner, host, spawner, Arc::new(|_| {}))
    }

    /// Creates one page-global orchestrator with an explicit diagnostic sink.
    #[must_use]
    pub fn new_with_logger(
        runner: Arc<dyn DynamicCordisPackageRunner>,
        host: Arc<dyn CordisRunHostSeam>,
        spawner: Arc<dyn ClientTaskSpawner>,
        logger: ClientOrchestratorLogger,
    ) -> Arc<Self> {
        Arc::new(Self {
            runner,
            host,
            spawner,
            logger,
            state: Mutex::new(OrchestratorState::default()),
        })
    }

    /// Stable activity snapshot reference until the next mutation.
    #[must_use]
    pub fn active_runs(&self) -> Arc<BTreeMap<CordisDynamicPluginId, CordisRunActivity>> {
        let mut state = self.state.lock();
        if let Some(snapshot) = &state.activity_cache {
            return snapshot.clone();
        }
        let snapshot = Arc::new(state.activity.clone());
        state.activity_cache = Some(snapshot.clone());
        snapshot
    }

    /// Stable failure snapshot reference until the next mutation.
    #[must_use]
    pub fn last_run_error(&self) -> Arc<BTreeMap<CordisDynamicPluginId, CordisRunFailure>> {
        let mut state = self.state.lock();
        if let Some(snapshot) = &state.failure_cache {
            return snapshot.clone();
        }
        let snapshot = Arc::new(state.failures.clone());
        state.failure_cache = Some(snapshot.clone());
        snapshot
    }

    /// Subscribes to every committed activity or failure mutation.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, listener: Listener) -> OrchestratorSubscription {
        let id = {
            let mut state = self.state.lock();
            state.next_listener += 1;
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        OrchestratorSubscription {
            orchestrator: Arc::downgrade(self),
            id,
        }
    }

    /// Opens one forwarded request and starts approval-free work immediately.
    pub fn open(self: &Arc<Self>, request: CordisRunRequest) {
        self.state
            .lock()
            .requests
            .insert(request.request_id.clone(), request.clone());
        if !request.requires_approval {
            let future = self.orchestrate(RunPlan {
                user: CordisUserRunRequest {
                    agent_id: request.agent_id,
                    plugin_id: request.plugin_id,
                    package_id: request.package_id,
                    mode: request.mode,
                    has_client_half: true,
                },
                request_id: Some(request.request_id),
                approve_future_versions: false,
            });
            self.spawner.spawn(Box::pin(future));
            return;
        }
        {
            let mut state = self.state.lock();
            if !matches!(
                state.activity.get(&request.plugin_id),
                Some(CordisRunActivity::Orchestrating { .. })
            ) {
                state.activity.insert(
                    request.plugin_id,
                    CordisRunActivity::AwaitingApproval {
                        request_id: request.request_id,
                        agent_id: request.agent_id,
                        package_id: request.package_id,
                        mode: request.mode,
                        name: request.name,
                        purpose: request.purpose,
                    },
                );
            }
        }
        self.commit();
    }

    /// Reconstructs pending approval state from authoritative Host inventory.
    pub fn reconcile_approvals(self: &Arc<Self>, rows: &[DynamicCordisInventoryRow]) {
        let expected = expected_requests(rows);
        let mut changed = false;
        let prior = self.state.lock().requests.clone();
        for (request_id, request) in prior {
            if expected.contains_key(&request_id) {
                continue;
            }
            let mut state = self.state.lock();
            state.requests.remove(&request_id);
            if matches!(
                state.activity.get(&request.plugin_id),
                Some(CordisRunActivity::AwaitingApproval { request_id: current, .. }) if *current == request_id
            ) {
                state.activity.remove(&request.plugin_id);
            }
            changed = true;
        }
        for (request_id, request) in expected {
            let current = self.state.lock().activity.get(&request.plugin_id).cloned();
            if !request.requires_approval
                && matches!(current, Some(CordisRunActivity::Orchestrating { .. }))
            {
                continue;
            }
            if request.requires_approval
                && self.state.lock().requests.get(&request_id) == Some(&request)
                && matches!(current, Some(CordisRunActivity::AwaitingApproval { request_id: ref current, .. }) if *current == request_id)
            {
                continue;
            }
            if !request.requires_approval {
                self.open(request);
                changed = true;
                continue;
            }
            let mut state = self.state.lock();
            state.requests.insert(request_id.clone(), request.clone());
            if !matches!(current, Some(CordisRunActivity::Orchestrating { .. })) {
                state.activity.insert(
                    request.plugin_id,
                    CordisRunActivity::AwaitingApproval {
                        request_id,
                        agent_id: request.agent_id,
                        package_id: request.package_id,
                        mode: request.mode,
                        name: request.name,
                        purpose: request.purpose,
                    },
                );
            }
            changed = true;
        }
        if changed {
            self.commit();
        }
    }

    /// Closes a request settled elsewhere.
    pub fn close(&self, request_id: &ApprovalRequestId) {
        let changed = {
            let mut state = self.state.lock();
            let Some(request) = state.requests.remove(request_id) else {
                return;
            };
            if matches!(
                state.activity.get(&request.plugin_id),
                Some(CordisRunActivity::AwaitingApproval { request_id: current, .. }) if current == request_id
            ) {
                state.activity.remove(&request.plugin_id);
            }
            true
        };
        if changed {
            self.commit();
        }
    }

    /// Approves one still-open explicit request.
    pub fn approve(
        self: &Arc<Self>,
        request_id: &ApprovalRequestId,
        approve_future_versions: bool,
    ) -> SharedRun {
        let request = self.state.lock().requests.get(request_id).cloned();
        let Some(request) = request.filter(|request| request.requires_approval) else {
            return futures::future::ready(()).boxed().shared();
        };
        self.orchestrate(RunPlan {
            user: CordisUserRunRequest {
                agent_id: request.agent_id,
                plugin_id: request.plugin_id,
                package_id: request.package_id,
                mode: request.mode,
                has_client_half: true,
            },
            request_id: Some(request.request_id),
            approve_future_versions,
        })
    }

    /// Rejects one still-waiting explicit request without starting either half.
    pub async fn decline(&self, request_id: &ApprovalRequestId) {
        let request = {
            let mut state = self.state.lock();
            let Some(request) = state.requests.get(request_id).cloned() else {
                return;
            };
            if !request.requires_approval
                || !matches!(
                    state.activity.get(&request.plugin_id),
                    Some(CordisRunActivity::AwaitingApproval { request_id: current, .. }) if current == request_id
                )
            {
                return;
            }
            state.requests.remove(request_id);
            state.activity.remove(&request.plugin_id);
            request
        };
        self.commit();
        self.answer(
            request.request_id,
            DynamicCordisRunResolution::Failure {
                reason: DynamicCordisRunFailureReason::Rejected,
                plugin_run_id: None,
                started_here: None,
                message: None,
                stack: None,
            },
        )
        .await;
    }

    /// Starts one direct panel run; the gesture itself authorizes it.
    pub fn start_user_run(self: &Arc<Self>, request: CordisUserRunRequest) -> SharedRun {
        self.orchestrate(RunPlan {
            user: request,
            request_id: None,
            approve_future_versions: false,
        })
    }

    fn orchestrate(self: &Arc<Self>, plan: RunPlan) -> SharedRun {
        if let Some(running) = self
            .state
            .lock()
            .in_flight
            .get(&plan.user.plugin_id)
            .cloned()
        {
            return running;
        }
        {
            let mut state = self.state.lock();
            state.activity.insert(
                plan.user.plugin_id.clone(),
                CordisRunActivity::Orchestrating {
                    agent_id: plan.user.agent_id.clone(),
                    package_id: plan.user.package_id.clone(),
                    mode: plan.user.mode,
                },
            );
            state.failures.remove(&plan.user.plugin_id);
            if let Some(request_id) = &plan.request_id {
                state.requests.remove(request_id);
            }
        }
        self.commit();
        let orchestrator = self.clone();
        let plugin_id = plan.user.plugin_id.clone();
        let completion_id = plugin_id.clone();
        let attempt = async move {
            orchestrator.drive(plan).await;
            {
                let mut state = orchestrator.state.lock();
                state.in_flight.remove(&completion_id);
                state.activity.remove(&completion_id);
            }
            orchestrator.commit();
        }
        .boxed()
        .shared();
        self.state
            .lock()
            .in_flight
            .insert(plugin_id, attempt.clone());
        attempt
    }

    async fn drive(&self, plan: RunPlan) {
        let started = self.start_host(&plan).await;
        let (plugin_run_id, started_here) = match started {
            DynamicCordisHostHalfResult::Success {
                plugin_run_id,
                started_here,
                ..
            } => (plugin_run_id, started_here),
            DynamicCordisHostHalfResult::Failure(error) => {
                self.fail(&plan.user, CordisPageFailureReason::HostHalfFailed, &error);
                if let Some(request_id) = plan.request_id {
                    self.answer(
                        request_id,
                        DynamicCordisRunResolution::Failure {
                            reason: DynamicCordisRunFailureReason::HostHalfFailed,
                            plugin_run_id: None,
                            started_here: None,
                            message: Some(error.message),
                            stack: error.stack,
                        },
                    )
                    .await;
                }
                return;
            }
        };
        if !plan.user.has_client_half {
            return;
        }
        let source = match self
            .host
            .get_client_code(
                plan.user.agent_id.clone(),
                plan.user.plugin_id.clone(),
                plugin_run_id.clone(),
            )
            .await
        {
            Ok(source) => source,
            Err(error) => {
                self.finish_client_failure(
                    plan,
                    plugin_run_id,
                    started_here,
                    CordisErrorDetails {
                        message: error.to_string(),
                        stack: None,
                    },
                )
                .await;
                return;
            }
        };
        let loaded = self
            .runner
            .load(ClientLoadRequest {
                plugin_id: source.plugin_id,
                package_id: source.package_id,
                plugin_run_id: source.plugin_run_id,
                agent_id: plan.user.agent_id.clone(),
                name: source.name,
                code: source.code,
            })
            .await
            .unwrap_or_else(|error| ClientLoadResult::Failure {
                cause: ClientLoadErrorCause::Evaluate,
                error: CordisErrorDetails {
                    message: error.message,
                    stack: error.stack,
                },
            });
        let resolution = match loaded {
            ClientLoadResult::Success {
                plugin_run_id,
                waiting_for,
            } => DynamicCordisRunResolution::Success {
                plugin_run_id,
                waiting_for,
            },
            ClientLoadResult::Failure { cause, error } => {
                self.finish_client_failure(
                    plan,
                    plugin_run_id,
                    started_here,
                    CordisErrorDetails {
                        message: format!("{}: {}", cause.as_str(), error.message),
                        stack: error.stack,
                    },
                )
                .await;
                return;
            }
        };
        if let Some(request_id) = plan.request_id {
            self.answer(request_id, resolution).await;
        } else {
            self.settle_direct(&plan.user, resolution).await;
        }
    }

    async fn start_host(&self, plan: &RunPlan) -> DynamicCordisHostHalfResult {
        self.host
            .run_host_half(
                plan.user.clone(),
                plan.request_id.clone(),
                plan.approve_future_versions,
            )
            .await
            .unwrap_or_else(|error| {
                DynamicCordisHostHalfResult::Failure(CordisErrorDetails {
                    message: error.to_string(),
                    stack: None,
                })
            })
    }

    async fn finish_client_failure(
        &self,
        plan: RunPlan,
        plugin_run_id: CordisDynamicPluginRunId,
        started_here: bool,
        failure: CordisErrorDetails,
    ) {
        (self.logger)(ClientOrchestratorLog::ClientActivationFailed {
            plugin_id: plan.user.plugin_id.clone(),
            package_id: plan.user.package_id.clone(),
            plugin_run_id: plugin_run_id.clone(),
            message: failure.message.clone(),
        });
        self.fail(
            &plan.user,
            CordisPageFailureReason::ClientHalfFailed,
            &failure,
        );
        let resolution = DynamicCordisRunResolution::Failure {
            reason: DynamicCordisRunFailureReason::ClientHalfFailed,
            plugin_run_id: Some(plugin_run_id),
            started_here: Some(started_here),
            message: Some(failure.message),
            stack: failure.stack,
        };
        if let Some(request_id) = plan.request_id {
            self.answer(request_id, resolution).await;
        } else {
            self.settle_direct(&plan.user, resolution).await;
        }
    }

    async fn settle_direct(
        &self,
        plan: &CordisUserRunRequest,
        resolution: DynamicCordisRunResolution,
    ) {
        match self
            .host
            .settle_user_run(plan.agent_id.clone(), plan.plugin_id.clone(), resolution)
            .await
        {
            Ok(response @ DynamicCordisRunResponse::Failure { .. }) => {
                self.fail_from_response(plan, &response);
            }
            Err(error) => self.fail(
                plan,
                CordisPageFailureReason::ClientHalfFailed,
                &CordisErrorDetails {
                    message: error.to_string(),
                    stack: None,
                },
            ),
            Ok(DynamicCordisRunResponse::Success { .. }) => {}
        }
    }

    async fn answer(&self, request_id: ApprovalRequestId, resolution: DynamicCordisRunResolution) {
        if let Err(error) = self
            .host
            .resolve_request_run(request_id.clone(), resolution)
            .await
        {
            (self.logger)(ClientOrchestratorLog::AnswerFailed {
                request_id,
                message: error.to_string(),
            });
        }
    }

    fn fail(
        &self,
        plan: &CordisUserRunRequest,
        reason: CordisPageFailureReason,
        failure: &CordisErrorDetails,
    ) {
        self.state.lock().failures.insert(
            plan.plugin_id.clone(),
            CordisRunFailure {
                package_id: plan.package_id.clone(),
                reason,
                message: failure.message.clone(),
                stack: failure.stack.clone(),
            },
        );
        self.commit();
    }

    fn fail_from_response(&self, plan: &CordisUserRunRequest, response: &DynamicCordisRunResponse) {
        if let DynamicCordisRunResponse::Failure { message, stack, .. } = response {
            self.fail(
                plan,
                CordisPageFailureReason::ClientHalfFailed,
                &CordisErrorDetails {
                    message: message.clone(),
                    stack: stack.clone(),
                },
            );
        }
    }

    fn commit(&self) {
        let listeners = {
            let mut state = self.state.lock();
            state.activity_cache = None;
            state.failure_cache = None;
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }
}

/// Idempotent activity/failure subscription disposer.
pub struct OrchestratorSubscription {
    orchestrator: std::sync::Weak<CordisRunOrchestrator>,
    id: u64,
}

impl OrchestratorSubscription {
    /// Stops notifications.
    pub fn dispose(&self) {
        if let Some(orchestrator) = self.orchestrator.upgrade() {
            orchestrator.state.lock().listeners.remove(&self.id);
        }
    }
}

fn expected_requests(
    rows: &[DynamicCordisInventoryRow],
) -> BTreeMap<ApprovalRequestId, CordisRunRequest> {
    let mut expected = BTreeMap::new();
    for row in rows {
        let Some(attempt) = &row.latest_run else {
            continue;
        };
        let Some(request_id) = &attempt.approval_request_id else {
            continue;
        };
        if !matches!(
            attempt.status,
            CordisRunStatus::AwaitingApproval
                | CordisRunStatus::StartingHost
                | CordisRunStatus::ClientPending
        ) {
            continue;
        }
        let Some(package) = row
            .packages
            .iter()
            .find(|package| package.package_id == attempt.package_id)
        else {
            continue;
        };
        expected.insert(
            request_id.clone(),
            CordisRunRequest {
                request_id: request_id.clone(),
                agent_id: row.agent_id.clone(),
                plugin_id: row.plugin_id.clone(),
                package_id: attempt.package_id.clone(),
                mode: attempt.mode,
                name: package.name.clone(),
                purpose: package.purpose.clone(),
                requires_approval: attempt
                    .requires_approval
                    .unwrap_or(attempt.status == CordisRunStatus::AwaitingApproval),
            },
        );
    }
    expected
}
