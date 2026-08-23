//! Browser root Sessions service over the Rust manager, scope, and lifetime core.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use seekdeep_identity::SessionId;
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::future_to_promise;

use crate::{
    BrowserManagerTimer, BrowserSessionTransport, BrowserSpawner, ConversationNodeAssembler,
    RuntimeDisposer, RuntimeSessionListState, RuntimeSessionScope, SessionManager,
    SessionManagerOptions, SessionRuntime, SessionRuntimeOptions, SessionScopeFactory,
    SessionSelection, SessionSelectionStorage, SubagentAddress, WasmClientSession,
    WasmSessionManager, WasmSessionProvideChannel, create_client_scope,
    wasm_notifier::browser_notifier_scheduler,
    wasm_session::{
        console_error, js_to_json, json_to_js, optional, parse_subagent_address, render_js,
        required, required_string, rpc_error_to_js,
    },
};

const SELECTION_KEY: &str = "seekdeep.sessions.current";

struct BrowserSelectionStorage {
    memory: RefCell<SessionSelection>,
}

impl BrowserSelectionStorage {
    fn new() -> Self {
        let stored = storage_call("getItem", &[JsValue::from_str(SELECTION_KEY)])
            .ok()
            .and_then(|value| value.as_string())
            .and_then(|value| JSON::parse(&value).ok())
            .and_then(|value| parse_selection(&value).ok())
            .unwrap_or_default();
        Self {
            memory: RefCell::new(stored),
        }
    }
}

impl SessionSelectionStorage for BrowserSelectionStorage {
    fn load(&self) -> SessionSelection {
        self.memory.borrow().clone()
    }

    fn store(&self, selection: &SessionSelection) {
        *self.memory.borrow_mut() = selection.clone();
        if let Ok(value) = selection_to_js(selection)
            && let Ok(serialized) = JSON::stringify(&value)
        {
            let _ = storage_call(
                "setItem",
                &[JsValue::from_str(SELECTION_KEY), JsValue::from(serialized)],
            );
        }
    }

    fn clear(&self) {
        *self.memory.borrow_mut() = SessionSelection::default();
        let _ = storage_call("removeItem", &[JsValue::from_str(SELECTION_KEY)]);
    }
}

struct BrowserScopeFactory {
    root: JsValue,
    contexts: Rc<RefCell<HashMap<SessionId, JsValue>>>,
}

impl SessionScopeFactory for BrowserScopeFactory {
    fn create(&self, session_id: &SessionId) -> Rc<RuntimeSessionScope> {
        let handle = create_client_scope(self.root.clone(), session_id.as_str().to_owned())
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        let fiber = Reflect::get(&handle, &JsValue::from_str("fiber"))
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        let context = Reflect::get(&handle, &JsValue::from_str("ctx"))
            .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
        self.contexts
            .borrow_mut()
            .insert(session_id.clone(), context);
        let contexts = self.contexts.clone();
        let id = session_id.clone();
        RuntimeSessionScope::new(
            session_id.clone(),
            Value::Null,
            RuntimeDisposer::new(move || {
                contexts.borrow_mut().remove(&id);
                if let Ok(dispose) = Reflect::get(&fiber, &JsValue::from_str("dispose"))
                    && let Ok(dispose) = dispose.dyn_into::<Function>()
                {
                    let _ = dispose.call0(&fiber);
                }
            }),
        )
    }
}

#[derive(Default)]
struct BrowserFaceCaches {
    session_faces: RefCell<HashMap<SessionId, JsValue>>,
    binding_faces: RefCell<HashMap<SessionId, JsValue>>,
    provide_infos: RefCell<HashMap<SessionId, JsValue>>,
}

struct RuntimeBrowserState {
    runtime: Rc<SessionRuntime>,
    manager_face: WasmSessionManager,
    contexts: Rc<RefCell<HashMap<SessionId, JsValue>>>,
    faces: Rc<BrowserFaceCaches>,
    provide_channel: RefCell<Option<Rc<WasmSessionProvideChannel>>>,
    list_cache: RefCell<Option<(Rc<RuntimeSessionListState>, JsValue)>>,
}

