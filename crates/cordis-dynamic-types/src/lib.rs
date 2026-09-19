//! Client-safe wire vocabulary for dynamic Cordis packages.

use std::fmt;

use seekdeep_identity::SessionId;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

macro_rules! string_id {
    ($name:ident, $docs:literal) => {
        #[doc = $docs]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps an exact wire identity.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the wire spelling.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(
    CordisDynamicPluginId,
    "Stable identity of one dynamic plugin."
);
string_id!(
    CordisDynamicPackageId,
    "Identity of one immutable package version."
);
string_id!(
    CordisDynamicPluginRunId,
    "Identity of one activation attempt."
);
string_id!(ApprovalRequestId, "Identity of one approval request.");
string_id!(CordisInspectRequestId, "Identity of one inspect query.");

/// Runtime plane owning an inspect provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CordisInspectPlatform {
    /// Native Host process.
    Host,
    /// Browser Client runtime.
    Client,
}

/// Whether an activation starts or replaces a version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DynamicCordisRunMode {
    /// Start a stopped package.
    Run,
    /// Replace the current version.
    Update,
}

/// How a Client activation request settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestRunOutcome {
    /// User approval admitted the request.
    Approved,
    /// Activation completed.
    Completed,
    /// User rejected the request.
    Rejected,
    /// Owner cancellation won.
    Cancelled,
    /// Activation failed.
    Failed,
}

/// Persisted state of the latest activation attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CordisRunStatus {
    /// Waiting for a user decision.
    AwaitingApproval,
    /// Host activation is starting.
    StartingHost,
    /// Host is ready and a Client is needed.
    ClientPending,
    /// Both required halves are running.
    Running,
    /// A valid fiber is waiting for services.
    Waiting,
    /// User rejected approval.
    Rejected,
    /// Activation failed.
    Failed,
    /// Owner cancellation won.
    Cancelled,
    /// Activation was stopped.
    Stopped,
}

/// Lifecycle status of one platform half.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CordisHalfStatus {
    /// Package has no such half.
    Absent,
    /// Activation is in progress.
    Pending,
    /// Half is not running.
    Stopped,
    /// Half is active.
    Running,
    /// Half waits for declared services.
    Waiting,
    /// Half failed.
    Failed,
}

/// Stage associated with an activation diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CordisDiagnosticPhase {
    /// Approval failed.
    Approval,
    /// Host source failed to load.
    HostLoad,
    /// Host plugin failed to apply.
    HostApply,
    /// Client source failed to load.
    ClientLoad,
    /// Client plugin failed to apply.
    ClientApply,
    /// Client component failed to render.
    ClientRender,
}

/// One platform half within an activation attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisHalfState {
    /// Lifecycle status.
    pub status: CordisHalfStatus,
    /// Declared services still absent.
    pub waiting_for: Vec<String>,
    /// Failure text when this half failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Structured failure associated with an exact activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisRunDiagnostic {
    /// Stage that failed.
    pub phase: CordisDiagnosticPhase,
    /// Failure message.
    pub message: String,
    /// Original stack when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Target package identity.
    pub package_id: CordisDynamicPackageId,
    /// Exact attempt identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
}

/// Latest attempt retained independently of the physical run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisRunAttempt {
    /// Exact attempt identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Target package.
    pub package_id: CordisDynamicPackageId,
    /// Run or update intent.
    pub mode: DynamicCordisRunMode,
    /// Current attempt state.
    pub status: CordisRunStatus,
    /// Pending Client request when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_request_id: Option<ApprovalRequestId>,
    /// Whether the pending request requires user approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_approval: Option<bool>,
    /// Host-half state.
    pub host: CordisHalfState,
    /// Client-half state.
    pub client: CordisHalfState,
    /// Latest failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CordisRunDiagnostic>,
}

