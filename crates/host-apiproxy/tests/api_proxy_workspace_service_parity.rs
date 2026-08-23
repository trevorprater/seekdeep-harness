//! Workspace-domain API cases ported from the pinned workspace specification.

use std::{
    collections::HashSet,
    fs,
    sync::{Arc, Mutex},
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use seekdeep_core::session::SessionId;
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyDefaults, ApiProxyRuntime, ApiProxyService, ClientResponse,
    ModelSelection, PathOpenerInternals, RpcError, RpcId, RpcMethod, RpcReceipt, RpcRequest,
    RpcResponse, RpcResult, WorkspaceRuntime, WorkspaceRuntimeError, WorkspaceSnapshot,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        workspace::{WorkspaceId, WorkspaceView},
    },
};
use seekdeep_host_directory_picker::{DirectoryPickerCapability, DirectoryPickerService};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};

#[derive(Default)]
struct WorkspaceState {
    next_id: u64,
    items: Vec<WorkspaceView>,
    archived: Vec<SessionId>,
    known_sessions: HashSet<SessionId>,
}

struct MemoryWorkspace {
    state: Arc<Mutex<WorkspaceState>>,
    events: tokio::sync::broadcast::Sender<HostFrame>,
}

impl Default for MemoryWorkspace {
    fn default() -> Self {
        let (events, _) = tokio::sync::broadcast::channel(32);
        Self {
            state: Arc::new(Mutex::new(WorkspaceState::default())),
            events,
        }
    }
}

impl MemoryWorkspace {
    fn know_session(&self, id: SessionId) {
        self.state.lock().unwrap().known_sessions.insert(id);
    }

    fn attach(&self, workspace_id: &WorkspaceId, session_id: SessionId) {
        let mut state = self.state.lock().unwrap();
        let changed = {
            let workspace = state
                .items
                .iter_mut()
                .find(|workspace| &workspace.workspace_id == workspace_id)
                .unwrap();
            if !workspace.session_ids.contains(&session_id) {
                workspace.session_ids.insert(0, session_id.clone());
            }
            workspace.clone()
        };
        state.known_sessions.insert(session_id);
        drop(state);
        let _ = self
            .events
            .send(HostFrame::WorkspaceChanged { workspace: changed });
    }
}

impl WorkspaceRuntime for MemoryWorkspace {
    fn list(&self) -> anyhow::Result<WorkspaceSnapshot> {
        let state = self.state.lock().unwrap();
        Ok(WorkspaceSnapshot {
            items: state.items.clone(),
            archived_session_ids: state.archived.clone(),
        })
    }