/// Browser `SessionRuntime` backed by the Rust service core.
#[wasm_bindgen(js_name = SessionRuntime)]
pub struct WasmSessionRuntime {
    state: Rc<RuntimeBrowserState>,
    list_face: JsValue,
}

#[wasm_bindgen(js_class = SessionRuntime)]
impl WasmSessionRuntime {
    /// Creates the browser root Sessions service.
    ///
    /// # Errors
    ///
    /// Returns malformed restored selection or JavaScript face failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(root: JsValue, api: JsValue, remote: JsValue) -> Result<Self, JsValue> {
        let storage = Rc::new(BrowserSelectionStorage::new());
        let restored = storage.load();
        let manager = SessionManager::new(
            Rc::new(BrowserSessionTransport::new(api.clone(), remote)),
            restored.session_id.clone(),
            SessionManagerOptions {
                scheduler: browser_notifier_scheduler(),
                spawner: Rc::new(BrowserSpawner),
                timer: Rc::new(BrowserManagerTimer),
                resolve_time_zone: Rc::new(|| {
                    crate::resolved_client_time_zone_js().map_err(|error| render_js(&error))
                }),
                create_conversation: Rc::new(|| {
                    ConversationNodeAssembler::new(
                        Rc::new(ServiceEmptyEvents),
                        Rc::new(ServiceEmptyViews),
                    )
                }),
                clock: Rc::new(browser_now),
                report: Rc::new(|message| console_error(&message)),
            },
        );
        let contexts = Rc::new(RefCell::new(HashMap::new()));
        let faces = Rc::new(BrowserFaceCaches::default());
        let prune_root = root.clone();
        let prune_faces = faces.clone();
        let runtime = SessionRuntime::new(
            &manager,
            SessionRuntimeOptions {
                selection: storage,
                scopes: Rc::new(BrowserScopeFactory {
                    root: root.clone(),
                    contexts: contexts.clone(),
                }),
                spawner: Rc::new(BrowserSpawner),
                prune_store_scope: Rc::new(move |session_id| {
                    prune_faces.session_faces.borrow_mut().remove(session_id);
                    prune_faces.binding_faces.borrow_mut().remove(session_id);
                    prune_faces.provide_infos.borrow_mut().remove(session_id);
                    prune_browser_slot_scope(&prune_root, session_id);
                }),
            },
        );
        let state = Rc::new(RuntimeBrowserState {
            runtime,
            manager_face: WasmSessionManager::from_manager(manager, api),
            contexts,
            faces,
            provide_channel: RefCell::new(None),
            list_cache: RefCell::new(None),
        });
        let channel = Rc::new(WasmSessionProvideChannel::new(provide_host(&state)?)?);
        *state.provide_channel.borrow_mut() = Some(channel);
        let weak = Rc::downgrade(&state);
        let _subscription = state.runtime.subscribe(Rc::new(move || {
            if let Some(state) = weak.upgrade()
                && let Some(channel) = state.provide_channel.borrow().as_ref()
            {
                channel
                    .publish_current()
                    .unwrap_or_else(|error| wasm_bindgen::throw_val(error));
            }
        }));
        let list_face = list_face(&state)?;
        Ok(Self { state, list_face })
    }

    /// Root list observable.
    #[wasm_bindgen(getter)]
    pub fn list(&self) -> JsValue {
        self.list_face.clone()
    }

    /// Wire-owned search result bound exposed to presentation plugins.
    #[wasm_bindgen(getter, js_name = searchResultLimit)]
    pub fn search_result_limit(&self) -> usize {
        crate::SESSION_SEARCH_RESULT_LIMIT
    }

    /// Atomic current-Session provide observable.
    ///
    /// # Panics
    ///
    /// Panics only if construction returned without installing its provide channel.
    #[wasm_bindgen(getter, js_name = currentProvideInfo)]
    pub fn current_provide_info(&self) -> JsValue {
        self.state
            .provide_channel
            .borrow()
            .as_ref()
            .unwrap()
            .current_provide_info()
    }

    /// Registers one JavaScript per-Session provider.
    ///
    /// # Errors
    ///
    /// Returns provider validation or live materialization failures.
    ///
    /// # Panics
    ///
    /// Panics only if construction returned without installing its provide channel.
    #[allow(clippy::needless_pass_by_value)]
    pub fn provide(&self, descriptor: JsValue) -> Result<Function, JsValue> {
        self.state
            .provide_channel
            .borrow()
            .as_ref()
            .unwrap()
            .provide(descriptor)
    }

    /// Selects one listed Session.
    ///
    /// # Errors
    ///
    /// Returns unknown-session diagnostics.
    pub fn open(&self, session_id: String) -> Result<(), JsValue> {
        self.state
            .runtime
            .open(&SessionId::new(session_id))
            .map_err(|error| js_sys::Error::new(&error).into())
    }

    /// Selects one healthy direct child.
    ///
    /// # Errors
    ///
    /// Returns malformed address or catalog-validation diagnostics.
    #[wasm_bindgen(js_name = openSubagent)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn open_subagent(&self, address: JsValue) -> Result<(), JsValue> {
        self.state
            .runtime
            .open_subagent(parse_subagent_address(&address)?)
            .map_err(|error| js_sys::Error::new(&error).into())
    }

    /// Resolves one retained or loaded-catalog direct-parent address.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = subagentAddress)]
    pub fn subagent_address(&self, session_id: String) -> Result<JsValue, JsValue> {
        self.state
            .runtime
            .subagent_address(&SessionId::new(session_id))
            .as_ref()
            .map(selection_address_to_js)
            .transpose()
            .map(|address| address.unwrap_or(JsValue::UNDEFINED))
    }

    /// Marks whether one direct-child catalog is actively consumed.
    #[wasm_bindgen(js_name = setSubagentCatalogOpen)]
    pub fn set_subagent_catalog_open(&self, parent_session_id: String, open: bool) {
        self.state
            .runtime
            .set_subagent_catalog_open(&SessionId::new(parent_session_id), open);
    }

    /// Refreshes one direct-child catalog.
    #[wasm_bindgen(js_name = refreshSubagents)]
    pub fn refresh_subagents(&self, parent_session_id: String) -> Promise {
        let runtime = self.state.runtime.clone();
        let parent_session_id = SessionId::new(parent_session_id);
        future_to_promise(async move {
            runtime.refresh_subagents(&parent_session_id).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Records one Host-confirmed Agent preset switch.
    #[wasm_bindgen(js_name = noteAgentPreset)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn note_agent_preset(&self, session_id: String, agent_preset: String) {
        self.state
            .runtime
            .note_agent_preset(&SessionId::new(session_id), &agent_preset);
    }

    /// Clears current and persisted selection.
    pub fn clear(&self) {
        self.state.runtime.clear();
    }

    /// Refreshes the real Session baseline.
    pub fn refresh(&self) -> Promise {
        let runtime = self.state.runtime.clone();
        future_to_promise(async move {
            runtime.refresh().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Searches with the caller's exact `AbortSignal`.
    ///
    /// # Errors
    ///
    /// Returns JavaScript request-construction failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn search(&self, query: String, signal: JsValue) -> Result<Promise, JsValue> {
        self.state.manager_face.search(query, signal)
    }

    /// Creates one Session and returns its identity.
    ///
    /// # Errors
    ///
    /// Returns malformed options conversion failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn create(&self, options: JsValue) -> Result<Promise, JsValue> {
        let options = if options.is_undefined() {
            json!({})
        } else {
            js_to_json(&options)?
        };
        let runtime = self.state.runtime.clone();
        Ok(future_to_promise(async move {
            match runtime.create(options).await {
                Ok(session_id) => Ok(JsValue::from_str(session_id.as_str())),
                Err(error) => Err(session_create_error_to_js(&error)?),
            }
        }))
    }

    /// Forks one Session and returns its child identity.
    ///
    /// # Errors
    ///
    /// Returns malformed option diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn fork(&self, options: JsValue) -> Result<Promise, JsValue> {
        let session_id = required_string(&options, "sessionId", "sessions.fork options")?;
        let at_seq = optional(&options, "atSeq")?
            .map(|value| {
                value.as_f64().ok_or_else(|| {
                    JsValue::from(js_sys::Error::new(
                        "sessions.fork options atSeq must be a number",
                    ))
                })
            })
            .transpose()?;
        let increase_title = optional(&options, "increaseTitle")?
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    JsValue::from(js_sys::Error::new(
                        "sessions.fork options increaseTitle must be a boolean",
                    ))
                })
            })
            .transpose()?
            .unwrap_or(false);
        let runtime = self.state.runtime.clone();
        Ok(future_to_promise(async move {
            match runtime
                .fork(&SessionId::new(session_id), at_seq, increase_title)
                .await
            {
                Ok(session_id) => Ok(JsValue::from_str(session_id.as_str())),
                Err(error) => Err(session_fork_error_to_js(&error)?),
            }
        }))
    }

    /// Resolves one stable Session binding without staging it.
    ///
    /// # Errors
    ///
    /// Returns JavaScript Session or binding-face construction failures.
    pub fn binding(&self, session_id: String) -> Result<JsValue, JsValue> {
        binding_face(&self.state, &SessionId::new(session_id))
    }

    /// Resolves one Agent-scoped context without staging it.
    pub fn scope(&self, session_id: String) -> JsValue {
        let session_id = SessionId::new(session_id);
        let _ = self.state.runtime.scope(&session_id);
        self.state
            .contexts
            .borrow()
            .get(&session_id)
            .cloned()
            .unwrap_or(JsValue::UNDEFINED)
    }

    /// Reads the Client Agent-scope tag.
    #[wasm_bindgen(js_name = scopeOf)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn scope_of(&self, context: JsValue) -> Option<String> {
        crate::scope_of(context)
    }

    /// Returns the Session face behind one scoped context.
    ///
    /// # Errors
    ///
    /// Returns JavaScript Session or binding-face construction failures.
    #[wasm_bindgen(js_name = sessionOf)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn session_of(&self, context: JsValue) -> Result<JsValue, JsValue> {
        let Some(session_id) = crate::scope_of(context) else {
            return Ok(JsValue::UNDEFINED);
        };
        binding_face(&self.state, &SessionId::new(session_id))
            .map(|binding| Reflect::get(&binding, &JsValue::from_str("session")))?
    }

    /// Routes one raw mux envelope.
    ///
    /// # Errors
    ///
    /// Returns malformed known-frame diagnostics.
    #[wasm_bindgen(js_name = handleMuxEnvelope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn handle_mux_envelope(&self, envelope: JsValue) -> Result<(), JsValue> {
        self.state.manager_face.handle_mux_envelope(envelope)
    }

    /// Routes one raw Host envelope.
    ///
    /// # Errors
    ///
    /// Returns malformed known-frame diagnostics.
    #[wasm_bindgen(js_name = handleHostEnvelope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn handle_host_envelope(&self, envelope: JsValue) -> Result<(), JsValue> {
        self.state.manager_face.handle_host_envelope(envelope)
    }

    /// Starts one connected-generation refresh and resync fanout.
    #[wasm_bindgen(js_name = handleConnected)]
    pub fn handle_connected(&self) {
        self.state.manager_face.handle_connected();
    }

    /// Drops generation-scoped pending state.
    #[wasm_bindgen(js_name = handleDisconnected)]
    pub fn handle_disconnected(&self) {
        self.state.manager_face.handle_disconnected();
    }
}

