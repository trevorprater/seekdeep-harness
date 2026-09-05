//! Browser `Session` facade and generated API/Remote transport adapter.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use futures::{FutureExt, future::LocalBoxFuture};
use indexmap::IndexMap;
use js_sys::{Array, Function, Map as JsMap, Object, Promise, Reflect};
use seekdeep_identity::{MessageId, RpcId, SessionId};
use serde_json::{Map as JsonMap, Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use crate::{
    ClientRpcError, ClientRpcResult, ClientSession, ComposerPhase, PendingWait, ProjectionFace,
    ProjectionsBaseline, PromptOperation, QueueItemInput, QueuePlacement, SessionHistoryEntry,
    SessionHistoryPage, SessionHistoryRequest, SessionMuxFrame, SessionOpenState, SessionOptions,
    SessionPromptError, SessionSnapshot, SessionTaskSpawner, SessionTransport,
    SessionTransportRequest, SubagentAddress, SubagentMode, resolved_client_time_zone_js,
    wasm_notifier::browser_notifier_scheduler,
};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_SAFE_INTEGER_U64: u64 = 9_007_199_254_740_991;

struct BrowserSessionSpawner;

impl SessionTaskSpawner for BrowserSessionSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

pub(crate) struct BrowserSessionTransport {
    api: JsValue,
    remote: JsValue,
}

impl BrowserSessionTransport {
    pub(crate) fn new(api: JsValue, remote: JsValue) -> Self {
        Self { api, remote }
    }
}

impl SessionTransport for BrowserSessionTransport {
    fn history(
        &self,
        request: SessionHistoryRequest,
    ) -> LocalBoxFuture<'static, Result<ClientRpcResult<SessionHistoryPage>, String>> {
        let api = self.api.clone();
        async move {
            let payload = history_payload(&request).map_err(|error| render_js(&error))?;
            let result = if request.address.is_some() {
                call_path(&api, &["subagents", "history"], &[payload])
            } else {
                call_path(&api, &["sessions", "history"], &[payload])
            }
            .map_err(|error| render_js(&error))?;
            let response = JsFuture::from(Promise::resolve(&result))
                .await
                .map_err(|error| render_js(&error))?;
            parse_history_result(&response).map_err(|error| render_js(&error))
        }
        .boxed_local()
    }

    fn call(
        &self,
        request: SessionTransportRequest,
    ) -> LocalBoxFuture<'static, Result<ClientRpcResult<Value>, String>> {
        let api = self.api.clone();
        let remote = self.remote.clone();
        async move {
            let result = match request.method.as_str() {
                "respond" => call_method(
                    &api,
                    "respond",
                    &[json_to_js(&request.payload).map_err(|error| render_js(&error))?],
                ),
                "commands.execute" => {
                    let session_id = request.payload["sessionId"]
                        .as_str()
                        .ok_or_else(|| "commands.execute requires sessionId".to_owned())?;
                    let line = request.payload["line"]
                        .as_str()
                        .ok_or_else(|| "commands.execute requires line".to_owned())?;
                    call_path(
                        &remote,
                        &["commands", "execute"],
                        &[JsValue::from_str(session_id), JsValue::from_str(line)],
                    )
                }
                method => {
                    let Some((namespace, method)) = method.split_once('.') else {
                        return Err(format!("unknown Session transport method {method:?}"));
                    };
                    let namespace = match namespace {
                        "session" => "sessions",
                        "subagent" => "subagents",
                        other => other,
                    };
                    call_path(
                        &api,
                        &[namespace, method],
                        &[json_to_js(&request.payload).map_err(|error| render_js(&error))?],
                    )
                }
            }
            .map_err(|error| render_js(&error))?;
            let response = JsFuture::from(Promise::resolve(&result))
                .await
                .map_err(|error| render_js(&error))?;
            let envelope = Reflect::get(&response, &JsValue::from_str("result"))
                .ok()
                .filter(|value| !value.is_undefined())
                .unwrap_or(response);
            parse_rpc_result(&envelope).map_err(|error| render_js(&error))
        }
        .boxed_local()
    }
}

fn history_payload(request: &SessionHistoryRequest) -> Result<JsValue, JsValue> {
    let payload = Object::new();
    if let Some(address) = &request.address {
        set(
            &payload,
            "parentSessionId",
            &JsValue::from_str(address.parent_session_id.as_str()),
        )?;
        set(
            &payload,
            "childSessionId",
            &JsValue::from_str(address.child_session_id.as_str()),
        )?;
        set(
            &payload,
            "mode",
            &JsValue::from_str(match address.mode {
                SubagentMode::OneShot => "one-shot",
                SubagentMode::Continuable => "continuable",
            }),
        )?;
    } else {
        set(
            &payload,
            "sessionId",
            &JsValue::from_str(request.session_id.as_str()),
        )?;
    }
    if let Some(before) = request.before_seq {
        set(
            &payload,
            "beforeSeq",
            &JsValue::from_f64(js_safe_number(before)?),
        )?;
    }
    if let Some(maximum) = request.max_messages {
        set(
            &payload,
            "maxMessages",
            &JsValue::from_f64(js_safe_number(maximum)?),
        )?;
    }
    Ok(payload.into())
}

fn parse_history_result(value: &JsValue) -> Result<ClientRpcResult<SessionHistoryPage>, JsValue> {
    let envelope = Reflect::get(value, &JsValue::from_str("result"))
        .ok()
        .filter(|result| !result.is_undefined())
        .unwrap_or_else(|| value.clone());
    match parse_rpc_result_js(&envelope)? {
        ParsedRpcResult::Failure(error) => Ok(ClientRpcResult::Failure(error)),
        ParsedRpcResult::Success(None) => Ok(ClientRpcResult::Success(None)),
        ParsedRpcResult::Success(Some(value)) => {
            let events = required(&value, "events", "history result")?;
            if !Array::is_array(&events) {
                return Err(js_sys::Error::new("history result events must be an array").into());
            }
            let entries = Array::from(&events)
                .iter()
                .map(|entry| parse_history_entry(&entry))
                .collect::<Result<Vec<_>, _>>()?;
            let has_more = required(&value, "hasMore", "history result")?
                .as_bool()
                .ok_or_else(|| js_sys::Error::new("history result hasMore must be boolean"))?;
            let projections = Reflect::get(&value, &JsValue::from_str("projections"))?;
            let projections = if projections.is_undefined() {
                None
            } else {
                Some(parse_projection_baseline(&projections)?)
            };
            Ok(ClientRpcResult::Success(Some(SessionHistoryPage {
                entries,
                has_more,
                projections,
            })))
        }
    }
}

fn parse_history_entry(value: &JsValue) -> Result<SessionHistoryEntry, JsValue> {
    let event = required(value, "event", "history entry")?;
    let view = Reflect::get(value, &JsValue::from_str("view"))?;
    Ok(SessionHistoryEntry {
        event: parse_event(&event)?,
        view: if view.is_undefined() {
            None
        } else {
            Some(Rc::new(js_to_json(&view)?))
        },
    })
}

pub(crate) fn parse_event(
    value: &JsValue,
) -> Result<Rc<crate::ConversationLocationEvent>, JsValue> {
    let wire = js_to_json(value)?;
    let seq = safe_u64(
        &required(value, "seq", "Session event")?,
        "Session event seq",
    )?;
    let time = safe_i64(
        &required(value, "time", "Session event")?,
        "Session event time",
    )?;
    let event_type = required_string(value, "type", "Session event")?;
    let data = js_to_json(&required(value, "data", "Session event")?)?;
    Ok(crate::ConversationLocationEvent::with_wire(
        seq, time, event_type, data, wire,
    ))
}