/// One immutable package version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisDefinition {
    /// Immutable package identity.
    pub package_id: CordisDynamicPackageId,
    /// Human-readable label.
    pub name: String,
    /// User-facing purpose.
    pub purpose: String,
    /// Host async-function body.
    pub host_code: Option<String>,
    /// Client async-function body.
    pub client_code: Option<String>,
}

/// One suspended model-driven activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisPendingRequest {
    /// Session whose model requested activation.
    pub agent_id: SessionId,
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Target package identity.
    pub package_id: CordisDynamicPackageId,
    /// Exact attempt identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Run or update intent.
    pub mode: DynamicCordisRunMode,
    /// Whether a user decision is required.
    pub requires_approval: bool,
}

/// Package metadata exposed without source code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisInventoryPackage {
    /// Immutable package identity.
    pub package_id: CordisDynamicPackageId,
    /// Package label.
    pub name: String,
    /// User-facing purpose.
    pub purpose: String,
    /// Whether Host code exists.
    pub has_host_half: bool,
    /// Whether Client code exists.
    pub has_client_half: bool,
}

/// Physical activation identity exposed in inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisActiveRun {
    /// Exact attempt identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Active package identity.
    pub package_id: CordisDynamicPackageId,
}

/// One stable plugin row in the global inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisInventoryRow {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Owning session.
    pub agent_id: SessionId,
    /// Immutable versions in definition order.
    pub packages: Vec<DynamicCordisInventoryPackage>,
    /// Last successfully activated version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_package_id: Option<CordisDynamicPackageId>,
    /// Failed or in-progress target version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_package_id: Option<CordisDynamicPackageId>,
    /// Current physical activation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run: Option<DynamicCordisActiveRun>,
    /// Latest attempt and diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_run: Option<DynamicCordisRunAttempt>,
}

/// One model-callable inspect method manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisInspectMethodManifest {
    /// Method name unique within its provider.
    pub name: String,
    /// What the method returns and when to use it.
    pub description: String,
    /// JSON Schema accepted by the method.
    pub input_schema: Value,
    /// JSON Schema produced by the method.
    pub output_schema: Value,
}

/// Serializable directory entry for one inspect provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisInspectProviderManifest {
    /// Provider identity unique within one platform.
    pub id: String,
    /// Capability described by the provider.
    pub description: String,
    /// Explicit read-only queries.
    pub methods: Vec<CordisInspectMethodManifest>,
}

/// Provider directory row returned to callers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisInspectProviderView {
    /// Provider identity unique within one platform.
    pub id: String,
    /// Capability described by the provider.
    pub description: String,
    /// Explicit read-only queries.
    pub methods: Vec<CordisInspectMethodManifest>,
    /// Runtime plane executing the methods.
    pub platform: CordisInspectPlatform,
}

/// Host broadcast requesting one live Client inspect result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisInspectQueryRequest {
    /// Correlation identity.
    pub request_id: CordisInspectRequestId,
    /// Session whose model requested the query.
    pub agent_id: SessionId,
    /// Provider selected from the Client manifest.
    pub provider: String,
    /// Method selected from the provider manifest.
    pub method: String,
    /// Query input, omitted for a fieldless method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

/// Client inspect failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CordisInspectFailureReason {
    /// Provider disappeared.
    ProviderMissing,
    /// Method disappeared.
    MethodMissing,
    /// Input failed validation.
    InvalidInput,
    /// Provider execution failed.
    ProviderError,
    /// Query was cancelled.
    Cancelled,
}

/// Result sent from a Client inspect provider.
#[derive(Clone, Debug, PartialEq)]
pub enum CordisInspectQueryResolution {
    /// Successful JSON result.
    Success {
        /// Provider result.
        data: Value,
    },
    /// Structured failure.
    Failure {
        /// Failure category.
        reason: CordisInspectFailureReason,
        /// Human-readable diagnostic.
        message: String,
    },
}

/// Notification that an inspect query is no longer answerable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisInspectQueryResolved {
    /// Query that left the pending state.
    pub request_id: CordisInspectRequestId,
}