fn provide_host(state: &Rc<RuntimeBrowserState>) -> Result<JsValue, JsValue> {
    let host = Object::new();
    let weak = Rc::downgrade(state);
    let rebuild = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let Some(state) = weak.upgrade() else {
            return Ok(JsValue::UNDEFINED);
        };
        let ids = state
            .faces
            .binding_faces
            .borrow()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let channel = state.provide_channel.borrow();
        let Some(channel) = channel.as_ref() else {
            return Ok(JsValue::UNDEFINED);
        };
        for id in ids {
            let binding = binding_face(&state, &id)?;
            let info = channel.materialize_info(binding)?;
            state.faces.provide_infos.borrow_mut().insert(id, info);
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&host, "rebuildBundles", &rebuild.into_js_value())?;
    let weak = Rc::downgrade(state);
    let resolve = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let Some(state) = weak.upgrade() else {
            return Ok(JsValue::UNDEFINED);
        };
        let current = state.runtime.list_snapshot().current.clone();
        let Some(current) = current else {
            return Ok(state
                .provide_channel
                .borrow()
                .as_ref()
                .unwrap()
                .maybe_info());
        };
        if let Some(info) = state.faces.provide_infos.borrow().get(&current) {
            return Ok(info.clone());
        }
        let binding = binding_face(&state, &current)?;
        let info = state
            .provide_channel
            .borrow()
            .as_ref()
            .unwrap()
            .materialize_info(binding)?;
        state
            .faces
            .provide_infos
            .borrow_mut()
            .insert(current, info.clone());
        Ok(info)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&host, "resolveCurrent", &resolve.into_js_value())?;
    Ok(host.into())
}