fn parse_projection_baseline(value: &JsValue) -> Result<ProjectionsBaseline<Value>, JsValue> {
    let as_of_seq = safe_i64(
        &required(value, "asOfSeq", "projection baseline")?,
        "projection baseline asOfSeq",
    )?;
    let values = required(value, "values", "projection baseline")?;
    if !values.is_object() || values.is_null() {
        return Err(js_sys::Error::new("projection baseline values must be an object").into());
    }
    let values = Object::from(values);
    let mut projections = IndexMap::new();
    for key in Object::keys(&values).iter() {
        let key = key.as_string().unwrap_or_default();
        projections.insert(
            key.clone(),
            Rc::new(js_to_json(&Reflect::get(
                &values,
                &JsValue::from_str(&key),
            )?)?),
        );
    }
    Ok(ProjectionsBaseline {
        as_of_seq,
        values: projections,
    })
}

enum ParsedRpcResult {
    Success(Option<JsValue>),
    Failure(ClientRpcError),
}

fn parse_rpc_result_js(value: &JsValue) -> Result<ParsedRpcResult, JsValue> {
    let ok = required(value, "ok", "RPC result")?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new("RPC result ok must be boolean"))?;
    if ok {
        let carried = Reflect::has(value, &JsValue::from_str("value"))?;
        return Ok(ParsedRpcResult::Success(
            carried
                .then(|| Reflect::get(value, &JsValue::from_str("value")))
                .transpose()?,
        ));
    }
    let error = required(value, "error", "RPC result")?;
    Ok(ParsedRpcResult::Failure(parse_rpc_error(&error)?))
}

fn parse_rpc_result(value: &JsValue) -> Result<ClientRpcResult<Value>, JsValue> {
    match parse_rpc_result_js(value)? {
        ParsedRpcResult::Success(value) => Ok(ClientRpcResult::Success(
            value.as_ref().map(js_to_json).transpose()?,
        )),
        ParsedRpcResult::Failure(error) => Ok(ClientRpcResult::Failure(error)),
    }
}

pub(crate) fn parse_rpc_error(value: &JsValue) -> Result<ClientRpcError, JsValue> {
    let details = Reflect::get(value, &JsValue::from_str("details"))?;
    let details = if details.is_undefined() {
        JsonMap::new()
    } else {
        js_to_json(&details)?
            .as_object()
            .cloned()
            .ok_or_else(|| js_sys::Error::new("RPC error details must be an object"))?
    };
    Ok(ClientRpcError {
        code: required_string(value, "code", "RPC error")?,
        message: required_string(value, "message", "RPC error")?,
        details,
    })
}

type QueueSnapshot = Rc<Vec<crate::QueuedMessage>>;
type PendingSnapshot = Rc<Vec<Rc<PendingWait>>>;

/// Browser `Session` backed by the Rust lifecycle owner.
#[wasm_bindgen(js_name = Session)]
pub struct WasmClientSession {
    session: Rc<ClientSession>,
    snapshot_cache: RefCell<Option<(Rc<SessionSnapshot>, JsValue)>>,
    chat_cache: RefCell<Option<(Rc<Value>, JsValue)>>,
    queue_cache: RefCell<Option<(QueueSnapshot, Array)>>,
    pending_cache: RefCell<Option<(PendingSnapshot, Array)>>,
    pending_faces: RefCell<Vec<(Rc<PendingWait>, JsValue)>>,
    views_face: JsValue,
    projections_face: JsValue,
    empty_chat: JsValue,
    open_promise: Rc<RefCell<Option<Promise>>>,
    scope: RefCell<Option<JsValue>>,
}

