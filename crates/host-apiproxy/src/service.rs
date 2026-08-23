//! Composed API Proxy runtime.
//!
//! This service owns Host/workspace-facing defaults and delegates other domain
//! seats to an injected runtime. Domain ownership remains explicit while all
//! calls share the same physical carrier and response vocabulary.

use std::sync::Arc;

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_host_directory_picker::{
    DIRECTORY_PICKER, DirectoryPickerCapability, DirectoryPickerFailure, DirectoryPickerService,
};
use seekdeep_llm::AbortSignal;
use serde_json::{Map, Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ConfigurationApiProxyOptions,
    ConfigurationApiProxyRuntime, RpcId, RpcMethod, RpcReceipt, RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        host::{
            HostCreateDirectoryRequest, HostDescribeValue, HostListDirectoryRequest,
            HostOpenPathRequest,
        },
        sessions::is_ecmascript_whitespace,
        workspace::{
            WorkspaceArchiveSessionRequest, WorkspaceCreateRequest, WorkspaceId,
            WorkspaceIdRequest, WorkspaceInsertBeforeRequest, WorkspaceInsertSessionBeforeRequest,
            WorkspaceRenameRequest, WorkspaceView,
        },
    },
    native_path_opener::{PathOpenerInternals, can_open_native_path, open_native_path},
};

/// Complete model selection used as the dynamic default for new sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    /// Registered provider route.
    pub provider: String,
    /// Provider-owned model id.
    pub model: String,
    /// Adapter-owned reasoning effort, when selected.
    pub reasoning_effort: Option<String>,
}

/// Dynamic default-model reader.
pub type DefaultModelSelection = Arc<dyn Fn() -> ModelSelection + Send + Sync>;
/// Dynamic attached-agent count reader.
pub type AttachedSessionCount = Arc<dyn Fn() -> usize + Send + Sync>;
/// Native path-open boundary.
pub type PathOpener =
    Arc<dyn Fn(String, AbortSignal) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// Native path affordance probe.
pub type PathCapabilityProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// Complete synchronous Workspace baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// Registry rows in durable display order.
    pub items: Vec<WorkspaceView>,
    /// Registry-global archived Session set.
    pub archived_session_ids: Vec<seekdeep_core::session::SessionId>,
}

/// Business-classified Workspace runtime failure.
#[derive(Debug, Error)]
pub enum WorkspaceRuntimeError {
    /// A Workspace source or anchor is absent.
    #[error("workspace \"{0}\" not found")]
    NotFound(WorkspaceId),
    /// An explicit display title duplicates another Workspace.
    #[error("workspace name '{0}' is already in use")]
    NameConflict(String),
    /// A Session source or anchor is not accounted by the Workspace.
    #[error("{0}")]
    MoveInvalid(String),
    /// The Session is neither live nor durably persisted.
    #[error("{message}")]
    UnknownSession {
        /// Unknown Session identity.
        session_id: seekdeep_core::session::SessionId,
        /// Registry-owned diagnostic.
        message: String,
    },
    /// Storage, durability, or other unclassified failure.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// Workspace registry seat consumed by the API Proxy.
pub trait WorkspaceRuntime: Send + Sync + 'static {
    /// Returns a no-I/O baseline in durable order.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime is not initialized or its in-memory
    /// registry is inconsistent.
    fn list(&self) -> anyhow::Result<WorkspaceSnapshot>;

    /// Atomically resolves or creates ownership of one canonical path.
    fn create(&self, path: String) -> BoxFuture<'static, anyhow::Result<(WorkspaceView, bool)>>;

    /// Atomically applies a trimmed unique display title.
    fn rename(
        &self,
        workspace_id: WorkspaceId,
        title: String,
    ) -> BoxFuture<'static, Result<WorkspaceView, WorkspaceRuntimeError>>;

    /// Deletes one registration while retaining its directory and Sessions.
    fn delete(
        &self,
        workspace_id: WorkspaceId,
    ) -> BoxFuture<'static, Result<(), WorkspaceRuntimeError>>;

    /// Moves one Workspace and returns the complete committed order.
    fn insert_before(
        &self,
        workspace_id: WorkspaceId,
        before_workspace_id: Option<WorkspaceId>,
    ) -> BoxFuture<'static, Result<Vec<WorkspaceId>, WorkspaceRuntimeError>>;