    fn create(&self, path: String) -> BoxFuture<'static, anyhow::Result<(WorkspaceView, bool)>> {
        let state = self.state.clone();
        let events = self.events.clone();
        async move {
            let canonical = fs::canonicalize(&path)?;
            anyhow::ensure!(canonical.is_dir(), "path is not a directory");
            let canonical = canonical.to_string_lossy().into_owned();
            let mut state = state.lock().unwrap();
            if let Some(existing) = state.items.iter().find(|item| item.path == canonical) {
                return Ok((existing.clone(), false));
            }
            state.next_id += 1;
            let workspace = WorkspaceView {
                workspace_id: WorkspaceId::new(format!("workspace-{}", state.next_id)),
                path: canonical.clone(),
                title: canonical
                    .rsplit(std::path::MAIN_SEPARATOR)
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                session_ids: Vec::new(),
                created_at: "2026-08-15T00:00:00.000Z".to_owned(),
                updated_at: "2026-08-15T00:00:00.000Z".to_owned(),
            };
            state.items.insert(0, workspace.clone());
            drop(state);
            let _ = events.send(HostFrame::WorkspaceChanged {
                workspace: workspace.clone(),
            });
            Ok((workspace, true))
        }
        .boxed()
    }

    fn rename(
        &self,
        workspace_id: WorkspaceId,
        title: String,
    ) -> BoxFuture<'static, Result<WorkspaceView, WorkspaceRuntimeError>> {
        let state = self.state.clone();
        let events = self.events.clone();
        async move {
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .items
                .iter()
                .position(|item| item.workspace_id == workspace_id)
            else {
                return Err(WorkspaceRuntimeError::NotFound(workspace_id));
            };
            if state
                .items
                .iter()
                .enumerate()
                .any(|(other, item)| other != index && item.title == title)
            {
                return Err(WorkspaceRuntimeError::NameConflict(title));
            }
            state.items[index].title = title;
            let workspace = state.items[index].clone();
            drop(state);
            let _ = events.send(HostFrame::WorkspaceChanged {
                workspace: workspace.clone(),
            });
            Ok(workspace)
        }
        .boxed()
    }

    fn delete(
        &self,
        workspace_id: WorkspaceId,
    ) -> BoxFuture<'static, Result<(), WorkspaceRuntimeError>> {
        let state = self.state.clone();
        let events = self.events.clone();
        async move {
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .items
                .iter()
                .position(|item| item.workspace_id == workspace_id)
            else {
                return Err(WorkspaceRuntimeError::NotFound(workspace_id));
            };
            state.items.remove(index);
            drop(state);
            let _ = events.send(HostFrame::WorkspaceRemoved { workspace_id });
            Ok(())
        }
        .boxed()
    }

    fn insert_before(
        &self,
        workspace_id: WorkspaceId,
        before_workspace_id: Option<WorkspaceId>,
    ) -> BoxFuture<'static, Result<Vec<WorkspaceId>, WorkspaceRuntimeError>> {
        let state = self.state.clone();
        let events = self.events.clone();
        async move {
            let mut state = state.lock().unwrap();
            let Some(source) = state
                .items
                .iter()
                .position(|item| item.workspace_id == workspace_id)
            else {
                return Err(WorkspaceRuntimeError::NotFound(workspace_id));
            };
            if before_workspace_id.as_ref() == Some(&workspace_id) {
                return Ok(state
                    .items
                    .iter()
                    .map(|item| item.workspace_id.clone())
                    .collect());
            }
            if let Some(anchor) = &before_workspace_id
                && !state.items.iter().any(|item| &item.workspace_id == anchor)
            {
                return Err(WorkspaceRuntimeError::NotFound(anchor.clone()));
            }
            let previous = state
                .items
                .iter()
                .map(|item| item.workspace_id.clone())
                .collect::<Vec<_>>();
            let moved = state.items.remove(source);
            let target = before_workspace_id
                .as_ref()
                .map_or(state.items.len(), |anchor| {
                    state
                        .items
                        .iter()
                        .position(|item| &item.workspace_id == anchor)
                        .unwrap()
                });
            state.items.insert(target, moved);
            let order = state
                .items
                .iter()
                .map(|item| item.workspace_id.clone())
                .collect::<Vec<_>>();
            drop(state);
            if order != previous {
                let _ = events.send(HostFrame::WorkspaceOrderChanged {
                    workspace_ids: order.clone(),
                });
            }
            Ok(order)
        }
        .boxed()
    }

    fn insert_session_before(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        before_session_id: Option<SessionId>,
    ) -> BoxFuture<'static, Result<WorkspaceView, WorkspaceRuntimeError>> {
        let state = self.state.clone();
        let events = self.events.clone();
        async move {
            let mut state = state.lock().unwrap();
            let Some(workspace) = state
                .items
                .iter_mut()
                .find(|item| item.workspace_id == workspace_id)
            else {
                return Err(WorkspaceRuntimeError::NotFound(workspace_id));
            };
            let Some(source) = workspace
                .session_ids
                .iter()
                .position(|id| id == &session_id)
            else {
                return Err(WorkspaceRuntimeError::MoveInvalid(format!(
                    "cannot move session '{session_id}' in workspace '{}': the session is not accounted",
                    workspace.path
                )));
            };
            if let Some(anchor) = &before_session_id
                && !workspace.session_ids.contains(anchor)
            {
                return Err(WorkspaceRuntimeError::MoveInvalid(format!(
                    "cannot move session '{session_id}' before '{anchor}' in workspace '{}': the anchor session is not accounted",
                    workspace.path
                )));
            }
            if before_session_id.as_ref() != Some(&session_id) {
                let moved = workspace.session_ids.remove(source);
                let target = before_session_id.as_ref().map_or(
                    workspace.session_ids.len(),
                    |anchor| {
                        workspace
                            .session_ids
                            .iter()
                            .position(|id| id == anchor)
                            .unwrap()
                    },
                );
                workspace.session_ids.insert(target, moved);
            }
            let changed = workspace.clone();
            drop(state);
            let _ = events.send(HostFrame::WorkspaceChanged {
                workspace: changed.clone(),
            });
            Ok(changed)
        }
        .boxed()
    }

    fn archive_session(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'static, Result<Vec<SessionId>, WorkspaceRuntimeError>> {
        let state = self.state.clone();
        let events = self.events.clone();
        async move {
            let mut state = state.lock().unwrap();
            if state.archived.contains(&session_id) {
                return Ok(state.archived.clone());
            }
            if !state.known_sessions.contains(&session_id) {
                return Err(WorkspaceRuntimeError::UnknownSession {
                    message: format!(
                        "cannot archive session '{session_id}': live sessions and session persistence hold no such session"
                    ),
                    session_id,
                });
            }
            state.archived.push(session_id);
            let archived = state.archived.clone();
            drop(state);
            let _ = events.send(HostFrame::ArchivedSessionsChanged {
                archived_session_ids: archived.clone(),
            });
            Ok(archived)
        }
        .boxed()
    }

    fn host_events(
        &self,
        signal: AbortSignal,
    ) -> futures::stream::BoxStream<'static, anyhow::Result<HostFrame>> {
        let mut subscription = self.events.subscribe();
        async_stream::stream! {
            loop {
                tokio::select! {
                    () = signal.cancelled() => return,
                    event = subscription.recv() => match event {
                        Ok(frame) => yield Ok(frame),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        Err(error) => {
                            yield Err(anyhow::anyhow!(error));
                            return;
                        }
                    },
                }
            }
        }
        .boxed()
    }
}

