//! Ordered registry, monotonic identities, and pending-request claims.

use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::PluginFiber;
use seekdeep_llm::SessionId;
use seekdeep_loader::DynamicHostRuntime;

use crate::{
    ApprovalRequestId, CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisActiveRun, DynamicCordisDefinition, DynamicCordisInventoryPackage,
    DynamicCordisPendingRequest, DynamicCordisRenderFailure, DynamicCordisRunAttempt,
};

/// Create a plugin or append to an existing plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicCordisPluginSelector {
    /// Create a new stable plugin from a semantic prefix.
    New {
        /// Model-supplied lowercase semantic prefix.
        id_prefix: String,
    },
    /// Append a version to an existing plugin.
    Existing {
        /// Stable plugin identity.
        plugin_id: CordisDynamicPluginId,
    },
}

/// Host and Client async-function bodies supplied at define time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DynamicCordisCode {
    /// Host body.
    pub host: Option<String>,
    /// Client body.
    pub client: Option<String>,
}

/// Validated input to one definition operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisDefineRequest {
    /// Session that owns the plugin.
    pub session_id: SessionId,
    /// New or existing plugin selection.
    pub plugin: DynamicCordisPluginSelector,
    /// Package label.
    pub name: String,
    /// User-facing purpose.
    pub purpose: String,
    /// At least one source half.
    pub code: DynamicCordisCode,
}

/// Successful definition receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisDefineReceipt {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// New immutable package identity.
    pub package_id: CordisDynamicPackageId,
    /// Trimmed package label.
    pub name: String,
    /// Trimmed purpose.
    pub purpose: String,
    /// Whether Host code exists.
    pub has_host_half: bool,
    /// Whether Client code exists.
    pub has_client_half: bool,
}

/// Source-free modification context for a plugin reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisReference {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Preferred package identity.
    pub package_id: CordisDynamicPackageId,
    /// Package label.
    pub name: String,
    /// User-facing purpose.
    pub purpose: String,
    /// Last successful package.
    pub current_package_id: Option<CordisDynamicPackageId>,
    /// Selected transition target.
    pub next_package_id: Option<CordisDynamicPackageId>,
    /// Current activation.
    pub active_run: Option<DynamicCordisActiveRun>,
    /// Latest attempt.
    pub latest_run: Option<DynamicCordisRunAttempt>,
}

/// Source-free plugin summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisPluginInspection {
    /// Preferred modification reference.
    pub reference: DynamicCordisReference,
    /// All immutable package summaries.
    pub packages: Vec<crate::DynamicCordisInventoryPackage>,
}

/// Exact immutable package metadata and source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisPackageInspection {
    /// Lifecycle reference using the selected package.
    pub reference: DynamicCordisReference,
    /// Host and Client code.
    pub code: DynamicCordisCode,
}

/// Mutable state of one stable dynamic plugin.
#[derive(Debug)]
pub struct DynamicCordisPluginState {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Owning session.
    pub session_id: SessionId,
    /// Immutable versions in definition order.
    pub packages: IndexMap<CordisDynamicPackageId, DynamicCordisDefinition>,
    /// Client-bearing versions authorized individually.
    pub approved_client_packages: std::collections::BTreeSet<CordisDynamicPackageId>,
    /// Whether one approval authorized later versions.
    pub client_version_updates_approved: bool,
    /// Last version that completed activation.
    pub current_package_id: Option<CordisDynamicPackageId>,
    /// Failed or in-progress target version.
    pub next_package_id: Option<CordisDynamicPackageId>,
    /// Current physical activation.
    pub active_run: Option<DynamicCordisActiveRun>,
    /// Physical Host activation and its lifecycle owner.
    pub run: Option<DynamicCordisPhysicalRun>,
    /// Latest attempt, including failures and pending approval.
    pub latest_run: Option<DynamicCordisRunAttempt>,
}

impl DynamicCordisPluginState {
    /// Creates one plugin with no versions or run state.
    #[must_use]
    pub fn new(plugin_id: CordisDynamicPluginId, session_id: SessionId) -> Self {
        Self {
            plugin_id,
            session_id,
            packages: IndexMap::new(),
            approved_client_packages: std::collections::BTreeSet::new(),
            client_version_updates_approved: false,
            current_package_id: None,
            next_package_id: None,
            active_run: None,
            run: None,
            latest_run: None,
        }
    }
}

/// One live activation and its Host lifecycle owner.
#[derive(Debug)]
pub struct DynamicCordisPhysicalRun {
    /// Exact activation identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Active package identity.
    pub package_id: CordisDynamicPackageId,
    /// Host fiber, absent for Client-only packages.
    pub fiber: Option<Arc<PluginFiber>>,
    /// Rust-owned interpreter bridge for registered Host methods.
    pub host_runtime: Option<Arc<DynamicHostRuntime>>,
    /// Last Client render failure observed for this exact activation.
    pub render_failure: Option<DynamicCordisRenderFailure>,
    /// Runtime failures already surfaced for this activation.
    pub reported_runtime_errors: std::collections::BTreeSet<String>,
    /// Approval request that started this run.
    pub started_for_request: Option<ApprovalRequestId>,
}

/// Host-rich active-run state consumed by inspection and result rendering.
#[derive(Clone, Debug)]
pub struct DynamicCordisSnapshotActiveRun {
    /// Exact activation identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Active immutable package.
    pub package_id: CordisDynamicPackageId,
    /// Host lifecycle owner, absent for Client-only packages.
    pub fiber: Option<Arc<PluginFiber>>,
    /// Registered Host method names.
    pub handlers: Vec<String>,
    /// Last Client render failure for this activation.
    pub render_failure: Option<DynamicCordisRenderFailure>,
}