/// Whether one Client inspect answer claimed the pending query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CordisInspectResolveAck {
    /// False for unknown, cancelled, stale, or late answers.
    pub accepted: bool,
}

/// Error fields preserved across Host and Client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CordisErrorDetails {
    /// Original error message.
    pub message: String,
    /// Original stack when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

/// One running package announced to browser pages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisPackage {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Active immutable package.
    pub package_id: CordisDynamicPackageId,
    /// Exact activation identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Package label.
    pub name: String,
}

/// Pending model-driven Client activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisRunRequest {
    /// Correlation and approval identity.
    pub request_id: ApprovalRequestId,
    /// Session whose model owns the request.
    pub agent_id: SessionId,
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Target immutable package.
    pub package_id: CordisDynamicPackageId,
    /// Run or update intent.
    pub mode: DynamicCordisRunMode,
    /// Package label.
    pub name: String,
    /// User-facing purpose.
    pub purpose: String,
    /// Whether a page must obtain explicit approval.
    pub requires_approval: bool,
}

/// Settled Client activation request broadcast.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisRequestResolved {
    /// Request that left the answerable state.
    pub request_id: ApprovalRequestId,
    /// Settlement outcome.
    pub outcome: RequestRunOutcome,
}

/// One exact activation withdrawn from every page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisRetracted {
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Withdrawn package identity.
    pub package_id: CordisDynamicPackageId,
    /// Withdrawn activation identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
}

/// Result of removing a plugin and all versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicCordisUndefineReceipt {
    /// Plugin was removed.
    Success {
        /// Whether a physical run was stopped.
        was_running: bool,
    },
    /// No plugin exists in process memory.
    PluginMissing {
        /// Stable user-facing explanation.
        message: String,
    },
}

/// Render failure observed after Client activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisRenderFailure {
    /// Slot whose component failed.
    pub slot: String,
    /// Failure text.
    pub message: String,
    /// Original stack when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// Whether the contribution relinquished its slot.
    pub abdicated: bool,
}

/// Successful activation phase exposed to callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DynamicCordisRunSuccessStatus {
    /// Waiting for approval.
    AwaitingApproval,
    /// A Client page is starting.
    Starting,
    /// Required halves are running.
    Running,
}

/// Failed activation category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DynamicCordisRunFailureReason {
    /// Plugin identity is absent.
    PluginMissing,
    /// Package identity is absent.
    PackageMissing,
    /// Run/update intent is invalid for current state.
    InvalidMode,
    /// Another transition owns the plugin.
    TransitionInFlight,
    /// Host half failed.
    HostHalfFailed,
    /// Client half failed.
    ClientHalfFailed,
    /// User rejected activation.
    Rejected,
    /// Owner cancelled activation.
    Cancelled,
    /// Plugin is not running.
    NotRunning,
}

/// Result shared by model-driven and panel-driven activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicCordisRunResponse {
    /// Successful or pending activation.
    Success {
        /// Synchronous status.
        status: DynamicCordisRunSuccessStatus,
        /// Stable plugin identity.
        plugin_id: CordisDynamicPluginId,
        /// Target package identity.
        package_id: CordisDynamicPackageId,
        /// Exact attempt identity.
        plugin_run_id: CordisDynamicPluginRunId,
        /// Missing Host services.
        waiting_for: Vec<String>,
        /// Missing Client services.
        client_waiting_for: Option<Vec<String>>,
        /// Last fully successful package.
        current_package_id: Option<CordisDynamicPackageId>,
        /// Selected transition target.
        next_package_id: Option<CordisDynamicPackageId>,
        /// Run or update intent.
        mode: DynamicCordisRunMode,
    },
    /// Structured activation failure.
    Failure {
        /// Failure category.
        reason: DynamicCordisRunFailureReason,
        /// Human-readable diagnostic.
        message: String,
        /// Original stack when available.
        stack: Option<String>,
    },
}

/// Stop failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DynamicCordisStopFailureReason {
    /// Plugin identity is absent.
    PluginMissing,
    /// Plugin has no active run.
    NotRunning,
}