fn binding_face(
    state: &Rc<RuntimeBrowserState>,
    session_id: &SessionId,
) -> Result<JsValue, JsValue> {
    if let Some(binding) = state.faces.binding_faces.borrow().get(session_id) {
        return Ok(binding.clone());
    }
    let Some(binding) = state.runtime.binding(session_id) else {
        return Ok(JsValue::UNDEFINED);
    };
    let session = if let Some(session) = state.faces.session_faces.borrow().get(session_id) {
        session.clone()
    } else {
        let session: JsValue = WasmClientSession::from_session(binding.session.clone())?.into();
        state
            .faces
            .session_faces
            .borrow_mut()
            .insert(session_id.clone(), session.clone());
        session
    };
    let context = state
        .contexts
        .borrow()
        .get(session_id)
        .cloned()
        .unwrap_or(JsValue::UNDEFINED);
    let value = Object::new();
    set(&value, "sessionId", &JsValue::from_str(session_id.as_str()))?;
    set(&value, "session", &session)?;
    set(&value, "ctx", &context)?;
    let value: JsValue = value.into();
    state
        .faces
        .binding_faces
        .borrow_mut()
        .insert(session_id.clone(), value.clone());
    Ok(value)
}

fn list_face(state: &Rc<RuntimeBrowserState>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let state_for_snapshot = state.clone();
    let snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let current = state_for_snapshot.runtime.list_snapshot();
        if let Some((known, value)) = &*state_for_snapshot.list_cache.borrow()
            && Rc::ptr_eq(known, &current)
        {
            return Ok(value.clone());
        }
        let value = runtime_list_to_js(&current)?;
        *state_for_snapshot.list_cache.borrow_mut() = Some((current, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &snapshot.into_js_value())?;
    let runtime = state.runtime.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        let disposer = runtime.subscribe(Rc::new(move || {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                wasm_bindgen::throw_val(error);
            }
        }));
        Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into::<Function>()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn runtime_list_to_js(list: &RuntimeSessionListState) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let ids = Array::new();
    for id in list.ids.iter() {
        ids.push(&JsValue::from_str(id.as_str()));
    }
    set(&value, "ids", &ids)?;
    let by_id = Object::new();
    for (id, summary) in list.by_id.iter() {
        let row = Object::new();
        set(&row, "id", &JsValue::from_str(id.as_str()))?;
        set(
            &row,
            "displayTitle",
            &JsValue::from_str(&summary.display_title),
        )?;
        set_optional_string(&row, "title", summary.title.as_deref())?;
        set_optional_string(&row, "cwd", summary.cwd.as_deref())?;
        set_optional_string(&row, "agentPreset", summary.agent_preset.as_deref())?;
        set_optional_string(
            &row,
            "parentId",
            summary.parent_id.as_ref().map(SessionId::as_str),
        )?;
        set_optional_string(&row, "origin", summary.origin.as_deref())?;
        set(&row, "running", &JsValue::from_bool(summary.running))?;
        if summary.completed {
            set(&row, "completed", &JsValue::TRUE)?;
        }
        set(&row, "blank", &JsValue::from_bool(summary.blank))?;
        #[allow(clippy::cast_precision_loss)]
        set(
            &row,
            "updatedAt",
            &JsValue::from_f64(summary.updated_at as f64),
        )?;
        if let Some(pending) = &summary.pending_interaction {
            set(&row, "pendingInteraction", &json_to_js(pending)?)?;
        }
        if let Some(projections) = &summary.projection_values {
            set(&row, "projectionValues", &json_to_js(projections)?)?;
        }
        set(&by_id, id.as_str(), &row)?;
    }
    set(&value, "byId", &by_id)?;
    set(
        &value,
        "current",
        &list
            .current
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    set(
        &value,
        "phase",
        &JsValue::from_str(match list.phase {
            crate::SessionListPhase::Pending => "pending",
            crate::SessionListPhase::Ready => "ready",
        }),
    )?;
    let catalogs = Object::new();
    for (id, catalog) in list.subagents_by_parent.iter() {
        set(&catalogs, id.as_str(), &json_to_js(&catalog_json(catalog))?)?;
    }
    set(&value, "subagentsByParent", &catalogs)?;
    let jobs = Object::new();
    for (id, rows) in list.jobs_by_session.iter() {
        set(
            &jobs,
            id.as_str(),
            &json_to_js(&Value::Array(rows.as_ref().clone()))?,
        )?;
    }
    set(&value, "jobsBySession", &jobs)?;
    set(
        &value,
        "currentAddress",
        &list
            .current_address
            .as_ref()
            .map(selection_address_to_js)
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    Ok(value.into())
}

fn catalog_json(catalog: &crate::SubagentCatalogSnapshot) -> Value {
    json!({
        "entries":catalog.entries.iter().map(catalog_entry_json).collect::<Vec<_>>(),
        "parentAvailable":catalog.parent_available,
        "state":match catalog.state {
            crate::SubagentCatalogState::Loading => "loading",
            crate::SubagentCatalogState::Ready => "ready",
            crate::SubagentCatalogState::Error => "error",
        },
        "error":catalog.error.as_ref().map(|error| json!({
            "code":error.code,"message":error.message,"details":error.details
        }))
    })
}

fn catalog_entry_json(entry: &crate::SubagentCatalogEntry) -> Value {
    match entry {
        crate::SubagentCatalogEntry::Child {
            id,
            mode,
            label,
            running,
            has_children,
        } => {
            let mut row = serde_json::Map::from_iter([
                ("kind".to_owned(), json!("child")),
                ("id".to_owned(), json!(id.as_str())),
                (
                    "mode".to_owned(),
                    json!(match mode {
                        crate::SubagentMode::OneShot => "one-shot",
                        crate::SubagentMode::Continuable => "continuable",
                    }),
                ),
                (
                    "activity".to_owned(),
                    json!(if *running { "running" } else { "inactive" }),
                ),
                ("hasChildren".to_owned(), json!(has_children)),
            ]);
            if let Some(label) = label {
                row.insert("label".to_owned(), json!(label));
            }
            Value::Object(row)
        }
        crate::SubagentCatalogEntry::Diagnostic { id, reason } => {
            json!({"kind":"diagnostic","id":id.as_str(),"reason":reason})
        }
    }
}

fn parse_selection(value: &JsValue) -> Result<SessionSelection, JsValue> {
    Ok(SessionSelection {
        session_id: optional(value, "sessionId")?
            .and_then(|value| value.as_string())
            .map(SessionId::new),
        subagent_address: optional(value, "subagentAddress")?
            .as_ref()
            .map(parse_subagent_address)
            .transpose()?,
    })
}

fn selection_to_js(selection: &SessionSelection) -> Result<JsValue, JsValue> {
    let value = Object::new();
    if let Some(session_id) = &selection.session_id {
        set(&value, "sessionId", &JsValue::from_str(session_id.as_str()))?;
    }
    if let Some(address) = &selection.subagent_address {
        set(
            &value,
            "subagentAddress",
            &selection_address_to_js(address)?,
        )?;
    }
    Ok(value.into())
}

fn selection_address_to_js(address: &SubagentAddress) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "parentSessionId",
        &JsValue::from_str(address.parent_session_id.as_str()),
    )?;
    set(
        &value,
        "childSessionId",
        &JsValue::from_str(address.child_session_id.as_str()),
    )?;
    set(
        &value,
        "mode",
        &JsValue::from_str(match address.mode {
            crate::SubagentMode::OneShot => "one-shot",
            crate::SubagentMode::Continuable => "continuable",
        }),
    )?;
    Ok(value.into())
}