    /// Moves one accounted Session and returns the updated Workspace.
    fn insert_session_before(
        &self,
        workspace_id: WorkspaceId,
        session_id: seekdeep_core::session::SessionId,
        before_session_id: Option<seekdeep_core::session::SessionId>,
    ) -> BoxFuture<'static, Result<WorkspaceView, WorkspaceRuntimeError>>;

    /// Archives one known Session and returns the complete archive set.
    fn archive_session(
        &self,
        session_id: seekdeep_core::session::SessionId,
    ) -> BoxFuture<'static, Result<Vec<seekdeep_core::session::SessionId>, WorkspaceRuntimeError>>;

    /// Opens committed Workspace increments after establishing its baseline.
    fn host_events(
        &self,
        _signal: AbortSignal,
    ) -> futures::stream::BoxStream<'static, anyhow::Result<HostFrame>> {
        futures::stream::empty().boxed()
    }
}

/// Host defaults consumed by the composed API implementation.
#[derive(Clone)]
pub struct ApiProxyDefaults {
    /// Read on every access so a saved default reaches the next session.
    pub default_model_selection: DefaultModelSelection,
    /// Project directory for new sessions with no explicit cwd.
    pub cwd: String,
    /// Optional native open-with-default-application boundary.
    pub open_path: Option<PathOpener>,
    /// Optional native text-document editor boundary.
    pub open_text_file: Option<PathOpener>,
    /// Optional native-open capability override.
    pub can_open_path: Option<PathCapabilityProbe>,
    /// Platform facts and command runner used by the default native opener.
    pub native_path_opener: PathOpenerInternals,
}

impl std::fmt::Debug for ApiProxyDefaults {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiProxyDefaults")
            .field("cwd", &self.cwd)
            .field("has_open_path", &self.open_path.is_some())
            .field("has_open_text_file", &self.open_text_file.is_some())
            .field("has_can_open_path", &self.can_open_path.is_some())
            .field("native_path_opener", &self.native_path_opener)
            .finish_non_exhaustive()
    }
}

/// API Proxy runtime with the Host domain composed over the remaining domains.
pub struct ApiProxyService {
    defaults: ApiProxyDefaults,
    directory_picker: Arc<DirectoryPickerService>,
    attached_session_count: AttachedSessionCount,
    workspace: Option<Arc<dyn WorkspaceRuntime>>,
    domains: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for ApiProxyService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiProxyService")
            .field("defaults", &self.defaults)
            .field("directory_picker", &self.directory_picker)
            .field("has_workspace", &self.workspace.is_some())
            .finish_non_exhaustive()
    }
}