#[derive(Default)]
struct RemainingDomains;

impl ApiProxyRuntime for RemainingDomains {
    fn unary(
        &self,
        _method: RpcMethod,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        async { anyhow::bail!("unexpected delegated unary") }.boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async { Ok(RpcReceipt::Accepted) }.boxed()
    }

    fn mux(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<seekdeep_client_connection::HttpResponse>> {
        async {
            Ok(seekdeep_client_connection::HttpResponse::text(
                501, "unused",
            ))
        }
        .boxed()
    }
}

fn service(workspace: Arc<dyn WorkspaceRuntime>) -> Arc<ApiProxyService> {
    ApiProxyService::with_workspace(
        ApiProxyDefaults {
            default_model_selection: Arc::new(|| ModelSelection {
                provider: "p".into(),
                model: "m".into(),
                reasoning_effort: None,
            }),
            cwd: "/tmp".to_owned(),
            open_path: None,
            open_text_file: None,
            can_open_path: Some(Arc::new(|| false)),
            native_path_opener: PathOpenerInternals::default(),
        },
        DirectoryPickerService::new(DirectoryPickerCapability::Native {
            pick: Arc::new(|_| async { Ok(None) }.boxed()),
        }),
        Arc::new(|| 0),
        workspace,
        Arc::new(RemainingDomains),
    )
}

async fn invoke(service: &ApiProxyService, method: RpcMethod, payload: Value) -> RpcResult<Value> {
    service
        .unary(
            method,
            RpcRequest::new(RpcId::new("workspace-test"), payload),
            AbortSignal::default(),
        )
        .await
        .expect("workspace RPC")
        .result
}

fn value(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected value success, got {other:?}"),
    }
}

fn error(result: RpcResult<Value>) -> RpcError {
    match result {
        RpcResult::Failure { error } => error,
        other @ RpcResult::Success { .. } => panic!("expected failure, got {other:?}"),
    }
}

#[tokio::test]
async fn create_is_atomic_by_canonical_path_and_preserves_existing_title() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("alpha");
    fs::create_dir(&target).unwrap();
    let workspace = Arc::new(MemoryWorkspace::default());
    let service = service(workspace);
    let payload = json!({ "path": target.to_string_lossy() });
    let (first, second) = tokio::join!(
        invoke(&service, RpcMethod::WorkspaceCreate, payload.clone()),
        invoke(&service, RpcMethod::WorkspaceCreate, payload),
    );
    let first = value(first);
    let second = value(second);
    assert_ne!(first["created"], second["created"]);
    assert_eq!(
        first["workspace"]["workspaceId"],
        second["workspace"]["workspaceId"]
    );
    assert_eq!(
        value(invoke(&service, RpcMethod::WorkspaceList, json!({})).await)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let id = first["workspace"]["workspaceId"].as_str().unwrap();
    let renamed = value(
        invoke(
            &service,
            RpcMethod::WorkspaceRename,
            json!({ "workspaceId": id, "title": "  renamed-existing  " }),
        )
        .await,
    );
    assert_eq!(renamed["workspace"]["title"], "renamed-existing");
    let reopened = value(
        invoke(
            &service,
            RpcMethod::WorkspaceCreate,
            json!({ "path": target.to_string_lossy() }),
        )
        .await,
    );
    assert_eq!(reopened["created"], false);
    assert_eq!(reopened["workspace"]["title"], "renamed-existing");
}