/// One Session-owned Host-rich plugin snapshot.
#[derive(Clone, Debug)]
pub struct DynamicCordisSnapshotRow {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Last fully successful package.
    pub current_package_id: Option<CordisDynamicPackageId>,
    /// Failed or in-progress target package.
    pub next_package_id: Option<CordisDynamicPackageId>,
    /// Immutable packages in define order.
    pub packages: Vec<DynamicCordisInventoryPackage>,
    /// Current physical activation and Host-only details.
    pub active_run: Option<DynamicCordisSnapshotActiveRun>,
    /// Latest activation attempt.
    pub latest_run: Option<DynamicCordisRunAttempt>,
}

#[derive(Default)]
struct RegistryState {
    plugins: IndexMap<CordisDynamicPluginId, Arc<Mutex<DynamicCordisPluginState>>>,
    pending: IndexMap<ApprovalRequestId, DynamicCordisPendingRequest>,
    next_plugin: u64,
    next_package: u64,
    next_run: u64,
    next_approval: u64,
}

/// Thread-safe process-local registry.
#[derive(Default)]
pub struct DynamicCordisRegistry {
    state: Mutex<RegistryState>,
}

impl std::fmt::Debug for DynamicCordisRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("DynamicCordisRegistry")
            .field("plugins", &state.plugins.len())
            .field("pending", &state.pending.len())
            .finish()
    }
}

impl DynamicCordisRegistry {
    /// Creates an empty process-local registry.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mints a semantic plugin id without reusing prior suffixes.
    #[must_use]
    pub fn mint_plugin_id(&self, prefix: &str) -> CordisDynamicPluginId {
        let mut state = self.state.lock();
        loop {
            state.next_plugin += 1;
            let id = CordisDynamicPluginId::new(format!("{prefix}-{}", state.next_plugin));
            if !state.plugins.contains_key(&id) {
                return id;
            }
        }
    }

    /// Mints a process-unique immutable package id.
    #[must_use]
    pub fn mint_package_id(&self) -> CordisDynamicPackageId {
        let mut state = self.state.lock();
        state.next_package += 1;
        CordisDynamicPackageId::new(format!("pkg-{}", state.next_package))
    }

    /// Mints a process-unique activation id.
    #[must_use]
    pub fn mint_plugin_run_id(&self) -> CordisDynamicPluginRunId {
        let mut state = self.state.lock();
        state.next_run += 1;
        CordisDynamicPluginRunId::new(format!("run-{}", state.next_run))
    }

    /// Mints a process-unique approval request id.
    #[must_use]
    pub fn mint_approval_request_id(&self) -> ApprovalRequestId {
        let mut state = self.state.lock();
        state.next_approval += 1;
        ApprovalRequestId::new(format!("approval-{}", state.next_approval))
    }

    /// Adds or replaces one stable plugin record.
    pub fn add(&self, plugin: DynamicCordisPluginState) {
        self.state
            .lock()
            .plugins
            .insert(plugin.plugin_id.clone(), Arc::new(Mutex::new(plugin)));
    }

    /// Reads one stable plugin record.
    #[must_use]
    pub fn get(&self, id: &CordisDynamicPluginId) -> Option<Arc<Mutex<DynamicCordisPluginState>>> {
        self.state.lock().plugins.get(id).cloned()
    }

    /// Deletes one plugin and every immutable version it owns.
    pub fn delete(&self, id: &CordisDynamicPluginId) -> bool {
        self.state.lock().plugins.shift_remove(id).is_some()
    }

    /// Snapshots all plugins in creation order.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<Mutex<DynamicCordisPluginState>>> {
        self.state.lock().plugins.values().cloned().collect()
    }

    /// Snapshots one session's plugins in creation order.
    #[must_use]
    pub fn of_session(&self, session_id: &SessionId) -> Vec<Arc<Mutex<DynamicCordisPluginState>>> {
        self.state
            .lock()
            .plugins
            .values()
            .filter(|plugin| plugin.lock().session_id == *session_id)
            .cloned()
            .collect()
    }

    /// Publishes one pending approval request.
    pub fn arm_request(&self, id: ApprovalRequestId, request: DynamicCordisPendingRequest) {
        self.state.lock().pending.insert(id, request);
    }

    /// Reads a pending request without claiming it.
    #[must_use]
    pub fn peek_request(&self, id: &ApprovalRequestId) -> Option<DynamicCordisPendingRequest> {
        self.state.lock().pending.get(id).cloned()
    }

    /// Claims a pending request. The first caller removes and owns it.
    #[must_use]
    pub fn claim_request(&self, id: &ApprovalRequestId) -> Option<DynamicCordisPendingRequest> {
        self.state.lock().pending.shift_remove(id)
    }

    /// Cancels one pending request.
    pub fn disarm_request(&self, id: &ApprovalRequestId) {
        self.state.lock().pending.shift_remove(id);
    }

    /// Finds the pending request for one plugin.
    #[must_use]
    pub fn pending_request_for(
        &self,
        plugin_id: &CordisDynamicPluginId,
    ) -> Option<ApprovalRequestId> {
        self.state
            .lock()
            .pending
            .iter()
            .find(|(_, request)| request.plugin_id == *plugin_id)
            .map(|(id, _)| id.clone())
    }
}