#[wasm_bindgen(js_class = Session)]
impl WasmClientSession {
    /// Creates one browser Session over generated API and Remote faces.
    ///
    /// # Errors
    ///
    /// Returns malformed options or JavaScript face-construction failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        session_id: String,
        api: JsValue,
        remote: JsValue,
        options: JsValue,
    ) -> Result<Self, JsValue> {
        let scheduler = browser_notifier_scheduler();
        let address = optional(&options, "address")?
            .as_ref()
            .map(parse_subagent_address)
            .transpose()?;
        let parent_available = optional(&options, "parentAvailable")?
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let on_engaged = optional(&options, "onEngaged")?
            .map(wasm_bindgen::JsCast::dyn_into::<Function>)
            .transpose()?
            .map(|callback| {
                Rc::new(move |session_id: SessionId| {
                    if let Err(error) =
                        callback.call1(&JsValue::UNDEFINED, &JsValue::from_str(session_id.as_str()))
                    {
                        wasm_bindgen::throw_val(error);
                    }
                }) as Rc<dyn Fn(SessionId)>
            });
        let transport = Rc::new(BrowserSessionTransport::new(api, remote));
        let session = ClientSession::new(
            SessionId::new(session_id),
            transport,
            SessionOptions {
                address,
                parent_available,
                projections: None,
                conversation: None,
                scheduler,
                spawner: Rc::new(BrowserSessionSpawner),
                resolve_time_zone: Rc::new(|| {
                    resolved_client_time_zone_js().map_err(|error| render_js(&error))
                }),
                on_engaged,
                report: Rc::new(|message| console_error(&message)),
            },
        );
        let views_face = views_face(&session)?;
        let projections_face = projections_face(session.projections())?;
        Ok(Self {
            session,
            snapshot_cache: RefCell::new(None),
            chat_cache: RefCell::new(None),
            queue_cache: RefCell::new(None),
            pending_cache: RefCell::new(None),
            pending_faces: RefCell::new(Vec::new()),
            views_face,
            projections_face,
            empty_chat: empty_chat_snapshot()?,
            open_promise: Rc::new(RefCell::new(None)),
            scope: RefCell::new(None),
        })
    }

    /// Host Session identity.
    #[wasm_bindgen(getter, js_name = sessionId)]
    pub fn session_id(&self) -> String {
        self.session.session_id().as_str().to_owned()
    }

    /// Host-computed projection store face.
    #[wasm_bindgen(getter)]
    pub fn projections(&self) -> JsValue {
        self.projections_face.clone()
    }

    /// Cached observable snapshot.
    ///
    /// # Errors
    ///
    /// Returns JavaScript snapshot-construction failures.
    #[wasm_bindgen(js_name = getSnapshot)]
    pub fn get_snapshot(&self) -> Result<JsValue, JsValue> {
        let snapshot = self.session.snapshot();
        if let Some((current, value)) = &*self.snapshot_cache.borrow()
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = self.snapshot_to_js(&snapshot)?;
        *self.snapshot_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }

    /// Subscribes to snapshot changes.
    pub fn subscribe(&self, listener: Function) -> Function {
        let disposer = self.session.subscribe(Rc::new(move || {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                wasm_bindgen::throw_val(error);
            }
        }));
        Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
    }

    /// Opens the tail window, preserving one Promise for concurrent callers.
    pub fn open(&self) -> Promise {
        if self.session.snapshot().open_state == SessionOpenState::Open {
            return Promise::resolve(&JsValue::UNDEFINED);
        }
        if let Some(promise) = &*self.open_promise.borrow() {
            return promise.clone();
        }
        let open = self.session.open();
        let cache = self.open_promise.clone();
        let promise = future_to_promise(async move {
            open.await;
            cache.borrow_mut().take();
            Ok(JsValue::UNDEFINED)
        });
        *self.open_promise.borrow_mut() = Some(promise.clone());
        promise
    }

    /// Rebuilds the Session window after reconnect.
    pub fn resync(&self) -> Promise {
        let resync = self.session.resync();
        future_to_promise(async move {
            resync.await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Loads one older page.
    #[wasm_bindgen(js_name = loadOlder)]
    pub fn load_older(&self) -> Promise {
        let session = self.session.clone();
        future_to_promise(async move {
            session.load_older().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Sends an ordinary or addressed prompt. The engaging edge is synchronous.
    ///
    /// # Errors
    ///
    /// Returns malformed content conversion failures before transport begins.
    #[allow(clippy::needless_pass_by_value)]
    pub fn prompt(&self, content: Array, mode: String) -> Result<Promise, JsValue> {
        let content = content
            .iter()
            .map(|part| js_to_json(&part))
            .collect::<Result<Vec<_>, _>>()?;
        let prompt = self.session.prompt(content, mode);
        Ok(future_to_promise(
            async move { rpc_result_to_js(prompt.await) },
        ))
    }

    /// Cancels the active ordinary or continuable Turn.
    pub fn cancel(&self) -> Promise {
        let session = self.session.clone();
        future_to_promise(async move { rpc_result_to_js(session.cancel().await) })
    }

    /// Renames the Session.
    pub fn rename(&self, title: String) -> Promise {
        let session = self.session.clone();
        future_to_promise(async move { rpc_result_to_js(session.rename(&title).await) })
    }

    /// Executes one slash command.
    pub fn command(&self, line: String) -> Promise {
        let session = self.session.clone();
        future_to_promise(async move { rpc_result_to_js(session.command(&line).await) })
    }

    /// Applies one queue operation without local optimism.
    ///
    /// # Errors
    ///
    /// Returns malformed action conversion failures.
    #[wasm_bindgen(js_name = updateQueue)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn update_queue(&self, item_id: String, action: JsValue) -> Result<Promise, JsValue> {
        let action = js_to_json(&action)?;
        let session = self.session.clone();
        Ok(future_to_promise(async move {
            rpc_result_to_js(session.update_queue(&MessageId::new(item_id), action).await)
        }))
    }

    /// Reads and decodes one image attachment.
    #[wasm_bindgen(js_name = readAttachment)]
    pub fn read_attachment(&self, attachment_id: String) -> Promise {
        let session = self.session.clone();
        future_to_promise(async move {
            match session.read_attachment(&attachment_id).await {
                ClientRpcResult::Success(Some(read)) => {
                    let carried = Object::new();
                    set(&carried, "attachment", &json_to_js(&read.attachment)?)?;
                    set(
                        &carried,
                        "data",
                        &js_sys::Uint8Array::from(read.data.as_slice()),
                    )?;
                    let result = Object::new();
                    set(&result, "ok", &JsValue::TRUE)?;
                    set(&result, "value", &carried)?;
                    Ok(result.into())
                }
                ClientRpcResult::Success(None) => {
                    rpc_result_to_js(ClientRpcResult::<Value>::Success(None))
                }
                ClientRpcResult::Failure(error) => {
                    rpc_result_to_js(ClientRpcResult::<Value>::Failure(error))
                }
            }
        })
    }

    /// Routes one raw mux frame.
    ///
    /// # Errors
    ///
    /// Returns malformed known-frame diagnostics.
    #[wasm_bindgen(js_name = handleMuxEnvelope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn handle_mux_envelope(&self, rpc_id: String, frame: JsValue) -> Result<(), JsValue> {
        let frame = parse_mux_frame(&frame)?;
        self.session.handle_mux_envelope(RpcId::new(rpc_id), frame);
        Ok(())
    }

    /// Relays the Host running bit.
    #[wasm_bindgen(js_name = handleRunning)]
    pub fn handle_running(&self, running: bool) {
        self.session.handle_running(running);
    }

    /// Relays the authoritative summary blank bit.
    #[wasm_bindgen(js_name = handleBlank)]
    pub fn handle_blank(&self, blank: bool) {
        self.session.handle_blank(blank);
    }

    /// Flags Host removal.
    #[wasm_bindgen(js_name = handleRemoved)]
    pub fn handle_removed(&self) {
        self.session.handle_removed();
    }

    /// Relays an unpositioned Agent failure.
    #[wasm_bindgen(js_name = handleAgentError)]
    pub fn handle_agent_error(&self, message: String) {
        self.session.handle_agent_error(message);
    }

    /// Reserved resident-instance no-op.
    pub fn dispose(&self) {
        self.session.dispose();
    }

    /// Binds one Agent-scoped Client context.
    ///
    /// # Errors
    ///
    /// Returns the source diagnostic on a second bind.
    #[wasm_bindgen(js_name = bindScope)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn bind_scope(&self, scope: JsValue) -> Result<(), JsValue> {
        self.session
            .bind_scope()
            .map_err(|error| js_sys::Error::new(&error))?;
        *self.scope.borrow_mut() = Some(scope);
        Ok(())
    }

    /// Releases the bound Client context.
    #[wasm_bindgen(js_name = unbindScope)]
    pub fn unbind_scope(&self) {
        self.scope.borrow_mut().take();
        self.session.unbind_scope();
    }
}

fn views_face(session: &Rc<ClientSession>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let session = session.clone();
    let cache = Rc::new(RefCell::new(HashMap::<String, (Rc<Value>, JsValue)>::new()));
    let get = Closure::wrap(Box::new(move |target: String| -> Result<JsValue, JsValue> {
        let Some(snapshot) = session.conversation_snapshot(&target) else {
            return Ok(JsValue::UNDEFINED);
        };
        if let Some((current, value)) = cache.borrow().get(&target)
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let previous = cache.borrow().get(&target).cloned();
        let value = if target == "chat" {
            chat_snapshot_to_js_reusing(
                &snapshot,
                previous
                    .as_ref()
                    .map(|(previous, value)| (previous.as_ref(), value)),
            )?
        } else {
            json_to_js(&snapshot)?
        };
        cache.borrow_mut().insert(target, (snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&face, "get", &get.into_js_value())?;
    Ok(face.into())
}

fn projections_face(store: Rc<crate::ProjectionValueStore<Value>>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let faces = Rc::new(RefCell::new(HashMap::<String, JsValue>::new()));
    let face_store = store.clone();
    let face_cache = faces;
    let face_of = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        if let Some(face) = face_cache.borrow().get(&key) {
            return Ok(face.clone());
        }
        let value = projection_value_face(face_store.face_of(key.clone()))?;
        face_cache.borrow_mut().insert(key, value.clone());
        Ok(value)
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&face, "faceOf", &face_of.into_js_value())?;
    let get_store = store.clone();
    let get = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        get_store
            .get(&key)
            .map_or(Ok(JsValue::UNDEFINED), |value| json_to_js(&value))
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&face, "get", &get.into_js_value())?;
    let values_store = store;
    let values_cache = Rc::new(RefCell::new(
        None::<(Rc<IndexMap<String, Rc<Value>>>, JsValue)>,
    ));
    let values = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = values_store.values();
        if let Some((current, value)) = &*values_cache.borrow()
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let object = Object::new();
        for (key, value) in snapshot.iter() {
            set(&object, key, &json_to_js(value)?)?;
        }
        Object::freeze(&object);
        let value: JsValue = object.into();
        *values_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "values", &values.into_js_value())?;
    Ok(face.into())
}

fn projection_value_face(face: Rc<ProjectionFace<Value>>) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let snapshot_face = face.clone();
    let cache = Rc::new(RefCell::new(None::<(Rc<Value>, JsValue)>));
    let snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let Some(current) = snapshot_face.snapshot() else {
            return Ok(JsValue::UNDEFINED);
        };
        if let Some((known, value)) = &*cache.borrow()
            && Rc::ptr_eq(known, &current)
        {
            return Ok(value.clone());
        }
        let value = json_to_js(&current)?;
        *cache.borrow_mut() = Some((current, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&value, "getSnapshot", &snapshot.into_js_value())?;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        let disposer = face.subscribe(Rc::new(move || {
            if let Err(error) = listener.call0(&JsValue::UNDEFINED) {
                wasm_bindgen::throw_val(error);
            }
        }));
        Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into::<Function>()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&value, "subscribe", &subscribe.into_js_value())?;
    Ok(value.into())
}

pub(crate) fn empty_chat_snapshot() -> Result<JsValue, JsValue> {
    let empty = Array::new();
    let nodes = Object::new();
    set(&nodes, "get", &Function::new_no_args("return undefined"))?;
    let empty_for_values = empty.clone();
    let values =
        Closure::wrap(Box::new(move || empty_for_values.clone()) as Box<dyn FnMut() -> Array>);
    set(&nodes, "values", &values.into_js_value())?;
    let locations = Object::new();
    for method in ["getTurn", "getStep"] {
        let empty = empty.clone();
        let callback = Closure::wrap(Box::new(move || empty.clone()) as Box<dyn FnMut() -> Array>);
        set(&locations, method, &callback.into_js_value())?;
    }
    let timeline = Object::new();
    set(&timeline, "turnOrder", &empty)?;
    set(&timeline, "turns", &js_sys::Map::new())?;
    let legacy = Object::new();
    set(&legacy, "nodes", &empty)?;
    set(&legacy, "turnTimings", &js_sys::Map::new())?;
    set(&legacy, "turnEnds", &js_sys::Map::new())?;
    set(&legacy, "partial", &JsValue::NULL)?;
    set(&legacy, "runningCalls", &empty)?;
    let chat = Object::new();
    set(&chat, "order", &empty)?;
    set(&chat, "nodes", &nodes)?;
    set(&chat, "locations", &locations)?;
    set(&chat, "timeline", &timeline)?;
    set(&chat, "legacy", &legacy)?;
    Ok(chat.into())
}

#[allow(clippy::too_many_lines)] // One atomic source-shaped Chat snapshot face.
pub(crate) fn chat_snapshot_to_js_reusing(
    chat: &Value,
    previous: Option<(&Value, &JsValue)>,
) -> Result<JsValue, JsValue> {
    if chat.get("encoding").and_then(Value::as_str) != Some("seekdeep-chat-v1") {
        return json_to_js(chat);
    }
    let timeline_data = chat
        .get("timeline")
        .ok_or_else(|| js_sys::Error::new("encoded Chat snapshot omitted timeline"))?;
    let previous_timeline = previous.and_then(|(snapshot, rendered)| {
        (snapshot.get("timeline") == Some(timeline_data))
            .then(|| Reflect::get(rendered, &JsValue::from_str("timeline")).ok())
            .flatten()
    });
    let (timeline, turns, steps) = if let Some(previous) = previous_timeline {
        chat_timeline_faces_from_existing(timeline_data, previous)?
    } else {
        chat_timeline_to_js(timeline_data)?
    };
    let encoded_nodes = chat
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| js_sys::Error::new("encoded Chat snapshot nodes must be an array"))?;
    let previous_nodes = previous
        .and_then(|(_, snapshot)| Reflect::get(snapshot, &JsValue::from_str("nodes")).ok())
        .filter(|nodes| !nodes.is_undefined());
    let previous_encoded = previous
        .and_then(|(snapshot, _)| snapshot.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| Some((node.get("key")?.as_str()?.to_owned(), node)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let touched_keys = encoded_nodes
        .iter()
        .filter_map(|node| {
            let key = node.get("key")?.as_str()?;
            let previous = previous_encoded.get(key)?;
            (previous != &node && same_chat_node_structure(previous, node)).then(|| key.to_owned())
        })
        .collect::<HashSet<_>>();
    let node_values = Array::new();
    let nodes_by_key = JsMap::new();
    for encoded in encoded_nodes {
        let key = encoded
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| js_sys::Error::new("encoded Chat node omitted key"))?;
        let previous_node = previous_nodes
            .as_ref()
            .and_then(|nodes| Reflect::get(nodes, &JsValue::from_str("get")).ok())
            .and_then(|get| get.dyn_into::<Function>().ok())
            .and_then(|get| {
                get.call1(previous_nodes.as_ref().unwrap(), &JsValue::from_str(key))
                    .ok()
            })
            .filter(|node| !node.is_undefined());
        let node = if previous_encoded
            .get(key)
            .is_some_and(|previous| *previous == encoded)
        {
            previous_node.clone().unwrap_or(json_to_js(encoded)?)
        } else {
            let node = json_to_js(encoded)?;
            if let (Some(previous), Some(previous_node), Some(next_data)) = (
                previous_encoded.get(key),
                previous_node.as_ref(),
                encoded.get("data"),
            ) && let Some(previous_data) = previous.get("data")
            {
                let previous_js = Reflect::get(previous_node, &JsValue::from_str("data"))?;
                let data = json_to_js_reusing(previous_data, &previous_js, next_data)?;
                set(node.unchecked_ref::<Object>(), "data", &data)?;
            }
            let location = normalized_chat_location(encoded.get("location"), &turns, &steps)?;
            set(node.unchecked_ref::<Object>(), "location", &location)?;
            node
        };
        node_values.push(&node);
        nodes_by_key.set(&JsValue::from_str(key), &node);
    }
    let nodes = previous_nodes
        .as_ref()
        .and_then(|nodes| nodes.dyn_ref::<Object>())
        .cloned()
        .unwrap_or_else(Object::new);
    set(&nodes, "__entries", &nodes_by_key)?;
    set(&nodes, "__values", &node_values)?;
    if Reflect::get(&nodes, &JsValue::from_str("get"))?.is_undefined() {
        let owner = nodes.clone();
        let get = Closure::wrap(Box::new(move |key: String| {
            Reflect::get(&owner, &JsValue::from_str("__entries"))
                .map(|entries| {
                    entries
                        .unchecked_into::<JsMap>()
                        .get(&JsValue::from_str(&key))
                })
                .unwrap_or(JsValue::UNDEFINED)
        }) as Box<dyn FnMut(String) -> JsValue>);
        set(&nodes, "get", &get.into_js_value())?;
        let owner = nodes.clone();
        let values = Closure::wrap(Box::new(move || {
            Reflect::get(&owner, &JsValue::from_str("__values"))
                .map_or_else(|_| Array::new(), JsValue::unchecked_into::<Array>)
        }) as Box<dyn FnMut() -> Array>);
        set(&nodes, "values", &values.into_js_value())?;
    }

    let next_locations = chat
        .get("locations")
        .ok_or_else(|| js_sys::Error::new("encoded Chat snapshot omitted locations"))?;
    let previous_locations = previous.and_then(|(snapshot, rendered)| {
        Some((
            snapshot.get("locations")?,
            Reflect::get(rendered, &JsValue::from_str("locations")).ok()?,
        ))
    });
    let locations = chat_locations_to_js(next_locations, previous_locations, &touched_keys)?;
    let next_legacy = chat
        .get("legacy")
        .ok_or_else(|| js_sys::Error::new("encoded Chat snapshot omitted legacy"))?;
    let previous_legacy = previous.and_then(|(snapshot, rendered)| {
        Some((
            snapshot.get("legacy")?,
            Reflect::get(rendered, &JsValue::from_str("legacy")).ok()?,
        ))
    });
    let legacy = chat_legacy_to_js(next_legacy, previous_legacy)?;
    let value = Object::new();
    let empty_order = Value::Array(Vec::new());
    let next_order = chat.get("order").unwrap_or(&empty_order);
    let order = if let Some((previous, previous_js)) = previous
        && previous.get("order") == Some(next_order)
    {
        Reflect::get(previous_js, &JsValue::from_str("order"))?
    } else {
        json_to_js(next_order)?
    };
    set(&value, "order", &order)?;
    set(&value, "nodes", &nodes)?;
    set(&value, "locations", &locations)?;
    set(&value, "timeline", &timeline)?;
    set(&value, "legacy", &legacy)?;
    Ok(value.into())
}

fn json_to_js_reusing(
    previous: &Value,
    previous_js: &JsValue,
    next: &Value,
) -> Result<JsValue, JsValue> {
    if previous == next {
        return Ok(previous_js.clone());
    }
    match (previous, next) {
        (Value::Array(previous), Value::Array(next)) => {
            let value = Array::new();
            for (index, entry) in next.iter().enumerate() {
                let entry = if let Some(previous) = previous.get(index) {
                    json_to_js_reusing(
                        previous,
                        &Array::from(previous_js).get(u32::try_from(index).unwrap_or(u32::MAX)),
                        entry,
                    )?
                } else {
                    json_to_js(entry)?
                };
                value.push(&entry);
            }
            Ok(value.into())
        }
        (Value::Object(previous), Value::Object(next)) => {
            let value = Object::new();
            for (key, entry) in next {
                let entry = if let Some(previous) = previous.get(key) {
                    json_to_js_reusing(
                        previous,
                        &Reflect::get(previous_js, &JsValue::from_str(key))?,
                        entry,
                    )?
                } else {
                    json_to_js(entry)?
                };
                set(&value, key, &entry)?;
            }
            Ok(value.into())
        }
        _ => json_to_js(next),
    }
}

fn same_chat_node_structure(previous: &Value, next: &Value) -> bool {
    previous.get("anchorSeq") == next.get("anchorSeq")
        && previous.get("visibility") == next.get("visibility")
        && same_chat_location_identity(previous.get("location"), next.get("location"))
}

fn same_chat_location_identity(previous: Option<&Value>, next: Option<&Value>) -> bool {
    for key in ["kind", "turn", "step"] {
        if previous.and_then(|value| value.get(key)) != next.and_then(|value| value.get(key)) {
            return false;
        }
    }
    true
}

type ChatTimelineFaces = (JsValue, HashMap<u64, JsValue>, HashMap<String, JsValue>);

fn chat_timeline_faces_from_existing(
    encoded: &Value,
    timeline: JsValue,
) -> Result<ChatTimelineFaces, JsValue> {
    let turns_map = Reflect::get(&timeline, &JsValue::from_str("turns"))?.dyn_into::<JsMap>()?;
    let mut turns = HashMap::new();
    let mut steps = HashMap::new();
    for row in encoded
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let turn_number = row
            .get("turn")
            .and_then(Value::as_u64)
            .ok_or_else(|| js_sys::Error::new("encoded Chat Turn omitted turn"))?;
        let turn = turns_map.get(&JsValue::from_f64(js_safe_number(turn_number)?));
        if turn.is_undefined() {
            return Err(js_sys::Error::new("cached Chat timeline omitted Turn").into());
        }
        for step in Array::from(&Reflect::get(&turn, &JsValue::from_str("steps"))?).iter() {
            let step_number = Reflect::get(&step, &JsValue::from_str("step"))?
                .as_f64()
                .ok_or_else(|| js_sys::Error::new("cached Chat Step omitted step"))?;
            steps.insert(format!("{turn_number}:{step_number}"), step);
        }
        turns.insert(turn_number, turn);
    }
    Ok((timeline, turns, steps))
}

fn chat_timeline_to_js(value: &Value) -> Result<ChatTimelineFaces, JsValue> {
    let rows = value
        .get("turns")
        .and_then(Value::as_array)
        .ok_or_else(|| js_sys::Error::new("encoded Chat timeline turns must be an array"))?;
    let mut turns = HashMap::new();
    let mut steps = HashMap::new();
    let turns_map = JsMap::new();
    for row in rows {
        let turn_number = row
            .get("turn")
            .and_then(Value::as_u64)
            .ok_or_else(|| js_sys::Error::new("encoded Chat Turn omitted turn"))?;
        let turn = Object::new();
        set(
            &turn,
            "turn",
            &JsValue::from_f64(js_safe_number(turn_number)?),
        )?;
        set_optional_json(&turn, "start", row.get("start"))?;
        set_optional_json(&turn, "end", row.get("end"))?;
        set(
            &turn,
            "status",
            &JsValue::from_str(
                row.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            ),
        )?;
        set(&turn, "data", &chat_data_store_to_js(row.get("data"))?)?;
        let step_values = Array::new();
        for step_row in row
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let step_number = step_row
                .get("step")
                .and_then(Value::as_u64)
                .ok_or_else(|| js_sys::Error::new("encoded Chat Step omitted step"))?;
            let step = Object::new();
            set(
                &step,
                "turn",
                &JsValue::from_f64(js_safe_number(turn_number)?),
            )?;
            set(
                &step,
                "step",
                &JsValue::from_f64(js_safe_number(step_number)?),
            )?;
            set_optional_json(&step, "start", step_row.get("start"))?;
            set_optional_json(&step, "end", step_row.get("end"))?;
            set(
                &step,
                "status",
                &JsValue::from_str(
                    step_row
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                ),
            )?;
            set(&step, "data", &chat_data_store_to_js(step_row.get("data"))?)?;
            let step_value: JsValue = step.into();
            steps.insert(format!("{turn_number}:{step_number}"), step_value.clone());
            step_values.push(&step_value);
        }
        set(&turn, "steps", &step_values)?;
        let turn_value: JsValue = turn.into();
        turns.insert(turn_number, turn_value.clone());
        turns_map.set(
            &JsValue::from_f64(js_safe_number(turn_number)?),
            &turn_value,
        );
    }
    let timeline = Object::new();
    set(
        &timeline,
        "turnOrder",
        &json_to_js(value.get("turnOrder").unwrap_or(&Value::Array(Vec::new())))?,
    )?;
    set(&timeline, "turns", &turns_map)?;
    Ok((timeline.into(), turns, steps))
}

