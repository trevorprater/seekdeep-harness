//! Browser Workspaces service over the Rust entity, manager, and selection core.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_identity::{SessionId, WorkspaceId};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    BrowserSessionTransport, BrowserSpawner, ClientRpcError, ClientWorkspaceView,
    DirectoryBrowseCallFailure, DirectoryBrowseFailure, RuntimeDisposer, RuntimeWorkspaceListState,
    SessionRuntimeWorkspacePort, WasmSessionRuntime, WorkspaceActionFailure,
    WorkspaceConnectFailure, WorkspaceCreateFailure, WorkspaceCreateInput, WorkspaceHostFrame,
    WorkspaceListPhase, WorkspaceListState, WorkspaceManagerOptions, WorkspaceRuntime,
    WorkspaceRuntimeOptions, WorkspaceSessionsPort,
    wasm_notifier::browser_notifier_scheduler,
    wasm_session::{
        call_path, console_error, js_to_json, parse_rpc_error, required, required_string,
        rpc_error_to_js,
    },
    wasm_session_service::session_create_error_to_js,
};

type WorkspaceItems = Rc<Vec<Rc<ClientWorkspaceView>>>;

struct BrowserWorkspaceState {
    runtime: Rc<WorkspaceRuntime>,
    api: JsValue,
    list_cache: RefCell<Option<(Rc<RuntimeWorkspaceListState>, JsValue)>>,
    items_cache: RefCell<Option<(WorkspaceItems, Array)>>,
    archived_cache: RefCell<Option<(Rc<Vec<SessionId>>, Array)>>,
    view_cache: RefCell<HashMap<WorkspaceId, (Rc<ClientWorkspaceView>, JsValue)>>,
    connect_promises: RefCell<HashMap<WorkspaceId, Promise>>,
    refresh_promise: RefCell<Option<Promise>>,
}

/// Browser `WorkspaceRuntime` backed by the Rust object layer.
#[wasm_bindgen(js_name = WorkspaceRuntime)]
pub struct WasmWorkspaceRuntime {
    state: Rc<BrowserWorkspaceState>,
    list_face: JsValue,
}