#[tokio::test]
async fn create_rejects_missing_paths_and_allows_duplicate_derived_titles() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Arc::new(MemoryWorkspace::default());
    let service = service(workspace);
    let missing = root.path().join("missing");
    let failure = error(
        invoke(
            &service,
            RpcMethod::WorkspaceCreate,
            json!({ "path": missing.to_string_lossy() }),
        )
        .await,
    );
    assert_eq!(failure.code, "workspace-invalid-path");
    assert_eq!(failure.details["path"], missing.to_string_lossy().as_ref());
    assert!(!missing.exists());

    let first = root.path().join("one/project");
    let second = root.path().join("two/project");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let first_value = value(
        invoke(
            &service,
            RpcMethod::WorkspaceCreate,
            json!({ "path": first.to_string_lossy() }),
        )
        .await,
    );
    let second_value = value(
        invoke(
            &service,
            RpcMethod::WorkspaceCreate,
            json!({ "path": second.to_string_lossy() }),
        )
        .await,
    );
    assert_eq!(first_value["workspace"]["title"], "project");
    assert_eq!(second_value["workspace"]["title"], "project");
    assert_ne!(
        first_value["workspace"]["workspaceId"],
        second_value["workspace"]["workspaceId"]
    );
}

async fn create_dir_workspace(
    service: &ApiProxyService,
    root: &std::path::Path,
    name: &str,
) -> Value {
    let path = root.join(name);
    fs::create_dir(&path).unwrap();
    value(
        invoke(
            service,
            RpcMethod::WorkspaceCreate,
            json!({ "path": path.to_string_lossy() }),
        )
        .await,
    )["workspace"]
        .clone()
}

#[tokio::test]
async fn reorder_returns_complete_order_and_maps_missing_source_or_anchor() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Arc::new(MemoryWorkspace::default());
    let service = service(workspace);
    let first = create_dir_workspace(&service, root.path(), "first").await;
    let second = create_dir_workspace(&service, root.path(), "second").await;
    let third = create_dir_workspace(&service, root.path(), "third").await;
    let reordered = value(
        invoke(
            &service,
            RpcMethod::WorkspaceInsertBefore,
            json!({
                "workspaceId": first["workspaceId"],
                "beforeWorkspaceId": second["workspaceId"],
            }),
        )
        .await,
    );
    assert_eq!(
        reordered["workspaceIds"],
        json!([
            third["workspaceId"],
            first["workspaceId"],
            second["workspaceId"]
        ])
    );
    let missing_source = error(
        invoke(
            &service,
            RpcMethod::WorkspaceInsertBefore,
            json!({ "workspaceId": "missing" }),
        )
        .await,
    );
    assert_eq!(missing_source.code, "workspace-not-found");
    assert_eq!(missing_source.details["workspaceId"], "missing");
    let missing_anchor = error(
        invoke(
            &service,
            RpcMethod::WorkspaceInsertBefore,
            json!({
                "workspaceId": first["workspaceId"],
                "beforeWorkspaceId": "missing-anchor",
            }),
        )
        .await,
    );
    assert_eq!(missing_anchor.details["workspaceId"], "missing-anchor");
}