fn chat_data_store_to_js(value: Option<&Value>) -> Result<JsValue, JsValue> {
    let data = value.cloned().unwrap_or_else(|| json!({}));
    let store = Object::new();
    let get = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
        data.get(&key)
            .filter(|value| !value.is_null())
            .map(json_to_js)
            .transpose()
            .map(|value| value.unwrap_or(JsValue::UNDEFINED))
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    set(&store, "get", &get.into_js_value())?;
    Ok(store.into())
}

fn normalized_chat_location(
    encoded: Option<&Value>,
    turns: &HashMap<u64, JsValue>,
    steps: &HashMap<String, JsValue>,
) -> Result<JsValue, JsValue> {
    let encoded = encoded.unwrap_or(&Value::Null);
    let kind = encoded
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unresolved");
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str(kind))?;
    if matches!(kind, "turn" | "step") {
        let turn_number = encoded
            .get("turn")
            .and_then(Value::as_u64)
            .ok_or_else(|| js_sys::Error::new("encoded Chat Location omitted turn"))?;
        let turn = turns.get(&turn_number).ok_or_else(|| {
            js_sys::Error::new(&format!(
                "encoded Chat Location references unknown Turn {turn_number}"
            ))
        })?;
        set(&value, "turn", turn)?;
        if kind == "step" {
            let step_number = encoded
                .get("step")
                .and_then(Value::as_u64)
                .ok_or_else(|| js_sys::Error::new("encoded Chat Location omitted step"))?;
            let step = steps
                .get(&format!("{turn_number}:{step_number}"))
                .ok_or_else(|| {
                    js_sys::Error::new(&format!(
                        "encoded Chat Location references unknown Step {turn_number}:{step_number}"
                    ))
                })?;
            set(&value, "step", step)?;
        }
    }
    Ok(value.into())
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn chat_locations_to_js(
    value: &Value,
    previous: Option<(&Value, JsValue)>,
    touched_keys: &HashSet<String>,
) -> Result<JsValue, JsValue> {
    let turns = JsMap::new();
    for row in value
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let pair = row
            .as_array()
            .ok_or_else(|| js_sys::Error::new("encoded Chat Turn index row must be an array"))?;
        let turn = pair
            .first()
            .and_then(Value::as_u64)
            .ok_or_else(|| js_sys::Error::new("encoded Chat Turn index omitted turn"))?;
        let next_keys = pair
            .get(1)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let keys = previous
            .as_ref()
            .and_then(|(previous, face)| {
                let row = previous
                    .get("turns")?
                    .as_array()?
                    .iter()
                    .find(|row| row.get(0).and_then(Value::as_u64) == Some(turn))?;
                (row.get(1) == Some(&next_keys) && !contains_touched_key(&next_keys, touched_keys))
                    .then(|| {
                        Reflect::get(face, &JsValue::from_str("getTurn"))
                            .ok()?
                            .dyn_into::<Function>()
                            .ok()?
                            .call1(face, &JsValue::from_f64(js_safe_number(turn).ok()?))
                            .ok()
                    })?
            })
            .unwrap_or(json_to_js(&next_keys)?);
        turns.set(&JsValue::from_f64(js_safe_number(turn)?), &keys);
    }
    let steps = JsMap::new();
    for row in value
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let fields = row
            .as_array()
            .ok_or_else(|| js_sys::Error::new("encoded Chat Step index row must be an array"))?;
        let turn = fields
            .first()
            .and_then(Value::as_u64)
            .ok_or_else(|| js_sys::Error::new("encoded Chat Step index omitted turn"))?;
        let step = fields
            .get(1)
            .and_then(Value::as_u64)
            .ok_or_else(|| js_sys::Error::new("encoded Chat Step index omitted step"))?;
        let next_keys = fields
            .get(2)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let keys = previous
            .as_ref()
            .and_then(|(previous, face)| {
                let row = previous.get("steps")?.as_array()?.iter().find(|row| {
                    row.get(0).and_then(Value::as_u64) == Some(turn)
                        && row.get(1).and_then(Value::as_u64) == Some(step)
                })?;
                (row.get(2) == Some(&next_keys) && !contains_touched_key(&next_keys, touched_keys))
                    .then(|| {
                        Reflect::get(face, &JsValue::from_str("getStep"))
                            .ok()?
                            .dyn_into::<Function>()
                            .ok()?
                            .call2(
                                face,
                                &JsValue::from_f64(js_safe_number(turn).ok()?),
                                &JsValue::from_f64(js_safe_number(step).ok()?),
                            )
                            .ok()
                    })?
            })
            .unwrap_or(json_to_js(&next_keys)?);
        steps.set(&JsValue::from_str(&format!("{turn}:{step}")), &keys);
    }
    let locations = previous
        .as_ref()
        .and_then(|(_, face)| face.dyn_ref::<Object>())
        .cloned()
        .unwrap_or_else(Object::new);
    set(&locations, "__turns", &turns)?;
    set(&locations, "__steps", &steps)?;
    if Reflect::get(&locations, &JsValue::from_str("getTurn"))?.is_undefined() {
        let owner = locations.clone();
        let get_turn = Closure::wrap(Box::new(move |turn: f64| {
            let index = Reflect::get(&owner, &JsValue::from_str("__turns"))
                .map(JsValue::unchecked_into::<JsMap>);
            let value = index.map_or(JsValue::UNDEFINED, |index| {
                index.get(&JsValue::from_f64(turn))
            });
            if value.is_undefined() {
                Array::new()
            } else {
                value.unchecked_into::<Array>()
            }
        }) as Box<dyn FnMut(f64) -> Array>);
        set(&locations, "getTurn", &get_turn.into_js_value())?;
        let owner = locations.clone();
        let get_step = Closure::wrap(Box::new(move |turn: f64, step: f64| {
            let index = Reflect::get(&owner, &JsValue::from_str("__steps"))
                .map(JsValue::unchecked_into::<JsMap>);
            let value = index.map_or(JsValue::UNDEFINED, |index| {
                index.get(&JsValue::from_str(&format!("{turn}:{step}")))
            });
            if value.is_undefined() {
                Array::new()
            } else {
                value.unchecked_into::<Array>()
            }
        }) as Box<dyn FnMut(f64, f64) -> Array>);
        set(&locations, "getStep", &get_step.into_js_value())?;
    }
    Ok(locations.into())
}