#[wasm_bindgen(js_class = WorkspaceRuntime)]
impl WasmWorkspaceRuntime {
    /// Creates the browser Workspaces service over the same Rust Sessions core.
    ///
    /// # Errors
    ///
    /// Returns JavaScript observable-face construction failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        _root: JsValue,
        api: JsValue,
        sessions: &WasmSessionRuntime,
    ) -> Result<Self, JsValue> {
        let transport = Rc::new(BrowserSessionTransport::new(
            api.clone(),
            JsValue::UNDEFINED,
        ));
        let sessions_port: Rc<dyn WorkspaceSessionsPort> =
            SessionRuntimeWorkspacePort::new(sessions.core_runtime());
        let runtime = WorkspaceRuntime::new(
            transport,
            &sessions_port,
            WorkspaceRuntimeOptions {
                manager: WorkspaceManagerOptions {
                    scheduler: browser_notifier_scheduler(),
                    spawner: Rc::new(BrowserSpawner),
                    parse_date: Rc::new(js_sys::Date::parse),
                },
                spawner: Rc::new(BrowserSpawner),
                report: Rc::new(console_error),
            },
        );
        let state = Rc::new(BrowserWorkspaceState {
            runtime,
            api,
            list_cache: RefCell::new(None),
            items_cache: RefCell::new(None),
            archived_cache: RefCell::new(None),
            view_cache: RefCell::new(HashMap::new()),
            connect_promises: RefCell::new(HashMap::new()),
            refresh_promise: RefCell::new(None),
        });
        let list_face = list_face(&state)?;
        Ok(Self { state, list_face })
    }

    /// UI-facing Workspace list observable.
    #[wasm_bindgen(getter)]
    pub fn list(&self) -> JsValue {
        self.list_face.clone()
    }

    /// Resolves or creates the blank Session used to enter one Workspace.
    pub fn connect_workspace(&self, workspace_id: String) -> Promise {
        let workspace_id = WorkspaceId::new(workspace_id);
        if let Some(promise) = self.state.connect_promises.borrow().get(&workspace_id) {
            return promise.clone();
        }
        let task = self.state.runtime.connect_workspace(&workspace_id);
        let cache = self.state.runtime.has_inflight_connect(&workspace_id);
        let weak = Rc::downgrade(&self.state);
        let id = workspace_id.clone();
        let promise = future_to_promise(async move {
            let result = task.await;
            if cache && let Some(state) = weak.upgrade() {
                state.connect_promises.borrow_mut().remove(&id);
            }
            match result {
                Ok(session_id) => Ok(JsValue::from_str(session_id.as_str())),
                Err(WorkspaceConnectFailure::SessionCreate(error)) => {
                    Err(session_create_error_to_js(&error)?)
                }
                Err(error) => Err(js_sys::Error::new(&error.to_string()).into()),
            }
        });
        if cache {
            self.state
                .connect_promises
                .borrow_mut()
                .insert(workspace_id, promise.clone());
        }
        promise
    }

    /// Starts the one-shot initial selection policy.
    ///
    /// # Errors
    ///
    /// Returns when the policy was already started.
    #[wasm_bindgen(js_name = startInitialSelection)]
    pub fn start_initial_selection(&self) -> Result<Function, JsValue> {
        self.state
            .runtime
            .start_initial_selection()
            .map(runtime_disposer)
            .map_err(|error| js_sys::Error::new(&error).into())
    }

    /// Starts one New Session flow.
    #[wasm_bindgen(js_name = startSession)]
    pub fn start_session(&self, workspace_id: Option<String>) {
        self.state
            .runtime
            .start_session(workspace_id.map(WorkspaceId::new));
    }

    /// Registers an existing path as a Workspace.
    ///
    /// # Errors
    ///
    /// Returns malformed input diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn create(&self, input: JsValue) -> Result<Promise, JsValue> {
        let path = required_string(&input, "path", "workspaces.create input")?;
        let runtime = self.state.runtime.clone();
        let state = self.state.clone();
        Ok(future_to_promise(async move {
            match runtime.create(WorkspaceCreateInput { path }).await {
                Ok(workspace) => workspace_view_to_js(&state, &workspace),
                Err(error) => Err(workspace_create_error_to_js(&error)?),
            }
        }))
    }

    /// Opens the Host native directory picker.
    pub fn pick_directory(&self) -> Promise {
        let runtime = self.state.runtime.clone();
        future_to_promise(async move {
            match runtime.pick_directory().await {
                Ok(Some(path)) => Ok(JsValue::from_str(&path)),
                Ok(None) => Ok(JsValue::NULL),
                Err(error) => Err(workspace_action_error_to_js(&error)),
            }
        })
    }

    /// Lists one directory level with the caller's exact `AbortSignal`.
    #[wasm_bindgen(js_name = listDirectory)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn list_directory(&self, path: Option<String>, signal: JsValue) -> Promise {
        let api = self.state.api.clone();
        future_to_promise(async move {
            let payload = Object::new();
            if let Some(path) = path {
                set(&payload, "path", &JsValue::from_str(&path))?;
            }
            match call_browser_rpc(&api, &["host", "listDirectory"], &[payload.into(), signal])
                .await?
            {
                Ok(value) => Ok(value),
                Err(error) => Err(directory_browse_error_to_js(&DirectoryBrowseFailure {
                    rpc_error: error,
                })?),
            }
        })
    }

    /// Creates one child directory.
    #[wasm_bindgen(js_name = createDirectory)]
    pub fn create_directory(&self, path: String, name: String) -> Promise {
        let runtime = self.state.runtime.clone();
        future_to_promise(async move {
            match runtime.create_directory(&path, &name).await {
                Ok(path) => Ok(JsValue::from_str(&path)),
                Err(error) => Err(directory_call_error_to_js(&error)?),
            }
        })
    }

    /// Opens one filesystem path with the Host operating system.
    #[wasm_bindgen(js_name = openPath)]
    pub fn open_path(&self, path: String) -> Promise {
        let runtime = self.state.runtime.clone();
        future_to_promise(async move {
            runtime
                .open_path(&path)
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|error| workspace_action_error_to_js(&error))
        })
    }

    /// Renames one Workspace.
    pub fn rename(&self, workspace_id: String, title: String) -> Promise {
        let runtime = self.state.runtime.clone();
        let state = self.state.clone();
        future_to_promise(async move {
            match runtime
                .rename(&WorkspaceId::new(workspace_id), &title)
                .await
            {
                Ok(workspace) => workspace_view_to_js(&state, &workspace),
                Err(error) => Err(workspace_action_error_to_js(&error)),
            }
        })
    }

    /// Deletes one Workspace registration.
    pub fn delete(&self, workspace_id: String) -> Promise {
        let runtime = self.state.runtime.clone();
        let state = self.state.clone();
        future_to_promise(async move {
            let workspace_id = WorkspaceId::new(workspace_id);
            match runtime.delete(&workspace_id).await {
                Ok(()) => {
                    state.view_cache.borrow_mut().remove(&workspace_id);
                    Ok(JsValue::UNDEFINED)
                }
                Err(error) => Err(workspace_action_error_to_js(&error)),
            }
        })
    }

    /// Moves one Workspace in durable order.
    #[wasm_bindgen(js_name = insertBefore)]
    pub fn insert_before(
        &self,
        workspace_id: String,
        before_workspace_id: Option<String>,
    ) -> Promise {
        let runtime = self.state.runtime.clone();
        future_to_promise(async move {
            let before = before_workspace_id.map(WorkspaceId::new);
            runtime
                .insert_before(&WorkspaceId::new(workspace_id), before.as_ref())
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|error| workspace_action_error_to_js(&error))
        })
    }

    /// Archives one Session.
    #[wasm_bindgen(js_name = archiveSession)]
    pub fn archive_session(&self, session_id: String) -> Promise {
        let runtime = self.state.runtime.clone();
        future_to_promise(async move {
            runtime
                .archive_session(&SessionId::new(session_id))
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|error| workspace_action_error_to_js(&error))
        })
    }

    /// Moves one Session inside its Workspace.
    #[wasm_bindgen(js_name = insertSessionBefore)]
    pub fn insert_session_before(
        &self,
        workspace_id: String,
        session_id: String,
        before_session_id: Option<String>,
    ) -> Promise {
        let runtime = self.state.runtime.clone();
        let state = self.state.clone();
        future_to_promise(async move {
            let before = before_session_id.map(SessionId::new);
            match runtime
                .insert_session_before(
                    &WorkspaceId::new(workspace_id),
                    &SessionId::new(session_id),
                    before.as_ref(),
                )
                .await
            {
                Ok(workspace) => workspace_view_to_js(&state, &workspace),
                Err(error) => Err(workspace_action_error_to_js(&error)),
            }
        })
    }

    /// Refreshes the Workspace baseline, sharing one JavaScript Promise.
    pub fn refresh(&self) -> Promise {
        if let Some(promise) = self.state.refresh_promise.borrow().as_ref() {
            return promise.clone();
        }
        let refresh = self.state.runtime.refresh();
        let weak = Rc::downgrade(&self.state);
        let promise = future_to_promise(async move {
            refresh.await;
            if let Some(state) = weak.upgrade() {
                state.refresh_promise.borrow_mut().take();
            }
            Ok(JsValue::UNDEFINED)
        });
        *self.state.refresh_promise.borrow_mut() = Some(promise.clone());
        promise
    }

    /// Routes one raw Host envelope.
    ///
    /// # Errors
    ///
    /// Returns malformed known Workspace-frame diagnostics.
    #[wasm_bindgen(js_name = handleHostEnvelope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn handle_host_envelope(&self, envelope: JsValue) -> Result<(), JsValue> {
        if let Some(frame) = parse_workspace_host_frame(&envelope)? {
            if let WorkspaceHostFrame::Removed(workspace_id) = &frame {
                self.state.view_cache.borrow_mut().remove(workspace_id);
            }
            self.state.runtime.handle_host_frame(frame);
        }
        Ok(())
    }

    /// Rebuilds the Workspace baseline after connection.
    #[wasm_bindgen(js_name = handleConnected)]
    pub fn handle_connected(&self) {
        self.state.runtime.handle_connected();
    }
}