impl ApiProxyService {
    /// Composes Host behavior over a runtime owning the other API domains.
    #[must_use]
    pub fn new(
        defaults: ApiProxyDefaults,
        directory_picker: Arc<DirectoryPickerService>,
        attached_session_count: AttachedSessionCount,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            defaults,
            directory_picker,
            attached_session_count,
            workspace: None,
            domains,
        })
    }

    /// Composes Host and Workspace behavior over all remaining domains.
    #[must_use]
    pub fn with_workspace(
        defaults: ApiProxyDefaults,
        directory_picker: Arc<DirectoryPickerService>,
        attached_session_count: AttachedSessionCount,
        workspace: Arc<dyn WorkspaceRuntime>,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            defaults,
            directory_picker,
            attached_session_count,
            workspace: Some(workspace),
            domains,
        })
    }

    /// Composes Host and configuration domains from their Cordis services.
    ///
    /// # Errors
    ///
    /// Returns an error when the composition does not mount its required
    /// directory-picker or LLM service.
    pub fn from_context(
        context: &seekdeep_cordis::Context,
        defaults: ApiProxyDefaults,
        attached_session_count: AttachedSessionCount,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> anyhow::Result<Arc<Self>> {
        let picker = context
            .get(DIRECTORY_PICKER)
            .ok_or_else(|| anyhow::anyhow!("directoryPicker service is required"))?;
        let configuration = ConfigurationApiProxyRuntime::from_context(
            context,
            ConfigurationApiProxyOptions {
                open_text_file: defaults.open_text_file.clone(),
                native_path_opener: defaults.native_path_opener.clone(),
            },
            domains,
        )?;
        Ok(Self::new(
            defaults,
            picker,
            attached_session_count,
            configuration,
        ))
    }

    async fn host_unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        match method {
            RpcMethod::HostDescribe => Ok(self.describe(request)),
            RpcMethod::HostPickDirectory => self.pick_directory(request, signal).await,
            RpcMethod::HostListDirectory => self.list_directory(request, signal).await,
            RpcMethod::HostCreateDirectory => self.create_directory(request).await,
            RpcMethod::HostOpenPath => self.open_path(request, signal).await,
            RpcMethod::WorkspaceList
            | RpcMethod::WorkspaceCreate
            | RpcMethod::WorkspaceRename
            | RpcMethod::WorkspaceDelete
            | RpcMethod::WorkspaceInsertBefore
            | RpcMethod::WorkspaceInsertSessionBefore
            | RpcMethod::WorkspaceArchiveSession
                if self.workspace.is_some() =>
            {
                self.workspace_unary(method, request).await
            }
            _ => self.domains.unary(method, request, signal).await,
        }
    }

    async fn workspace_unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let workspace = self
            .workspace
            .as_ref()
            .expect("Workspace methods are gated on a composed runtime");
        match method {
            RpcMethod::WorkspaceList => Self::workspace_list(workspace, request),
            RpcMethod::WorkspaceCreate => Self::workspace_create(workspace, request).await,
            RpcMethod::WorkspaceRename => Self::workspace_rename(workspace, request).await,
            RpcMethod::WorkspaceDelete => Self::workspace_delete(workspace, request).await,
            RpcMethod::WorkspaceInsertBefore => {
                Self::workspace_insert_before(workspace, request).await
            }
            RpcMethod::WorkspaceInsertSessionBefore => {
                Self::workspace_insert_session_before(workspace, request).await
            }
            RpcMethod::WorkspaceArchiveSession => {
                Self::workspace_archive_session(workspace, request).await
            }
            _ => unreachable!("only Workspace methods enter workspace_unary"),
        }
    }

    fn workspace_list(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let snapshot = workspace.list()?;
        Ok(success(
            request,
            json!({
                "items": snapshot.items,
                "archivedSessionIds": snapshot.archived_session_ids,
            }),
        ))
    }

    async fn workspace_create(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: WorkspaceCreateRequest = serde_json::from_value(request.payload.clone())?;
        match workspace.create(payload.path.clone()).await {
            Ok((workspace, created)) => Ok(success(
                request,
                json!({ "workspace": workspace, "created": created }),
            )),
            Err(error) => Ok(failure(
                request,
                "workspace-invalid-path",
                format!("cannot create a workspace at \"{}\": {error}", payload.path),
                Map::from_iter([("path".to_owned(), Value::String(payload.path))]),
            )),
        }
    }

    async fn workspace_rename(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: WorkspaceRenameRequest = serde_json::from_value(request.payload.clone())?;
        let title = payload
            .title
            .trim_matches(is_ecmascript_whitespace)
            .to_owned();
        match workspace.rename(payload.workspace_id, title).await {
            Ok(workspace) => Ok(success(request, json!({ "workspace": workspace }))),
            Err(WorkspaceRuntimeError::NotFound(id)) => Ok(workspace_not_found(request, &id)),
            Err(WorkspaceRuntimeError::NameConflict(name)) => Ok(failure(
                request,
                "workspace-name-conflict",
                format!("workspace name '{name}' is already in use"),
                Map::from_iter([("name".to_owned(), Value::String(name))]),
            )),
            Err(error) => Err(workspace_internal(error)),
        }
    }

    async fn workspace_delete(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: WorkspaceIdRequest = serde_json::from_value(request.payload.clone())?;
        match workspace.delete(payload.workspace_id).await {
            Ok(()) => Ok(success(request, json!({ "deleted": true }))),
            Err(WorkspaceRuntimeError::NotFound(id)) => Ok(workspace_not_found(request, &id)),
            Err(error) => Err(workspace_internal(error)),
        }
    }

    async fn workspace_insert_before(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: WorkspaceInsertBeforeRequest =
            serde_json::from_value(request.payload.clone())?;
        match workspace
            .insert_before(payload.workspace_id, payload.before_workspace_id)
            .await
        {
            Ok(ids) => Ok(success(request, json!({ "workspaceIds": ids }))),
            Err(WorkspaceRuntimeError::NotFound(id)) => Ok(workspace_not_found(request, &id)),
            Err(error) => Err(workspace_internal(error)),
        }
    }

    async fn workspace_insert_session_before(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: WorkspaceInsertSessionBeforeRequest =
            serde_json::from_value(request.payload.clone())?;
        let details = [
            (
                "workspaceId".to_owned(),
                Value::String(payload.workspace_id.as_str().to_owned()),
            ),
            (
                "sessionId".to_owned(),
                Value::String(payload.session_id.as_str().to_owned()),
            ),
        ]
        .into_iter()
        .chain(payload.before_session_id.as_ref().map(|id| {
            (
                "beforeSessionId".to_owned(),
                Value::String(id.as_str().to_owned()),
            )
        }))
        .collect::<Map<_, _>>();
        match workspace
            .insert_session_before(
                payload.workspace_id,
                payload.session_id,
                payload.before_session_id,
            )
            .await
        {
            Ok(workspace) => Ok(success(request, json!({ "workspace": workspace }))),
            Err(WorkspaceRuntimeError::NotFound(id)) => Ok(workspace_not_found(request, &id)),
            Err(WorkspaceRuntimeError::MoveInvalid(message)) => {
                Ok(failure(request, "workspace-move-invalid", message, details))
            }
            Err(error) => Err(workspace_internal(error)),
        }
    }

    async fn workspace_archive_session(
        workspace: &Arc<dyn WorkspaceRuntime>,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: WorkspaceArchiveSessionRequest =
            serde_json::from_value(request.payload.clone())?;
        match workspace.archive_session(payload.session_id).await {
            Ok(ids) => Ok(success(request, json!({ "archivedSessionIds": ids }))),
            Err(WorkspaceRuntimeError::UnknownSession {
                session_id,
                message,
            }) => Ok(failure(
                request,
                "session-not-found",
                message,
                Map::from_iter([(
                    "sessionId".to_owned(),
                    Value::String(session_id.as_str().to_owned()),
                )]),
            )),
            Err(error) => Err(workspace_internal(error)),
        }
    }

    fn describe(&self, request: RpcRequest<Value>) -> RpcResponse<Value> {
        let selection = (self.defaults.default_model_selection)();
        let attached_sessions = u64::try_from((self.attached_session_count)()).unwrap_or(u64::MAX);
        let value = HostDescribeValue {
            // Exact pinned-source placeholder. The source carries the same TODO.
            version: "0.0.1".to_owned(),
            cwd: self.defaults.cwd.clone(),
            provider: Some(selection.provider),
            model: Some(selection.model),
            attached_sessions,
            can_open_path: self.can_open_paths(),
        };
        success(
            request,
            serde_json::to_value(value).expect("Host description must serialize"),
        )
    }

    async fn pick_directory(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let DirectoryPickerCapability::Native { pick } = self.directory_picker.capability() else {
            return Ok(capability_unavailable(
                request,
                "host.pickDirectory",
                "native",
                self.directory_picker.capability().kind(),
            ));
        };
        match pick(signal.clone()).await {
            Ok(path) => Ok(success(request, json!({ "path": path }))),
            Err(_) if signal.is_aborted() => Ok(failure(
                request,
                "cancelled",
                "directory picker was aborted",
                Map::new(),
            )),
            Err(error) => Ok(failure(
                request,
                "internal",
                format!("directory picker failed: {error}"),
                Map::new(),
            )),
        }
    }

    async fn list_directory(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: HostListDirectoryRequest = serde_json::from_value(request.payload.clone())?;
        let DirectoryPickerCapability::Browse { list, .. } = self.directory_picker.capability()
        else {
            return Ok(capability_unavailable(
                request,
                "host.listDirectory",
                "browse",
                self.directory_picker.capability().kind(),
            ));
        };
        match list(payload.path, signal.clone()).await {
            Ok(listing) => Ok(success(
                request,
                serde_json::to_value(listing).expect("directory listing must serialize"),
            )),
            Err(_) if signal.is_aborted() => Ok(failure(
                request,
                "cancelled",
                "directory listing was aborted",
                Map::new(),
            )),
            Err(error) => Ok(directory_failure(request, error)),
        }
    }

    async fn create_directory(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: HostCreateDirectoryRequest = serde_json::from_value(request.payload.clone())?;
        let DirectoryPickerCapability::Browse {
            create_directory, ..
        } = self.directory_picker.capability()
        else {
            return Ok(capability_unavailable(
                request,
                "host.createDirectory",
                "browse",
                self.directory_picker.capability().kind(),
            ));
        };
        match create_directory(payload.path, payload.name).await {
            Ok(path) => Ok(success(request, json!({ "path": path }))),
            Err(error) => Ok(directory_failure(request, error)),
        }
    }

    async fn open_path(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: HostOpenPathRequest = serde_json::from_value(request.payload.clone())?;
        let result = if let Some(open) = &self.defaults.open_path {
            open(payload.path, signal.clone()).await
        } else {
            open_native_path(&payload.path, &signal, &self.defaults.native_path_opener).await
        };
        match result {
            Ok(()) => Ok(success(request, json!({ "opened": true }))),
            Err(_) if signal.is_aborted() => Ok(failure(
                request,
                "cancelled",
                "path open was aborted",
                Map::new(),
            )),
            Err(error) => Ok(failure(
                request,
                "internal",
                format!("path open failed: {error}"),
                Map::new(),
            )),
        }
    }

    fn can_open_paths(&self) -> bool {
        if let Some(probe) = &self.defaults.can_open_path {
            return probe();
        }
        self.defaults.open_path.is_some() || can_open_native_path(&self.defaults.native_path_opener)
    }
}