fn contains_touched_key(keys: &Value, touched: &HashSet<String>) -> bool {
    keys.as_array().is_some_and(|keys| {
        keys.iter()
            .filter_map(Value::as_str)
            .any(|key| touched.contains(key))
    })
}

#[allow(clippy::needless_pass_by_value)] // Owns the optional cached JS handle for this render.
fn chat_legacy_to_js(
    value: &Value,
    previous: Option<(&Value, JsValue)>,
) -> Result<JsValue, JsValue> {
    let legacy = Object::new();
    let empty = Value::Array(Vec::new());
    let nodes = reused_legacy_member(value, previous.as_ref(), "nodes", &empty)?;
    let partial = reused_legacy_member(value, previous.as_ref(), "partial", &Value::Null)?;
    let running = reused_legacy_member(value, previous.as_ref(), "runningCalls", &empty)?;
    let turn_timings: JsValue = reused_legacy_map(value, previous.as_ref(), "turnTimings")?;
    let turn_ends: JsValue = reused_legacy_map(value, previous.as_ref(), "turnEnds")?;
    set(&legacy, "nodes", &nodes)?;
    set(&legacy, "turnTimings", &turn_timings)?;
    set(&legacy, "turnEnds", &turn_ends)?;
    set(&legacy, "partial", &partial)?;
    set(&legacy, "runningCalls", &running)?;
    Ok(legacy.into())
}