#[tokio::test]
async fn rename_delete_move_and_archive_map_only_business_failures() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Arc::new(MemoryWorkspace::default());
    let service = service(workspace.clone());
    let first = create_dir_workspace(&service, root.path(), "first").await;
    let second = create_dir_workspace(&service, root.path(), "second").await;
    let first_id = WorkspaceId::new(first["workspaceId"].as_str().unwrap());
    let second_id = second["workspaceId"].as_str().unwrap();

    let conflict = error(
        invoke(
            &service,
            RpcMethod::WorkspaceRename,
            json!({ "workspaceId": first_id, "title": " second " }),
        )
        .await,
    );
    assert_eq!(conflict.code, "workspace-name-conflict");
    assert_eq!(conflict.details["name"], "second");
    let missing = error(
        invoke(
            &service,
            RpcMethod::WorkspaceRename,
            json!({ "workspaceId": "missing", "title": "name" }),
        )
        .await,
    );
    assert_eq!(missing.code, "workspace-not-found");

    let one = SessionId::new("one");
    let two = SessionId::new("two");
    workspace.attach(&first_id, one.clone());
    workspace.attach(&first_id, two.clone());
    let moved = value(
        invoke(
            &service,
            RpcMethod::WorkspaceInsertSessionBefore,
            json!({
                "workspaceId": first_id,
                "sessionId": one,
                "beforeSessionId": two,
            }),
        )
        .await,
    );
    assert_eq!(moved["workspace"]["sessionIds"], json!(["one", "two"]));
    let invalid = error(
        invoke(
            &service,
            RpcMethod::WorkspaceInsertSessionBefore,
            json!({ "workspaceId": first_id, "sessionId": "ghost" }),
        )
        .await,
    );
    assert_eq!(invalid.code, "workspace-move-invalid");
    assert_eq!(invalid.details["sessionId"], "ghost");

    workspace.know_session(one.clone());
    let archived = value(
        invoke(
            &service,
            RpcMethod::WorkspaceArchiveSession,
            json!({ "sessionId": one }),
        )
        .await,
    );
    assert_eq!(archived["archivedSessionIds"], json!(["one"]));
    let unknown = error(
        invoke(
            &service,
            RpcMethod::WorkspaceArchiveSession,
            json!({ "sessionId": "ghost" }),
        )
        .await,
    );
    assert_eq!(unknown.code, "session-not-found");
    assert_eq!(unknown.details["sessionId"], "ghost");

    assert_eq!(
        value(
            invoke(
                &service,
                RpcMethod::WorkspaceDelete,
                json!({ "workspaceId": second_id }),
            )
            .await
        ),
        json!({ "deleted": true })
    );
    let missing_delete = error(
        invoke(
            &service,
            RpcMethod::WorkspaceDelete,
            json!({ "workspaceId": second_id }),
        )
        .await,
    );
    assert_eq!(missing_delete.code, "workspace-not-found");
}

#[tokio::test]
async fn host_stream_merges_only_committed_workspace_increments() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Arc::new(MemoryWorkspace::default());
    let service = service(workspace.clone());
    let signal = AbortSignal::default();
    let mut stream = service.host(
        RpcRequest::new(RpcId::new("host-stream"), json!({})),
        signal.clone(),
    );

    let first = create_dir_workspace(&service, root.path(), "first").await;
    let frame = stream.next().await.unwrap().unwrap();
    assert!(matches!(
        frame.payload,
        HostFrame::WorkspaceChanged { ref workspace }
            if workspace.workspace_id.as_str() == first["workspaceId"].as_str().unwrap()
    ));

    let second = create_dir_workspace(&service, root.path(), "second").await;
    assert!(matches!(
        stream.next().await.unwrap().unwrap().payload,
        HostFrame::WorkspaceChanged { .. }
    ));
    value(
        invoke(
            &service,
            RpcMethod::WorkspaceInsertBefore,
            json!({
                "workspaceId": first["workspaceId"],
                "beforeWorkspaceId": second["workspaceId"],
            }),
        )
        .await,
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap().payload,
        HostFrame::WorkspaceOrderChanged { .. }
    ));

    let session_id = SessionId::new("archived");
    workspace.know_session(session_id.clone());
    value(
        invoke(
            &service,
            RpcMethod::WorkspaceArchiveSession,
            json!({ "sessionId": session_id }),
        )
        .await,
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap().payload,
        HostFrame::ArchivedSessionsChanged { .. }
    ));

    value(
        invoke(
            &service,
            RpcMethod::WorkspaceDelete,
            json!({ "workspaceId": second["workspaceId"] }),
        )
        .await,
    );
    assert!(matches!(
        stream.next().await.unwrap().unwrap().payload,
        HostFrame::WorkspaceRemoved { .. }
    ));
    signal.abort();
    assert!(stream.next().await.is_none());
}