/// Result of stopping a plugin without deleting definitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicCordisStopResponse {
    /// Active run stopped.
    Success,
    /// Stop was rejected.
    Failure {
        /// Failure category.
        reason: DynamicCordisStopFailureReason,
        /// Human-readable diagnostic.
        message: String,
    },
}

/// Result of bringing up the Host half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicCordisHostHalfResult {
    /// Host half exists or was created.
    Success {
        /// Stable plugin identity.
        plugin_id: CordisDynamicPluginId,
        /// Package identity.
        package_id: CordisDynamicPackageId,
        /// Exact activation identity.
        plugin_run_id: CordisDynamicPluginRunId,
        /// Missing Host services.
        waiting_for: Vec<String>,
        /// False when attaching to an existing run.
        started_here: bool,
    },
    /// Host load or apply failure.
    Failure(CordisErrorDetails),
}

/// Client-half source for one exact activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicCordisClientSource {
    /// Browser JavaScript body.
    pub code: String,
    /// Package label.
    pub name: String,
    /// Stable plugin identity.
    pub plugin_id: CordisDynamicPluginId,
    /// Immutable package identity.
    pub package_id: CordisDynamicPackageId,
    /// Exact activation identity.
    pub plugin_run_id: CordisDynamicPluginRunId,
}

/// Browser verdict for approval and panel runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicCordisRunResolution {
    /// Client half loaded successfully.
    Success {
        /// Exact activation identity.
        plugin_run_id: CordisDynamicPluginRunId,
        /// Missing Client services.
        waiting_for: Option<Vec<String>>,
    },
    /// Refusal or activation failure.
    Failure {
        /// Failure category.
        reason: DynamicCordisRunFailureReason,
        /// Activation identity when one existed.
        plugin_run_id: Option<CordisDynamicPluginRunId>,
        /// Whether this page created the failed run.
        started_here: Option<bool>,
        /// Failure message.
        message: Option<String>,
        /// Original stack when available.
        stack: Option<String>,
    },
}

/// Whether a Client resolution claimed a pending request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicCordisResolveAck {
    /// False for late, unknown, or stale answers.
    pub accepted: bool,
}

/// Host invocation failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DynamicCordisInvokeErrorCode {
    /// Plugin has no active run.
    PluginNotRunning,
    /// Caller addressed an old run.
    StaleRun,
    /// Host method is absent.
    MethodNotFound,
    /// Host method threw or rejected.
    HandlerError,
}

/// Result of routing one Client call to the Host half.
#[derive(Clone, Debug, PartialEq)]
pub enum DynamicCordisInvokeResult {
    /// JSON result.
    Success {
        /// Handler result.
        value: Value,
    },
    /// Structured invocation failure.
    Failure {
        /// Machine-readable code.
        code: DynamicCordisInvokeErrorCode,
        /// Original error fields.
        error: CordisErrorDetails,
    },
}

fn serialize_json<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value.serialize(serializer)
}

impl Serialize for CordisInspectQueryResolution {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_json(
            &match self {
                Self::Success { data } => serde_json::json!({"ok": true, "data": data}),
                Self::Failure { reason, message } => {
                    serde_json::json!({"ok": false, "reason": reason, "message": message})
                }
            },
            serializer,
        )
    }
}

impl Serialize for DynamicCordisUndefineReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_json(
            &match self {
                Self::Success { was_running } => {
                    serde_json::json!({"ok": true, "wasRunning": was_running})
                }
                Self::PluginMissing { message } => serde_json::json!({
                    "ok": false,
                    "reason": "plugin-missing",
                    "message": message,
                }),
            },
            serializer,
        )
    }
}