fn reused_legacy_member(
    value: &Value,
    previous: Option<&(&Value, JsValue)>,
    key: &str,
    fallback: &Value,
) -> Result<JsValue, JsValue> {
    let next = value.get(key).unwrap_or(fallback);
    if let Some((previous, rendered)) = previous
        && let Some(previous) = previous.get(key)
    {
        return json_to_js_reusing(
            previous,
            &Reflect::get(rendered, &JsValue::from_str(key))?,
            next,
        );
    }
    json_to_js(next)
}

fn reused_legacy_map(
    value: &Value,
    previous: Option<&(&Value, JsValue)>,
    key: &str,
) -> Result<JsValue, JsValue> {
    if let Some((previous, rendered)) = previous
        && previous.get(key) == value.get(key)
    {
        return Reflect::get(rendered, &JsValue::from_str(key));
    }
    pairs_to_map(value.get(key)).map(Into::into)
}

fn pairs_to_map(value: Option<&Value>) -> Result<JsMap, JsValue> {
    let map = JsMap::new();
    for row in value.and_then(Value::as_array).into_iter().flatten() {
        let pair = row
            .as_array()
            .ok_or_else(|| js_sys::Error::new("encoded Chat map row must be an array"))?;
        if pair.len() >= 2 {
            map.set(&json_to_js(&pair[0])?, &json_to_js(&pair[1])?);
        }
    }
    Ok(map)
}

fn set_optional_json(object: &Object, key: &str, value: Option<&Value>) -> Result<(), JsValue> {
    set(
        object,
        key,
        &value
            .filter(|value| !value.is_null())
            .map(json_to_js)
            .transpose()?
            .unwrap_or(JsValue::UNDEFINED),
    )
}