fn session_create_error_to_js(error: &crate::SessionCreateFailure) -> Result<JsValue, JsValue> {
    let value = Object::from(JsValue::from(js_sys::Error::new(&error.to_string())));
    set(&value, "name", &JsValue::from_str("SessionCreateError"))?;
    set(&value, "rpcError", &rpc_error_to_js(&error.error)?)?;
    set(
        &value,
        "requestedSessionId",
        &error
            .requested_session_id
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    Ok(value.into())
}

fn session_fork_error_to_js(error: &crate::SessionForkFailure) -> Result<JsValue, JsValue> {
    let value = Object::from(JsValue::from(js_sys::Error::new(&error.to_string())));
    if error.kind == crate::SessionForkFailureKind::Fork {
        set(&value, "name", &JsValue::from_str("SessionForkError"))?;
        set(&value, "rpcError", &rpc_error_to_js(&error.error)?)?;
        set(
            &value,
            "sourceSessionId",
            &JsValue::from_str(error.source_session_id.as_str()),
        )?;
    }
    Ok(value.into())
}

fn prune_browser_slot_scope(root: &JsValue, session_id: &SessionId) {
    let Ok(get) = Reflect::get(root, &JsValue::from_str("get")) else {
        return;
    };
    let Ok(get) = get.dyn_into::<Function>() else {
        return;
    };
    let slots = match get.call1(root, &JsValue::from_str("slots")) {
        Ok(slots) if !slots.is_undefined() && !slots.is_null() => slots,
        Ok(_) => return,
        Err(error) => {
            console_error(&format!(
                "sessions scope prune could not resolve slots: {}",
                render_js(&error)
            ));
            return;
        }
    };
    let prune = match Reflect::get(&slots, &JsValue::from_str("pruneStoreScope"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
    {
        Ok(prune) => prune,
        Err(error) => {
            console_error(&format!(
                "sessions scope prune requires slots.pruneStoreScope: {}",
                render_js(&error)
            ));
            return;
        }
    };
    if let Err(error) = prune.call1(&slots, &JsValue::from_str(session_id.as_str())) {
        console_error(&format!(
            "sessions scope prune failed for {session_id}: {}",
            render_js(&error)
        ));
    }
}

fn storage_call(method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    let storage = Reflect::get(&global, &JsValue::from_str("localStorage"))?;
    if storage.is_undefined() || storage.is_null() {
        return Ok(JsValue::UNDEFINED);
    }
    let method = required(&storage, method, "localStorage")?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(&storage, &args)
}

fn set_optional_string(object: &Object, key: &str, value: Option<&str>) -> Result<(), JsValue> {
    if let Some(value) = value {
        set(object, key, &JsValue::from_str(value))?;
    }
    Ok(())
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set SessionRuntime member {key:?}")).into())
    }
}

fn browser_now() -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    {
        js_sys::Date::now() as i64
    }
}

struct ServiceEmptyEvents;

impl crate::AssemblerEventDefinitions for ServiceEmptyEvents {
    fn entries(&self) -> Vec<Rc<crate::AssemblerNodeDefinition>> {
        Vec::new()
    }

    fn fallback_entry(&self) -> Option<Rc<crate::AssemblerNodeDefinition>> {
        None
    }
}

struct ServiceEmptyViews;

impl crate::AssemblerViewDefinitions for ServiceEmptyViews {
    fn entries(&self) -> Vec<Rc<crate::AssemblerViewDefinition>> {
        Vec::new()
    }
}