fn list_face(state: &Rc<BrowserWorkspaceState>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let state_for_snapshot = state.clone();
    let snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let current = state_for_snapshot.runtime.list_snapshot();
        if let Some((known, value)) = &*state_for_snapshot.list_cache.borrow()
            && Rc::ptr_eq(known, &current)
        {
            return Ok(value.clone());
        }
        let value = runtime_list_to_js(&state_for_snapshot, &current)?;
        *state_for_snapshot.list_cache.borrow_mut() = Some((current, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &snapshot.into_js_value())?;
    let runtime = state.runtime.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        runtime_disposer(runtime.subscribe(Rc::new(move || {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                wasm_bindgen::throw_val(error);
            }
        })))
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn runtime_list_to_js(
    state: &BrowserWorkspaceState,
    list: &RuntimeWorkspaceListState,
) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let items = if let Some((known, items)) = &*state.items_cache.borrow()
        && Rc::ptr_eq(known, &list.items)
    {
        items.clone()
    } else {
        let items = Array::new();
        for workspace in list.items.iter() {
            items.push(&workspace_view_to_js(state, workspace)?);
        }
        *state.items_cache.borrow_mut() = Some((list.items.clone(), items.clone()));
        items
    };
    set(&value, "items", &items)?;
    let archived = if let Some((known, archived)) = &*state.archived_cache.borrow()
        && Rc::ptr_eq(known, &list.archived_session_ids)
    {
        archived.clone()
    } else {
        let archived = Array::new();
        for session_id in list.archived_session_ids.iter() {
            archived.push(&JsValue::from_str(session_id.as_str()));
        }
        *state.archived_cache.borrow_mut() =
            Some((list.archived_session_ids.clone(), archived.clone()));
        archived
    };
    set(&value, "archivedSessionIds", &archived)?;
    set(
        &value,
        "state",
        &JsValue::from_str(match list.state {
            WorkspaceListState::Idle => "idle",
            WorkspaceListState::Loading => "loading",
            WorkspaceListState::Error => "error",
        }),
    )?;
    set(
        &value,
        "phase",
        &JsValue::from_str(match list.phase {
            WorkspaceListPhase::Pending => "pending",
            WorkspaceListPhase::Ready => "ready",
        }),
    )?;
    set(
        &value,
        "error",
        &list
            .error
            .as_ref()
            .map(rpc_error_to_js)
            .transpose()?
            .unwrap_or(JsValue::NULL),
    )?;
    set(
        &value,
        "baselinesReady",
        &JsValue::from_bool(list.baselines_ready),
    )?;
    set(
        &value,
        "recentWorkspaceId",
        &list
            .recent_workspace_id
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    Ok(value.into())
}

fn workspace_view_to_js(
    state: &BrowserWorkspaceState,
    workspace: &Rc<ClientWorkspaceView>,
) -> Result<JsValue, JsValue> {
    if let Some((known, value)) = state.view_cache.borrow().get(&workspace.workspace_id)
        && Rc::ptr_eq(known, workspace)
    {
        return Ok(value.clone());
    }
    let value = workspace_view_to_js_uncached(workspace)?;
    state.view_cache.borrow_mut().insert(
        workspace.workspace_id.clone(),
        (workspace.clone(), value.clone()),
    );
    Ok(value)
}

fn workspace_view_to_js_uncached(workspace: &ClientWorkspaceView) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "workspaceId",
        &JsValue::from_str(workspace.workspace_id.as_str()),
    )?;
    set(&value, "path", &JsValue::from_str(&workspace.path))?;
    set(&value, "title", &JsValue::from_str(&workspace.title))?;
    let sessions = Array::new();
    for session_id in &workspace.session_ids {
        sessions.push(&JsValue::from_str(session_id.as_str()));
    }
    set(&value, "sessionIds", &sessions)?;
    set(
        &value,
        "createdAt",
        &JsValue::from_str(&workspace.created_at),
    )?;
    set(
        &value,
        "updatedAt",
        &JsValue::from_str(&workspace.updated_at),
    )?;
    Ok(value.into())
}