impl Serialize for DynamicCordisRunResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Success {
                status,
                plugin_id,
                package_id,
                plugin_run_id,
                waiting_for,
                client_waiting_for,
                current_package_id,
                next_package_id,
                mode,
            } => {
                let mut value = serde_json::json!({
                    "ok": true,
                    "status": status,
                    "pluginId": plugin_id,
                    "packageId": package_id,
                    "pluginRunId": plugin_run_id,
                    "waitingFor": waiting_for,
                    "mode": mode,
                });
                let object = value.as_object_mut().expect("literal object");
                if let Some(waiting) = client_waiting_for {
                    object.insert("clientWaitingFor".to_owned(), serde_json::json!(waiting));
                }
                if let Some(current) = current_package_id {
                    object.insert("currentPackageId".to_owned(), serde_json::json!(current));
                }
                if let Some(next) = next_package_id {
                    object.insert("nextPackageId".to_owned(), serde_json::json!(next));
                }
                value
            }
            Self::Failure {
                reason,
                message,
                stack,
            } => {
                let mut value = serde_json::json!({
                    "ok": false,
                    "reason": reason,
                    "message": message,
                });
                if let Some(stack) = stack {
                    value
                        .as_object_mut()
                        .expect("literal object")
                        .insert("stack".to_owned(), Value::String(stack.clone()));
                }
                value
            }
        };
        serialize_json(&value, serializer)
    }
}

impl Serialize for DynamicCordisStopResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_json(
            &match self {
                Self::Success => serde_json::json!({"ok": true}),
                Self::Failure { reason, message } => {
                    serde_json::json!({"ok": false, "reason": reason, "message": message})
                }
            },
            serializer,
        )
    }
}

impl Serialize for DynamicCordisHostHalfResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_json(
            &match self {
                Self::Success {
                    plugin_id,
                    package_id,
                    plugin_run_id,
                    waiting_for,
                    started_here,
                } => serde_json::json!({
                    "ok": true,
                    "pluginId": plugin_id,
                    "packageId": package_id,
                    "pluginRunId": plugin_run_id,
                    "waitingFor": waiting_for,
                    "startedHere": started_here,
                }),
                Self::Failure(error) => {
                    let mut value =
                        serde_json::to_value(error).map_err(serde::ser::Error::custom)?;
                    value
                        .as_object_mut()
                        .expect("error details object")
                        .insert("ok".to_owned(), Value::Bool(false));
                    value
                }
            },
            serializer,
        )
    }
}

impl Serialize for DynamicCordisRunResolution {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Success {
                plugin_run_id,
                waiting_for,
            } => {
                let mut value = serde_json::json!({"ok": true, "pluginRunId": plugin_run_id});
                if let Some(waiting) = waiting_for {
                    value
                        .as_object_mut()
                        .expect("literal object")
                        .insert("waitingFor".to_owned(), serde_json::json!(waiting));
                }
                value
            }
            Self::Failure {
                reason,
                plugin_run_id,
                started_here,
                message,
                stack,
            } => {
                let mut value = serde_json::json!({"ok": false, "reason": reason});
                let object = value.as_object_mut().expect("literal object");
                if let Some(run) = plugin_run_id {
                    object.insert("pluginRunId".to_owned(), serde_json::json!(run));
                }
                if let Some(started) = started_here {
                    object.insert("startedHere".to_owned(), Value::Bool(*started));
                }
                if let Some(message) = message {
                    object.insert("message".to_owned(), Value::String(message.clone()));
                }
                if let Some(stack) = stack {
                    object.insert("stack".to_owned(), Value::String(stack.clone()));
                }
                value
            }
        };
        serialize_json(&value, serializer)
    }
}

impl Serialize for DynamicCordisInvokeResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_json(
            &match self {
                Self::Success { value } => serde_json::json!({"ok": true, "value": value}),
                Self::Failure { code, error } => {
                    let mut value =
                        serde_json::to_value(error).map_err(serde::ser::Error::custom)?;
                    let object = value.as_object_mut().expect("error details object");
                    object.insert("ok".to_owned(), Value::Bool(false));
                    object.insert("code".to_owned(), serde_json::json!(code));
                    value
                }
            },
            serializer,
        )
    }
}
