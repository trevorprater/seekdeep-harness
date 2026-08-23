//! Browser `SessionManager` facade with cached Session and list identities.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use futures::future::LocalBoxFuture;
use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_identity::{RpcId, SessionId};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use crate::{
    AssemblerEventDefinitions, AssemblerViewDefinitions, ConversationNodeAssembler,
    ManagerHostFrame, ManagerListSnapshot, ManagerMuxEnvelope, ManagerMuxFrame,
    ManagerSessionSummary, RuntimeDisposer, SessionListPhase, SessionListState, SessionManager,
    SessionManagerOptions, SessionManagerTimer, SessionTaskSpawner, SubagentCatalogEntry,
    SubagentCatalogState, SubagentMode, WasmClientSession,
    wasm_notifier::browser_notifier_scheduler,
    wasm_session::{
        BrowserSessionTransport, call_path, console_error, js_to_json, json_to_js, optional,
        parse_mux_frame, parse_subagent_address, render_js, required, required_string,
        rpc_error_to_js, rpc_result_to_js, safe_i64, safe_u64,
    },
};

struct BrowserSpawner;

impl SessionTaskSpawner for BrowserSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

struct BrowserManagerTimer;

impl SessionManagerTimer for BrowserManagerTimer {
    fn schedule(&self, delay_ms: u64, callback: Box<dyn FnOnce()>) -> RuntimeDisposer {
        let global = js_sys::global();
        let callback = Closure::once_into_js(callback);
        let timer = Reflect::get(&global, &JsValue::from_str("setTimeout"))
            .ok()
            .and_then(|timer| timer.dyn_into::<Function>().ok());
        let Some(timer) = timer else {
            spawn_local(async move {
                let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
                let _ = callback
                    .unchecked_ref::<Function>()
                    .call0(&JsValue::UNDEFINED);
            });
            return RuntimeDisposer::new(|| {});
        };
        #[allow(clippy::cast_precision_loss)]
        let delay = JsValue::from_f64(delay_ms as f64);
        let id = timer
            .call2(&global, &callback, &delay)
            .ok()
            .and_then(|value| value.as_f64());
        RuntimeDisposer::new(move || {
            let Some(id) = id else {
                return;
            };
            if let Ok(clear) = Reflect::get(&global, &JsValue::from_str("clearTimeout"))
                && let Ok(clear) = clear.dyn_into::<Function>()
            {
                let _ = clear.call1(&global, &JsValue::from_f64(id));
            }
        })
    }
}

struct EmptyEvents;

impl AssemblerEventDefinitions for EmptyEvents {
    fn entries(&self) -> Vec<Rc<crate::AssemblerNodeDefinition>> {
        Vec::new()
    }

    fn fallback_entry(&self) -> Option<Rc<crate::AssemblerNodeDefinition>> {
        None
    }
}

struct EmptyViews;

impl AssemblerViewDefinitions for EmptyViews {
    fn entries(&self) -> Vec<Rc<crate::AssemblerViewDefinition>> {
        Vec::new()
    }
}

/// Browser manager backed by the portable Rust instance/list/catalog core.
#[wasm_bindgen(js_name = SessionManager)]
pub struct WasmSessionManager {
    manager: Rc<SessionManager>,
    api: JsValue,
    snapshot_cache: RefCell<Option<(Rc<ManagerListSnapshot>, JsValue)>>,
    session_cache: RefCell<HashMap<SessionId, JsValue>>,
}