impl ApiProxyRuntime for ApiProxyService {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        let service = Arc::new(Self {
            defaults: self.defaults.clone(),
            directory_picker: self.directory_picker.clone(),
            attached_session_count: self.attached_session_count.clone(),
            workspace: self.workspace.clone(),
            domains: self.domains.clone(),
        });
        async move { service.host_unary(method, request, signal).await }.boxed()
    }

    fn respond(
        &self,
        message: ClientResponse,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        self.domains.respond(message, signal)
    }

    fn mux(&self, request: RpcRequest<Value>, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        self.domains.mux(request, signal)
    }

    fn host(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        let domains = self.domains.host(request, signal.clone());
        let Some(workspace) = &self.workspace else {
            return domains;
        };
        let workspace = workspace.host_events(signal).map(|frame| {
            frame.map(|payload| RpcRequest::new(RpcId::new(Uuid::new_v4().to_string()), payload))
        });
        futures::stream::select(domains, workspace).boxed()
    }

    fn session_log(
        &self,
        query: SessionLogQuery,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        self.domains.session_log(query, signal)
    }
}

fn success(request: RpcRequest<Value>, value: Value) -> RpcResponse<Value> {
    RpcResponse::new(request.rpc_id, RpcResult::Success { value: Some(value) })
}

fn failure(
    request: RpcRequest<Value>,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Map<String, Value>,
) -> RpcResponse<Value> {
    RpcResponse::new(
        request.rpc_id,
        RpcResult::Failure {
            error: RpcError {
                code: code.into(),
                message: message.into(),
                details,
            },
        },
    )
}