pub(crate) fn parse_mux_frame(frame: &JsValue) -> Result<SessionMuxFrame, JsValue> {
    let frame_type = required_string(frame, "type", "mux frame")?;
    match frame_type.as_str() {
        "session/event" => {
            let event = parse_event(&required(frame, "event", "session/event")?)?;
            let view = Reflect::get(frame, &JsValue::from_str("view"))?;
            Ok(SessionMuxFrame::Event(SessionHistoryEntry {
                event,
                view: if view.is_undefined() {
                    None
                } else {
                    Some(Rc::new(js_to_json(&view)?))
                },
            }))
        }
        "session/queue" => {
            let items = required(frame, "items", "session/queue")?;
            if !Array::is_array(&items) {
                return Err(js_sys::Error::new("session/queue items must be an array").into());
            }
            let items = Array::from(&items)
                .iter()
                .map(|item| {
                    let message = required(&item, "message", "session queue item")?;
                    let placement = required_string(&item, "placement", "session queue item")?;
                    let content = js_to_json(&required(&message, "content", "queued message")?)?
                        .as_array()
                        .cloned()
                        .ok_or_else(|| {
                            js_sys::Error::new("queued message content must be an array")
                        })?;
                    Ok(QueueItemInput {
                        id: MessageId::new(required_string(&item, "id", "session queue item")?),
                        message_id: MessageId::new(required_string(
                            &message,
                            "id",
                            "queued message",
                        )?),
                        placement: match placement.as_str() {
                            "queued" => QueuePlacement::Queued,
                            "steering" => QueuePlacement::Steering,
                            _ => {
                                return Err(js_sys::Error::new(
                                    "session queue placement must be queued or steering",
                                )
                                .into());
                            }
                        },
                        content,
                    })
                })
                .collect::<Result<Vec<_>, JsValue>>()?;
            Ok(SessionMuxFrame::Queue(items))
        }
        "session/subscribed" => Ok(SessionMuxFrame::Subscribed {
            last_seq: safe_u64(
                &required(frame, "lastSeq", "session/subscribed")?,
                "session/subscribed lastSeq",
            )?,
        }),
        "approval/requested" => Ok(SessionMuxFrame::ApprovalRequested {
            payload: stripped_payload(frame, &["type", "sessionId"])?,
        }),
        "approval/resolved" => Ok(SessionMuxFrame::ApprovalResolved {
            approval_id: required_string(frame, "approvalId", "approval/resolved")?,
        }),
        "question/requested" => Ok(SessionMuxFrame::QuestionRequested {
            payload: stripped_payload(frame, &["type", "sessionId"])?,
        }),
        "question/resolved" => Ok(SessionMuxFrame::QuestionResolved {
            question_rpc_id: RpcId::new(required_string(
                frame,
                "questionRpcId",
                "question/resolved",
            )?),
        }),
        _ => Ok(SessionMuxFrame::Unknown),
    }
}

pub(crate) fn parse_subagent_address(value: &JsValue) -> Result<SubagentAddress, JsValue> {
    Ok(SubagentAddress {
        parent_session_id: SessionId::new(required_string(
            value,
            "parentSessionId",
            "subagent address",
        )?),
        child_session_id: SessionId::new(required_string(
            value,
            "childSessionId",
            "subagent address",
        )?),
        mode: match required_string(value, "mode", "subagent address")?.as_str() {
            "one-shot" => SubagentMode::OneShot,
            "continuable" => SubagentMode::Continuable,
            _ => {
                return Err(js_sys::Error::new(
                    "subagent address mode must be one-shot or continuable",
                )
                .into());
            }
        },
    })
}

fn subagent_state_to_js(state: &crate::SessionSubagentState) -> Result<JsValue, JsValue> {
    let value = Object::new();
    let address = Object::new();
    set(
        &address,
        "parentSessionId",
        &JsValue::from_str(state.address.parent_session_id.as_str()),
    )?;
    set(
        &address,
        "childSessionId",
        &JsValue::from_str(state.address.child_session_id.as_str()),
    )?;
    set(
        &address,
        "mode",
        &JsValue::from_str(match state.address.mode {
            SubagentMode::OneShot => "one-shot",
            SubagentMode::Continuable => "continuable",
        }),
    )?;
    set(&value, "address", &address)?;
    set(
        &value,
        "parentAvailable",
        &JsValue::from_bool(state.parent_available),
    )?;
    Ok(value.into())
}

fn prompt_error_to_js(error: &SessionPromptError) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "op",
        &JsValue::from_str(match error.operation {
            PromptOperation::Send => "send",
            PromptOperation::Stop => "stop",
        }),
    )?;
    set(&value, "error", &rpc_error_to_js(&error.error)?)?;
    Ok(value.into())
}

pub(crate) fn rpc_error_to_js(error: &ClientRpcError) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(&value, "code", &JsValue::from_str(&error.code))?;
    set(&value, "message", &JsValue::from_str(&error.message))?;
    set(
        &value,
        "details",
        &json_to_js(&Value::Object(error.details.clone()))?,
    )?;
    Ok(value.into())
}

pub(crate) fn rpc_result_to_js(result: ClientRpcResult<Value>) -> Result<JsValue, JsValue> {
    let value = Object::new();
    match result {
        ClientRpcResult::Success(carried) => {
            set(&value, "ok", &JsValue::TRUE)?;
            if let Some(carried) = carried {
                set(&value, "value", &json_to_js(&carried)?)?;
            }
        }
        ClientRpcResult::Failure(error) => {
            set(&value, "ok", &JsValue::FALSE)?;
            set(&value, "error", &rpc_error_to_js(&error)?)?;
        }
    }
    Ok(value.into())
}

fn stripped_payload(value: &JsValue, removed: &[&str]) -> Result<Value, JsValue> {
    let mut payload = js_to_json(value)?;
    let object = payload
        .as_object_mut()
        .ok_or_else(|| js_sys::Error::new("mux frame must be an object"))?;
    for key in removed {
        object.remove(*key);
    }
    Ok(payload)
}

pub(crate) fn call_path(
    root: &JsValue,
    path: &[&str],
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let (method, owners) = path
        .split_last()
        .ok_or_else(|| js_sys::Error::new("empty JavaScript method path"))?;
    let mut owner = root.clone();
    for key in owners {
        owner = required(&owner, key, "generated Client")?;
    }
    call_method(&owner, method, arguments)
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required(value, method, "generated Client namespace")?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args)
}

pub(crate) fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("{owner} requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

pub(crate) fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!member.is_undefined()).then_some(member))
}

fn optional_member(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    optional(value, key)
}

pub(crate) fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key} must be a string")).into())
}

fn js_safe_number(value: u64) -> Result<f64, JsValue> {
    if value > MAX_SAFE_INTEGER_U64 {
        return Err(
            js_sys::Error::new("JavaScript sequence exceeds the safe integer range").into(),
        );
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(value as f64)
}

pub(crate) fn safe_u64(value: &JsValue, owner: &str) -> Result<u64, JsValue> {
    let number = value
        .as_f64()
        .filter(|number| {
            number.is_finite() && number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(number)
        })
        .ok_or_else(|| {
            js_sys::Error::new(&format!("{owner} must be a non-negative safe integer"))
        })?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as u64)
}

pub(crate) fn safe_i64(value: &JsValue, owner: &str) -> Result<i64, JsValue> {
    let number = value
        .as_f64()
        .filter(|number| {
            number.is_finite()
                && number.fract() == 0.0
                && (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(number)
        })
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} must be a safe integer")))?;
    #[allow(clippy::cast_possible_truncation)]
    Ok(number as i64)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Session member {key:?}")).into())
    }
}