pub(crate) fn parse_workspace_host_frame(
    envelope: &JsValue,
) -> Result<Option<WorkspaceHostFrame>, JsValue> {
    let payload = required(envelope, "payload", "Host envelope")?;
    let frame_type = required_string(&payload, "type", "Host frame")?;
    match frame_type.as_str() {
        "host/workspace-changed" => {
            let workspace = required(&payload, "workspace", "workspace changed frame")?;
            let workspace: ClientWorkspaceView = serde_json::from_value(js_to_json(&workspace)?)
                .map_err(|error| js_sys::Error::new(&error.to_string()))?;
            Ok(Some(WorkspaceHostFrame::Changed(Rc::new(workspace))))
        }
        "host/workspace-removed" => Ok(Some(WorkspaceHostFrame::Removed(WorkspaceId::new(
            required_string(&payload, "workspaceId", "workspace removed frame")?,
        )))),
        "host/workspace-order-changed" => Ok(Some(WorkspaceHostFrame::OrderChanged(Rc::new(
            string_array(&payload, "workspaceIds", "workspace order frame")?
                .into_iter()
                .map(WorkspaceId::new)
                .collect(),
        )))),
        "host/archived-sessions-changed" => {
            Ok(Some(WorkspaceHostFrame::ArchivedSessionsChanged(Rc::new(
                string_array(&payload, "archivedSessionIds", "archived sessions frame")?
                    .into_iter()
                    .map(SessionId::new)
                    .collect(),
            ))))
        }
        _ => Ok(None),
    }
}