fn capability_unavailable(
    request: RpcRequest<Value>,
    method: &str,
    required: &str,
    actual: &str,
) -> RpcResponse<Value> {
    failure(
        request,
        "directory-picker-unavailable",
        format!(
            "{method} needs the {required} capability; the composed picker serves \"{actual}\""
        ),
        Map::from_iter([("capability".to_owned(), Value::String(actual.to_owned()))]),
    )
}

fn directory_failure(
    request: RpcRequest<Value>,
    error: DirectoryPickerFailure,
) -> RpcResponse<Value> {
    match error {
        DirectoryPickerFailure::Picker(error) => failure(
            request,
            error.code.as_str(),
            error.message,
            Map::from_iter([("path".to_owned(), Value::String(error.path))]),
        ),
        DirectoryPickerFailure::Internal(error) => {
            failure(request, "internal", error.to_string(), Map::new())
        }
    }
}

fn workspace_not_found(
    request: RpcRequest<Value>,
    workspace_id: &WorkspaceId,
) -> RpcResponse<Value> {
    failure(
        request,
        "workspace-not-found",
        format!("workspace \"{workspace_id}\" not found"),
        Map::from_iter([(
            "workspaceId".to_owned(),
            Value::String(workspace_id.as_str().to_owned()),
        )]),
    )
}

fn workspace_internal(error: WorkspaceRuntimeError) -> anyhow::Error {
    match error {
        WorkspaceRuntimeError::Internal(error) => error,
        other => anyhow::anyhow!(other),
    }
}