pub(crate) fn js_to_json(value: &JsValue) -> Result<Value, JsValue> {
    serde_wasm_bindgen::from_value(value.clone())
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

pub(crate) fn json_to_js(value: &Value) -> Result<JsValue, JsValue> {
    // JSON parsing preserves browser numbers instead of serde's arbitrary-precision wrapper objects.
    let text =
        serde_json::to_string(value).map_err(|error| js_sys::Error::new(&error.to_string()))?;
    js_sys::JSON::parse(&text)
}

pub(crate) fn render_js(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            Reflect::get(value, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

pub(crate) fn console_error(message: &str) {
    let global = js_sys::global();
    let Some((console, error)) = Reflect::get(&global, &JsValue::from_str("console"))
        .ok()
        .and_then(|console| {
            Reflect::get(&console, &JsValue::from_str("error"))
                .ok()
                .and_then(|error| error.dyn_into::<Function>().ok())
                .map(|error| (console, error))
        })
    else {
        return;
    };
    let _ = error.call1(&console, &JsValue::from_str(message));
}

impl WasmClientSession {
    pub(crate) fn from_session(session: Rc<ClientSession>) -> Result<Self, JsValue> {
        let views_face = views_face(&session)?;
        let projections_face = projections_face(session.projections())?;
        Ok(Self {
            session,
            snapshot_cache: RefCell::new(None),
            chat_cache: RefCell::new(None),
            queue_cache: RefCell::new(None),
            pending_cache: RefCell::new(None),
            pending_faces: RefCell::new(Vec::new()),
            views_face,
            projections_face,
            empty_chat: empty_chat_snapshot()?,
            open_promise: Rc::new(RefCell::new(None)),
            scope: RefCell::new(None),
        })
    }

    #[allow(clippy::too_many_lines)] // Mirrors the source's one atomic public snapshot shape.
    fn snapshot_to_js(&self, snapshot: &Rc<SessionSnapshot>) -> Result<JsValue, JsValue> {
        let value = Object::new();
        set(
            &value,
            "sessionId",
            &JsValue::from_str(snapshot.session_id.as_str()),
        )?;
        set(&value, "views", &self.views_face)?;
        let chat = snapshot
            .chat
            .as_ref()
            .map(|chat| self.chat_value(chat))
            .transpose()?
            .unwrap_or_else(|| self.empty_chat.clone());
        set(&value, "chat", &chat)?;
        let legacy = Reflect::get(&chat, &JsValue::from_str("legacy"))?;
        let empty = Array::new();
        set(
            &value,
            "nodes",
            &optional_member(&legacy, "nodes")?.unwrap_or_else(|| empty.clone().into()),
        )?;
        set(
            &value,
            "turnTimings",
            &optional_member(&legacy, "turnTimings")?.unwrap_or_else(|| js_sys::Map::new().into()),
        )?;
        set(
            &value,
            "turnEnds",
            &optional_member(&legacy, "turnEnds")?.unwrap_or_else(|| js_sys::Map::new().into()),
        )?;
        set(
            &value,
            "partial",
            &optional_member(&legacy, "partial")?.unwrap_or(JsValue::NULL),
        )?;
        set(
            &value,
            "runningCalls",
            &optional_member(&legacy, "runningCalls")?.unwrap_or_else(|| empty.clone().into()),
        )?;
        let pending: JsValue = self.pending_value(&snapshot.pending).into();
        set(&value, "pending", &pending)?;
        let queue: JsValue = self.queue_value(&snapshot.queue)?.into();
        set(&value, "queue", &queue)?;
        set(&value, "running", &JsValue::from_bool(snapshot.running))?;
        set(
            &value,
            "subagent",
            &snapshot
                .subagent
                .as_ref()
                .map(subagent_state_to_js)
                .transpose()?
                .unwrap_or(JsValue::NULL),
        )?;
        set(
            &value,
            "composerPhase",
            &JsValue::from_str(match snapshot.composer_phase {
                ComposerPhase::Blank => "blank",
                ComposerPhase::Engaging => "engaging",
                ComposerPhase::Active => "active",
            }),
        )?;
        set(&value, "removed", &JsValue::from_bool(snapshot.removed))?;
        set(
            &value,
            "openState",
            &JsValue::from_str(match snapshot.open_state {
                SessionOpenState::Cold => "cold",
                SessionOpenState::Loading => "loading",
                SessionOpenState::Open => "open",
                SessionOpenState::Error => "error",
            }),
        )?;
        set(
            &value,
            "openError",
            &snapshot
                .open_error
                .as_ref()
                .map(rpc_error_to_js)
                .transpose()?
                .unwrap_or(JsValue::NULL),
        )?;
        set(&value, "hasMore", &JsValue::from_bool(snapshot.has_more))?;
        set(
            &value,
            "loadingOlder",
            &JsValue::from_bool(snapshot.loading_older),
        )?;
        set(
            &value,
            "promptError",
            &snapshot
                .prompt_error
                .as_ref()
                .map(prompt_error_to_js)
                .transpose()?
                .unwrap_or(JsValue::NULL),
        )?;
        set(&value, "blank", &JsValue::from_bool(snapshot.blank))?;
        set(
            &value,
            "lastAgentError",
            &snapshot
                .last_agent_error
                .as_ref()
                .map_or(JsValue::NULL, |message| JsValue::from_str(message)),
        )?;
        Ok(value.into())
    }

    fn chat_value(&self, chat: &Rc<Value>) -> Result<JsValue, JsValue> {
        let cache = self.chat_cache.borrow();
        if let Some((current, value)) = &*cache
            && Rc::ptr_eq(current, chat)
        {
            return Ok(value.clone());
        }
        let value = chat_snapshot_to_js_reusing(
            chat,
            cache
                .as_ref()
                .map(|(previous, value)| (previous.as_ref(), value)),
        )?;
        drop(cache);
        *self.chat_cache.borrow_mut() = Some((chat.clone(), value.clone()));
        Ok(value)
    }

    fn queue_value(&self, queue: &QueueSnapshot) -> Result<Array, JsValue> {
        if let Some((current, value)) = &*self.queue_cache.borrow()
            && Rc::ptr_eq(current, queue)
        {
            return Ok(value.clone());
        }
        let value = Array::new();
        for item in queue.iter() {
            let row = Object::new();
            set(&row, "id", &JsValue::from_str(item.id.as_str()))?;
            set(
                &row,
                "messageId",
                &JsValue::from_str(item.message_id.as_str()),
            )?;
            set(
                &row,
                "placement",
                &JsValue::from_str(match item.placement {
                    QueuePlacement::Queued => "queued",
                    QueuePlacement::Steering => "steering",
                }),
            )?;
            set(
                &row,
                "content",
                &json_to_js(&Value::Array(item.content.clone()))?,
            )?;
            set(&row, "preview", &JsValue::from_str(&item.preview))?;
            set(
                &row,
                "text",
                &item
                    .text
                    .as_ref()
                    .map_or(JsValue::NULL, |text| JsValue::from_str(text)),
            )?;
            value.push(&row);
        }
        *self.queue_cache.borrow_mut() = Some((queue.clone(), value.clone()));
        Ok(value)
    }

    fn pending_value(&self, pending: &PendingSnapshot) -> Array {
        if let Some((current, value)) = &*self.pending_cache.borrow()
            && Rc::ptr_eq(current, pending)
        {
            return value.clone();
        }
        let value = Array::new();
        for wait in pending.iter() {
            value.push(&self.pending_face(wait));
        }
        *self.pending_cache.borrow_mut() = Some((pending.clone(), value.clone()));
        value
    }

    fn pending_face(&self, wait: &Rc<PendingWait>) -> JsValue {
        if let Some((_, value)) = self
            .pending_faces
            .borrow()
            .iter()
            .find(|(candidate, _)| Rc::ptr_eq(candidate, wait))
        {
            return value.clone();
        }
        let value: JsValue = crate::WasmPendingWait::from_native(wait.clone()).into();
        self.pending_faces
            .borrow_mut()
            .push((wait.clone(), value.clone()));
        value
    }
}