async fn call_browser_rpc(
    api: &JsValue,
    path: &[&str],
    arguments: &[JsValue],
) -> Result<Result<JsValue, ClientRpcError>, JsValue> {
    let response = call_path(api, path, arguments)?;
    let response = JsFuture::from(Promise::resolve(&response)).await?;
    let result = Reflect::get(&response, &JsValue::from_str("result"))
        .ok()
        .filter(|value| !value.is_undefined())
        .unwrap_or(response);
    let ok = required(&result, "ok", "RPC result")?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new("RPC result ok must be boolean"))?;
    if ok {
        Ok(Ok(required(&result, "value", "RPC success")?))
    } else {
        Ok(Err(parse_rpc_error(&required(
            &result,
            "error",
            "RPC failure",
        )?)?))
    }
}

fn workspace_create_error_to_js(error: &WorkspaceCreateFailure) -> Result<JsValue, JsValue> {
    let rpc_error = rpc_error_to_js(&error.rpc_error)?;
    if let Some(value) = crate::wasm_public_api::construct_public_error(
        "WorkspaceCreateError",
        std::slice::from_ref(&rpc_error),
    ) {
        return Ok(value);
    }
    let value = Object::from(JsValue::from(js_sys::Error::new(&error.to_string())));
    set(&value, "name", &JsValue::from_str("WorkspaceCreateError"))?;
    set(&value, "rpcError", &rpc_error)?;
    Ok(value.into())
}

fn directory_browse_error_to_js(error: &DirectoryBrowseFailure) -> Result<JsValue, JsValue> {
    let rpc_error = rpc_error_to_js(&error.rpc_error)?;
    if let Some(value) = crate::wasm_public_api::construct_public_error(
        "DirectoryBrowseError",
        std::slice::from_ref(&rpc_error),
    ) {
        return Ok(value);
    }
    let value = Object::from(JsValue::from(js_sys::Error::new(&error.to_string())));
    set(&value, "name", &JsValue::from_str("DirectoryBrowseError"))?;
    set(&value, "rpcError", &rpc_error)?;
    Ok(value.into())
}

fn directory_call_error_to_js(error: &DirectoryBrowseCallFailure) -> Result<JsValue, JsValue> {
    match error {
        DirectoryBrowseCallFailure::Business(error) => directory_browse_error_to_js(error),
        DirectoryBrowseCallFailure::Transport(error) => Ok(js_sys::Error::new(error).into()),
    }
}

fn workspace_action_error_to_js(error: &WorkspaceActionFailure) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

fn string_array(value: &JsValue, key: &str, owner: &str) -> Result<Vec<String>, JsValue> {
    let value = required(value, key, owner)?;
    if !Array::is_array(&value) {
        return Err(js_sys::Error::new(&format!("{owner} {key} must be an array")).into());
    }
    Array::from(&value)
        .iter()
        .map(|value| {
            value.as_string().ok_or_else(|| {
                js_sys::Error::new(&format!("{owner} {key} entries must be strings")).into()
            })
        })
        .collect()
}

fn runtime_disposer(disposer: RuntimeDisposer) -> Function {
    Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set WorkspaceRuntime member {key:?}")).into())
    }
}