#[wasm_bindgen(js_class = SessionManager)]
impl WasmSessionManager {
    /// Creates one browser manager over generated API and Remote namespaces.
    ///
    /// # Errors
    ///
    /// Returns malformed restored identities or address options.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        api: JsValue,
        remote: JsValue,
        restored_selection: Option<String>,
        restored_address: JsValue,
    ) -> Result<Self, JsValue> {
        let address = if restored_address.is_undefined() || restored_address.is_null() {
            None
        } else {
            Some(parse_subagent_address(&restored_address)?)
        };
        let manager = SessionManager::new(
            Rc::new(BrowserSessionTransport::new(api.clone(), remote)),
            restored_selection.map(SessionId::new),
            SessionManagerOptions {
                scheduler: browser_notifier_scheduler(),
                spawner: Rc::new(BrowserSpawner),
                timer: Rc::new(BrowserManagerTimer),
                resolve_time_zone: Rc::new(|| {
                    crate::resolved_client_time_zone_js().map_err(|error| render_js(&error))
                }),
                create_conversation: Rc::new(|| {
                    ConversationNodeAssembler::new(Rc::new(EmptyEvents), Rc::new(EmptyViews))
                }),
                clock: Rc::new(browser_now),
                report: Rc::new(|message| console_error(&message)),
            },
        );
        if let Some(address) = address {
            manager.retain_subagent_address(address);
        }
        Ok(Self {
            manager,
            api,
            snapshot_cache: RefCell::new(None),
            session_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Cached immutable list snapshot.
    ///
    /// # Errors
    ///
    /// Returns JavaScript snapshot construction failures.
    #[wasm_bindgen(js_name = getListSnapshot)]
    pub fn get_list_snapshot(&self) -> Result<JsValue, JsValue> {
        let snapshot = self.manager.snapshot();
        if let Some((current, value)) = &*self.snapshot_cache.borrow()
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = list_snapshot_to_js(&snapshot)?;
        *self.snapshot_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }

    /// Subscribes to committed list changes.
    pub fn subscribe(&self, listener: Function) -> Function {
        let disposer = self.manager.subscribe(Rc::new(move || {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                wasm_bindgen::throw_val(error);
            }
        }));
        Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }

    /// Lazily returns one identity-stable browser Session wrapper.
    ///
    /// # Errors
    ///
    /// Returns JavaScript Session-face construction failures.
    pub fn get(&self, session_id: String) -> Result<JsValue, JsValue> {
        let session_id = SessionId::new(session_id);
        if let Some(session) = self.session_cache.borrow().get(&session_id) {
            return Ok(session.clone());
        }
        let wrapper = WasmClientSession::from_session(self.manager.get(&session_id))?;
        let value: JsValue = wrapper.into();
        self.session_cache
            .borrow_mut()
            .insert(session_id, value.clone());
        Ok(value)
    }

    /// Drops one materialized Session wrapper and Rust instance.
    pub fn drop(&self, session_id: String) {
        let session_id = SessionId::new(session_id);
        self.session_cache.borrow_mut().remove(&session_id);
        self.manager.drop_session(&session_id);
    }

    /// Selects one listed or retained-address Session.
    ///
    /// # Errors
    ///
    /// Returns unknown-session diagnostics.
    pub fn select(&self, session_id: String) -> Result<(), JsValue> {
        self.manager
            .select(&SessionId::new(session_id))
            .map_err(|error| js_sys::Error::new(&error).into())
    }

    /// Selects one healthy direct child.
    ///
    /// # Errors
    ///
    /// Returns malformed address or catalog-validation diagnostics.
    #[wasm_bindgen(js_name = selectSubagent)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn select_subagent(&self, address: JsValue) -> Result<(), JsValue> {
        self.manager
            .select_subagent(parse_subagent_address(&address)?)
            .map_err(|error| js_sys::Error::new(&error).into())
    }

    /// Clears selection synchronously.
    #[wasm_bindgen(js_name = clearSelection)]
    pub fn clear_selection(&self) {
        self.manager.clear_selection();
    }

    /// Refreshes the authoritative Session list.
    #[wasm_bindgen(js_name = refreshList)]
    pub fn refresh_list(&self) -> Promise {
        let refresh = self.manager.refresh_list();
        future_to_promise(async move {
            refresh.await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Refreshes one direct-child catalog.
    #[wasm_bindgen(js_name = refreshSubagents)]
    pub fn refresh_subagents(&self, parent_session_id: String) -> Promise {
        let refresh = self
            .manager
            .refresh_subagents(&SessionId::new(parent_session_id));
        future_to_promise(async move {
            refresh.await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Marks one catalog menu open or closed.
    #[wasm_bindgen(js_name = setSubagentCatalogOpen)]
    pub fn set_subagent_catalog_open(&self, parent_session_id: String, open: bool) {
        self.manager
            .set_subagent_catalog_open(&SessionId::new(parent_session_id), open);
    }

    /// Searches with the caller's exact `AbortSignal` through the generated API.
    ///
    /// # Errors
    ///
    /// Returns JavaScript request-construction failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn search(&self, query: String, signal: JsValue) -> Result<Promise, JsValue> {
        let payload = Object::new();
        set(&payload, "query", &JsValue::from_str(&query))?;
        let request = call_path(
            &self.api,
            &["sessions", "search"],
            &[payload.into(), signal],
        )?;
        Ok(future_to_promise(async move {
            let response = JsFuture::from(Promise::resolve(&request)).await?;
            Reflect::get(&response, &JsValue::from_str("result"))
        }))
    }

    /// Creates one Session and publishes its local list echo.
    ///
    /// # Errors
    ///
    /// Returns malformed options conversion failures.
    #[allow(clippy::needless_pass_by_value)]
    pub fn create(&self, options: JsValue) -> Result<Promise, JsValue> {
        let options = js_to_json(&options)?;
        let manager = self.manager.clone();
        Ok(future_to_promise(async move {
            rpc_result_to_js(manager.create(options).await)
        }))
    }

    /// Forks one Session at an optional durable sequence.
    ///
    /// # Errors
    ///
    /// Returns an unsafe `atSeq` value.
    pub fn fork(&self, session_id: String, at_seq: Option<f64>) -> Result<Promise, JsValue> {
        let at_seq = at_seq
            .map(|value| safe_u64(&JsValue::from_f64(value), "fork atSeq"))
            .transpose()?;
        let manager = self.manager.clone();
        Ok(future_to_promise(async move {
            rpc_result_to_js(manager.fork(&SessionId::new(session_id), at_seq).await)
        }))
    }

    /// Records one Host-confirmed preset switch.
    #[wasm_bindgen(js_name = noteAgentPreset)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn note_agent_preset(&self, session_id: String, preset: String) {
        self.manager
            .note_agent_preset(&SessionId::new(session_id), &preset);
    }

    /// Routes one raw mux envelope.
    ///
    /// # Errors
    ///
    /// Returns malformed known-frame diagnostics.
    #[wasm_bindgen(js_name = handleMuxEnvelope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn handle_mux_envelope(&self, envelope: JsValue) -> Result<(), JsValue> {
        let rpc_id = RpcId::new(required_string(&envelope, "rpcId", "mux envelope")?);
        let payload = required(&envelope, "payload", "mux envelope")?;
        let session_id = SessionId::new(required_string(&payload, "sessionId", "mux frame")?);
        let frame_type = required_string(&payload, "type", "mux frame")?;
        let frame = match frame_type.as_str() {
            "session/projection" => ManagerMuxFrame::Projection {
                key: required_string(&payload, "key", "session/projection")?,
                value: Rc::new(js_to_json(&required(
                    &payload,
                    "value",
                    "session/projection",
                )?)?),
                seq: safe_i64(
                    &required(&payload, "seq", "session/projection")?,
                    "session/projection seq",
                )?,
            },
            "session/jobs" => {
                let jobs = js_to_json(&required(&payload, "jobs", "session/jobs")?)?
                    .as_array()
                    .cloned()
                    .ok_or_else(|| js_sys::Error::new("session/jobs jobs must be an array"))?;
                ManagerMuxFrame::Jobs(jobs)
            }
            "stream/error" => ManagerMuxFrame::StreamError,
            _ => ManagerMuxFrame::Session(parse_mux_frame(&payload)?),
        };
        self.manager.handle_mux_envelope(ManagerMuxEnvelope {
            rpc_id,
            session_id,
            frame,
        });
        Ok(())
    }

    /// Routes one raw Host envelope.
    ///
    /// # Errors
    ///
    /// Returns malformed known-frame diagnostics.
    #[wasm_bindgen(js_name = handleHostEnvelope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn handle_host_envelope(&self, envelope: JsValue) -> Result<(), JsValue> {
        let payload = required(&envelope, "payload", "Host envelope")?;
        self.manager.handle_host_frame(parse_host_frame(&payload)?);
        Ok(())
    }

    /// Drops generation-scoped pending state.
    #[wasm_bindgen(js_name = handleDisconnected)]
    pub fn handle_disconnected(&self) {
        self.manager.handle_disconnected();
    }

    /// Starts one connected-generation refresh and resync fanout.
    #[wasm_bindgen(js_name = handleConnected)]
    pub fn handle_connected(&self) {
        self.manager.handle_connected();
    }
}

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn list_snapshot_to_js(snapshot: &ManagerListSnapshot) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let items = Array::new();
    for entry in snapshot.items.iter() {
        let row = Object::new();
        let summary = &entry.summary;
        set(
            &row,
            "sessionId",
            &JsValue::from_str(summary.session_id.as_str()),
        )?;
        set(
            &row,
            "title",
            &summary
                .title
                .as_ref()
                .map_or(JsValue::UNDEFINED, |title| JsValue::from_str(title)),
        )?;
        set(
            &row,
            "updatedAt",
            &JsValue::from_f64(summary.updated_at as f64),
        )?;
        set(&row, "running", &JsValue::from_bool(summary.running))?;
        set(&row, "blank", &JsValue::from_bool(summary.blank))?;
        optional_string(
            &row,
            "parentSessionId",
            summary.parent_session_id.as_ref().map(SessionId::as_str),
        )?;
        optional_string(&row, "origin", summary.origin.as_deref())?;
        optional_string(&row, "cwd", summary.cwd.as_deref())?;
        optional_string(&row, "agentPreset", summary.agent_preset.as_deref())?;
        set(&row, "depth", &JsValue::from_f64(entry.depth as f64))?;
        set(
            &row,
            "pendingInteraction",
            &entry
                .pending_interaction
                .as_ref()
                .map(json_to_js)
                .transpose()?
                .unwrap_or(JsValue::UNDEFINED),
        )?;
        set(&row, "completed", &JsValue::from_bool(entry.completed))?;
        set(
            &row,
            "projectionValues",
            &summary
                .projection_values
                .as_ref()
                .map(json_to_js)
                .transpose()?
                .unwrap_or(JsValue::UNDEFINED),
        )?;
        items.push(&row);
    }
    set(&value, "items", &items)?;
    set(
        &value,
        "current",
        &snapshot
            .current
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    set(
        &value,
        "state",
        &JsValue::from_str(match snapshot.state {
            SessionListState::Idle => "idle",
            SessionListState::Loading => "loading",
            SessionListState::Error => "error",
        }),
    )?;
    set(
        &value,
        "phase",
        &JsValue::from_str(match snapshot.phase {
            SessionListPhase::Pending => "pending",
            SessionListPhase::Ready => "ready",
        }),
    )?;
    set(
        &value,
        "error",
        &snapshot
            .error
            .as_ref()
            .map(rpc_error_to_js)
            .transpose()?
            .unwrap_or(JsValue::NULL),
    )?;
    let catalogs = Object::new();
    for (parent, catalog) in snapshot.subagents_by_parent.iter() {
        set(&catalogs, parent.as_str(), &catalog_to_js(catalog)?)?;
    }
    set(&value, "subagentsByParent", &catalogs)?;
    let jobs = Object::new();
    for (session, rows) in snapshot.jobs_by_session.iter() {
        set(
            &jobs,
            session.as_str(),
            &json_to_js(&Value::Array(rows.as_ref().clone()))?,
        )?;
    }
    set(&value, "jobsBySession", &jobs)?;
    set(
        &value,
        "currentAddress",
        &snapshot
            .current_address
            .as_ref()
            .map(address_to_js)
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )?;
    Ok(value.into())
}

fn catalog_to_js(catalog: &crate::SubagentCatalogSnapshot) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let entries = Array::new();
    for entry in catalog.entries.iter() {
        let row = Object::new();
        match entry {
            SubagentCatalogEntry::Child {
                id,
                mode,
                label,
                running,
                has_children,
            } => {
                set(&row, "kind", &JsValue::from_str("child"))?;
                set(&row, "id", &JsValue::from_str(id.as_str()))?;
                set(
                    &row,
                    "mode",
                    &JsValue::from_str(match mode {
                        SubagentMode::OneShot => "one-shot",
                        SubagentMode::Continuable => "continuable",
                    }),
                )?;
                optional_string(&row, "label", label.as_deref())?;
                set(
                    &row,
                    "activity",
                    &JsValue::from_str(if *running { "running" } else { "inactive" }),
                )?;
                set(&row, "hasChildren", &JsValue::from_bool(*has_children))?;
            }
            SubagentCatalogEntry::Diagnostic { id, reason } => {
                set(&row, "kind", &JsValue::from_str("diagnostic"))?;
                set(&row, "id", &JsValue::from_str(id.as_str()))?;
                set(&row, "reason", &JsValue::from_str(reason))?;
            }
        }
        entries.push(&row);
    }
    set(&value, "entries", &entries)?;
    set(
        &value,
        "parentAvailable",
        &JsValue::from_bool(catalog.parent_available),
    )?;
    set(
        &value,
        "state",
        &JsValue::from_str(match catalog.state {
            SubagentCatalogState::Loading => "loading",
            SubagentCatalogState::Ready => "ready",
            SubagentCatalogState::Error => "error",
        }),
    )?;
    set(
        &value,
        "error",
        &catalog
            .error
            .as_ref()
            .map(rpc_error_to_js)
            .transpose()?
            .unwrap_or(JsValue::NULL),
    )?;
    Ok(value.into())
}

fn parse_host_frame(value: &JsValue) -> Result<ManagerHostFrame, JsValue> {
    let frame_type = required_string(value, "type", "Host frame")?;
    match frame_type.as_str() {
        "host/session-added" => Ok(ManagerHostFrame::Added(ManagerSessionSummary {
            session_id: SessionId::new(required_string(value, "sessionId", "host/session-added")?),
            updated_at: 0,
            running: false,
            blank: required(value, "blank", "host/session-added")?
                .as_bool()
                .unwrap_or(false),
            parent_session_id: optional(value, "parentSessionId")?
                .and_then(|value| value.as_string())
                .map(SessionId::new),
            origin: optional(value, "origin")?.and_then(|value| value.as_string()),
            cwd: optional(value, "cwd")?.and_then(|value| value.as_string()),
            agent_preset: optional(value, "agentPreset")?.and_then(|value| value.as_string()),
            projections: None,
        })),
        "host/session-removed" => Ok(ManagerHostFrame::Removed {
            session_id: SessionId::new(required_string(
                value,
                "sessionId",
                "host/session-removed",
            )?),
        }),
        "host/session-status" => Ok(ManagerHostFrame::Status {
            session_id: SessionId::new(required_string(value, "sessionId", "host/session-status")?),
            running: required(value, "running", "host/session-status")?
                .as_bool()
                .unwrap_or(false),
        }),
        "host/agent-error" => Ok(ManagerHostFrame::AgentError {
            session_id: SessionId::new(required_string(value, "sessionId", "host/agent-error")?),
            message: required_string(value, "message", "host/agent-error")?,
        }),
        _ => Ok(ManagerHostFrame::Unknown),
    }
}

fn address_to_js(address: &crate::SubagentAddress) -> Result<JsValue, JsValue> {
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
            SubagentMode::OneShot => "one-shot",
            SubagentMode::Continuable => "continuable",
        }),
    )?;
    Ok(value.into())
}

fn optional_string(object: &Object, key: &str, value: Option<&str>) -> Result<(), JsValue> {
    set(
        object,
        key,
        &value.map_or(JsValue::UNDEFINED, JsValue::from_str),
    )
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set SessionManager member {key:?}")).into())
    }
}

fn browser_now() -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    {
        js_sys::Date::now() as i64
    }
}
