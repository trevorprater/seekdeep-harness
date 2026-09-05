//! JavaScript faces over Rust-owned same-origin unary transport and services.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use parking_lot::Mutex;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};
use web_sys::{Event, MessageEvent, Request, RequestInit, Response, WebSocket};

thread_local! {
    static API_PROXY_FACTORY: Function = Function::new_with_args(
        "client",
        r"
const namespaces = new Map();
const prefixes = { sessions: 'session', subagents: 'subagent', skills: 'skill', agentPresets: 'agentPreset' };
return new Proxy({}, {
  get(target, namespace, receiver) {
    if (Reflect.has(target, namespace)) return Reflect.get(target, namespace, receiver);
    if (namespace === 'toJSON') return undefined;
    if (namespace === 'respond') return (message, signal) => client.respond(message, signal);
    if (namespace === 'subscribeEnvelopes') return listener => client.subscribeEnvelopes(listener);
    if (typeof namespace !== 'string') return undefined;
    if (!namespaces.has(namespace)) namespaces.set(namespace, new Proxy({}, {
      get(_namespace, method) {
        if (typeof method !== 'string') return undefined;
        if (method === 'toJSON') return undefined;
        if (namespace === 'events' && (method === 'mux' || method === 'host')) {
          return (_payload, _signal, onOpen) => ({
            async *[Symbol.asyncIterator]() { onOpen?.(); },
          });
        }
        return (payload = {}, signal) => client.call((prefixes[namespace] ?? namespace) + '.' + method, payload, signal);
      },
    }));
    return namespaces.get(namespace);
  },
});
",
    );
    static CLIENT_REMOTE_FACTORIES: RefCell<Option<(Function, Function)>> = const { RefCell::new(None) };
}

type Listeners = Rc<Mutex<Vec<(u64, Function)>>>;

#[derive(Clone)]
struct ConnectionCallbacks {
    sinks: JsValue,
    listeners: Listeners,
    description: Rc<Mutex<JsValue>>,
    description_listeners: Listeners,
    stopped: Rc<Cell<bool>>,
    lost: Rc<Cell<bool>>,
}

struct BrowserSocket {
    socket: WebSocket,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(Event)>,
    _on_error: Closure<dyn FnMut(Event)>,
}

impl BrowserSocket {
    fn stop(&self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        let _ = self.socket.close();
    }
}

/// Same-origin unary API client used by the browser service face.
#[wasm_bindgen]
pub struct WasmBrowserApiClient {
    next_listener: Cell<u64>,
    listeners: Listeners,
}

#[wasm_bindgen]
impl WasmBrowserApiClient {
    /// Creates an unstarted browser client.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            next_listener: Cell::new(0),
            listeners: Rc::new(Mutex::new(Vec::new())),
        }
    }

    /// Performs one Host API unary request.
    #[wasm_bindgen(js_name = call)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn call(&self, method: String, payload: JsValue, signal: JsValue) -> Promise {
        let listeners = self.listeners.clone();
        future_to_promise(async move {
            let rpc_id = random_uuid()?;
            let request = object(&[
                ("type", JsValue::from_str("client-request")),
                ("rpcId", JsValue::from_str(&rpc_id)),
                ("method", JsValue::from_str(&method)),
                ("payload", payload),
            ])?;
            notify(&listeners, &request.clone().into());
            let response = post_json(&format!("/api/{method}"), request.into(), signal).await?;
            notify(&listeners, &response);
            let echoed = Reflect::get(&response, &JsValue::from_str("rpcId"))?
                .as_string()
                .ok_or_else(|| js_sys::Error::new("connection response omitted rpcId"))?;
            if echoed != rpc_id {
                return Err(js_sys::Error::new(&format!(
                    "rpcId mismatch for {method}: sent {rpc_id}, got {echoed}"
                ))
                .into());
            }
            Ok(response)
        })
    }

    /// Sends one Client response envelope.
    #[allow(clippy::needless_pass_by_value)]
    pub fn respond(&self, message: JsValue, signal: JsValue) -> Promise {
        let listeners = self.listeners.clone();
        future_to_promise(async move {
            notify(&listeners, &message);
            let response = post_json("/api/respond", message, signal).await?;
            notify(&listeners, &response);
            Ok(response)
        })
    }

    /// Invokes one Client Gateway RPC using its generated payload.
    #[wasm_bindgen(js_name = callRpc)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn call_rpc(
        &self,
        _channel: String,
        endpoint: String,
        payload: JsValue,
        signal: JsValue,
    ) -> Promise {
        let listeners = self.listeners.clone();
        future_to_promise(async move {
            let rpc_id = random_uuid()?;
            let request = object(&[
                ("type", JsValue::from_str("client-request")),
                ("rpcId", JsValue::from_str(&rpc_id)),
                ("method", JsValue::from_str(&endpoint)),
                ("payload", payload),
            ])?;
            notify(&listeners, &request.clone().into());
            let response = post_json(&format!("/api/{endpoint}"), request.into(), signal).await?;
            notify(&listeners, &response);
            let echoed = Reflect::get(&response, &JsValue::from_str("rpcId"))?
                .as_string()
                .ok_or_else(|| js_sys::Error::new("connection response omitted rpcId"))?;
            if echoed != rpc_id {
                return Err(js_sys::Error::new(&format!(
                    "rpcId mismatch for {endpoint}: sent {rpc_id}, got {echoed}"
                ))
                .into());
            }
            Reflect::get(&response, &JsValue::from_str("result"))
        })
    }

    /// Subscribes to ordered unary envelopes.
    ///
    /// # Errors
    ///
    /// Returns an error when `listener` is not callable or the listener id space is exhausted.
    #[wasm_bindgen(js_name = subscribeEnvelopes)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn subscribe_envelopes(&self, listener: JsValue) -> Result<Function, JsValue> {
        let listener = listener
            .dyn_into::<Function>()
            .map_err(|_| js_sys::Error::new("connection envelope listener must be a function"))?;
        let id =
            self.next_listener.get().checked_add(1).ok_or_else(|| {
                JsValue::from(js_sys::Error::new("connection listener id exhausted"))
            })?;
        self.next_listener.set(id);
        self.listeners.lock().push((id, listener));
        let listeners = self.listeners.clone();
        let dispose = Closure::wrap(Box::new(move || {
            listeners.lock().retain(|(candidate, _)| *candidate != id);
        }) as Box<dyn FnMut()>);
        Ok(dispose.into_js_value().unchecked_into())
    }
}

impl Default for WasmBrowserApiClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Compiled Client Connection plugin descriptor.
///
/// # Errors
///
/// Returns JavaScript face-construction failures.
#[wasm_bindgen(js_name = clientConnectionPlugin)]
#[allow(clippy::too_many_lines)] // The exported service face is assembled atomically at this boundary.
pub fn client_connection_plugin() -> Result<JsValue, JsValue> {
    plugin("client-connection", &[], |context| {
        let client_core = WasmBrowserApiClient::new();
        let envelope_listeners = client_core.listeners.clone();
        let client: JsValue = client_core.into();
        let api = API_PROXY_FACTORY.with(|factory| factory.call1(&JsValue::UNDEFINED, &client))?;
        let description = Rc::new(Mutex::new(JsValue::UNDEFINED));
        let description_listeners: Listeners = Rc::new(Mutex::new(Vec::new()));
        let next_description_listener = Rc::new(Cell::new(0_u64));
        let get_description = {
            let description = description.clone();
            Closure::wrap(
                Box::new(move || description.lock().clone()) as Box<dyn FnMut() -> JsValue>
            )
        };
        let subscribe_description = {
            let listeners = description_listeners.clone();
            let next = next_description_listener.clone();
            Closure::wrap(Box::new(move |listener: Function| -> Function {
                let id = next.get().wrapping_add(1);
                next.set(id);
                listeners.lock().push((id, listener));
                let listeners = listeners.clone();
                Closure::wrap(Box::new(move || {
                    listeners.lock().retain(|(candidate, _)| *candidate != id);
                }) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_into()
            }) as Box<dyn FnMut(Function) -> Function>)
        };
        let host_description = object(&[
            ("getSnapshot", get_description.into_js_value()),
            ("subscribe", subscribe_description.into_js_value()),
        ])?;
        let started = Rc::new(Cell::new(false));
        let start = {
            let started = started.clone();
            let api = api.clone();
            let description = description.clone();
            let description_listeners = description_listeners.clone();
            let envelope_listeners = envelope_listeners.clone();
            Closure::wrap(Box::new(
                move |sinks: JsValue, _config: JsValue| -> Result<JsValue, JsValue> {
                    if started.replace(true) {
                        return Err(js_sys::Error::new(
                            "connection: the stream loop is already owned by another consumer",
                        )
                        .into());
                    }
                    let host = Reflect::get(&api, &JsValue::from_str("host"))?;
                    let describe = Reflect::get(&host, &JsValue::from_str("describe"))?
                        .dyn_into::<Function>()?;
                    let request = describe.call1(&host, &Object::new())?;
                    let stopped = Rc::new(Cell::new(false));
                    let lost = Rc::new(Cell::new(false));
                    let callbacks = ConnectionCallbacks {
                        sinks,
                        listeners: envelope_listeners.clone(),
                        description: description.clone(),
                        description_listeners: description_listeners.clone(),
                        stopped: stopped.clone(),
                        lost,
                    };
                    let (mux, mux_ready) = match open_browser_socket(
                        "/api/events.mux",
                        "onMuxEnvelope",
                        callbacks.clone(),
                    ) {
                        Ok(socket) => socket,
                        Err(error) => {
                            started.set(false);
                            return Err(error);
                        }
                    };
                    let (host, host_ready) = match open_browser_socket(
                        "/api/events.host",
                        "onHostEnvelope",
                        callbacks.clone(),
                    ) {
                        Ok(socket) => socket,
                        Err(error) => {
                            mux.stop();
                            started.set(false);
                            return Err(error);
                        }
                    };
                    let sockets = Rc::new(RefCell::new(vec![mux, host]));
                    let readiness = Array::new();
                    readiness.push(&request);
                    readiness.push(&mux_ready);
                    readiness.push(&host_ready);
                    let task_callbacks = callbacks.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let Ok(values) = JsFuture::from(Promise::all(&readiness)).await else {
                            publish_connection_loss(&task_callbacks);
                            return;
                        };
                        if task_callbacks.stopped.get() {
                            return;
                        }
                        let response = Array::from(&values).get(0);
                        let Ok(result) = Reflect::get(&response, &JsValue::from_str("result"))
                        else {
                            publish_connection_loss(&task_callbacks);
                            return;
                        };
                        if Reflect::get(&result, &JsValue::from_str("ok"))
                            .ok()
                            .and_then(|value| value.as_bool())
                            != Some(true)
                        {
                            publish_connection_loss(&task_callbacks);
                            return;
                        }
                        let value = Reflect::get(&result, &JsValue::from_str("value"))
                            .unwrap_or(JsValue::UNDEFINED);
                        publish_description(&task_callbacks, value.clone());
                        call_optional(
                            &task_callbacks.sinks,
                            "onStateChange",
                            &[JsValue::from_str("connected")],
                        );
                        if !task_callbacks.stopped.get() {
                            call_optional(&task_callbacks.sinks, "onConnected", &[value]);
                        }
                    });
                    let running = started.clone();
                    let stop_callbacks = callbacks;
                    let stop = Closure::wrap(Box::new(move || {
                        if stop_callbacks.stopped.replace(true) {
                            return;
                        }
                        for socket in sockets.borrow().iter() {
                            socket.stop();
                        }
                        sockets.borrow_mut().clear();
                        running.set(false);
                        publish_description(&stop_callbacks, JsValue::UNDEFINED);
                    }) as Box<dyn FnMut()>);
                    object(&[("stop", stop.into_js_value())]).map(Into::into)
                },
            )
                as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>)
        };
        let hostname = Reflect::get(&js_sys::global(), &JsValue::from_str("location"))
            .ok()
            .and_then(|location| Reflect::get(&location, &JsValue::from_str("hostname")).ok())
            .and_then(|hostname| hostname.as_string())
            .unwrap_or_default();
        let rpc_client = client.clone();
        let rpc_call = Closure::wrap(Box::new(
            move |channel: String,
                  endpoint: String,
                  payload: JsValue,
                  signal: JsValue|
                  -> Promise {
                match call_method(
                    &rpc_client,
                    "callRpc",
                    &[
                        JsValue::from_str(&channel),
                        JsValue::from_str(&endpoint),
                        payload,
                        signal,
                    ],
                ) {
                    Ok(value) => Promise::resolve(&value),
                    Err(error) => Promise::reject(&error),
                }
            },
        )
            as Box<dyn FnMut(String, String, JsValue, JsValue) -> Promise>);
        let rpc = object(&[("call", rpc_call.into_js_value())])?;
        let connection = object(&[
            ("api", api),
            ("rpc", rpc.into()),
            (
                "isLoopback",
                JsValue::from_bool(is_loopback_hostname(&hostname)),
            ),
            ("hostDescription", host_description.into()),
            ("start", start.into_js_value()),
        ])?;
        call_method(
            &context,
            "provide",
            &[JsValue::from_str("connection"), connection.into()],
        )?;
        Ok(())
    })
}

/// Configures the thin Cordis Service bindings around the Rust Client Remote core.
///
/// # Errors
///
/// Returns when either factory is not callable.
#[wasm_bindgen(js_name = configureClientApiGateway)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_api_gateway(
    remote_factory: JsValue,
    namespace_factory: JsValue,
) -> Result<(), JsValue> {
    let remote_factory = remote_factory
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("Client Remote service factory must be callable"))?;
    let namespace_factory = namespace_factory
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("Remote namespace factory must be callable"))?;
    CLIENT_REMOTE_FACTORIES.with(|factories| {
        *factories.borrow_mut() = Some((remote_factory, namespace_factory));
    });
    Ok(())
}

/// Compiled Client Typert registry plugin descriptor.
///
/// # Errors
///
/// Returns JavaScript face-construction failures.
#[wasm_bindgen(js_name = clientTypertRegistryPlugin)]
#[allow(clippy::too_many_lines)]
pub fn client_typert_registry_plugin() -> Result<JsValue, JsValue> {
    plugin("typert-registry", &[], |context| {
        let binders = Rc::new(RefCell::new(HashMap::<String, (u64, JsValue)>::new()));
        let next_binder = Rc::new(Cell::new(0_u64));
        let register_binders = binders.clone();
        let register_next = next_binder;
        let register = Closure::wrap(Box::new(
            move |name: String, descriptor: JsValue| -> Result<Function, JsValue> {
                if name.is_empty() {
                    return Err(js_sys::Error::new(
                        "typert context name must be a non-empty string",
                    )
                    .into());
                }
                let identity = Reflect::get(&descriptor, &JsValue::from_str("identity"))?;
                if !identity.is_function() {
                    return Err(js_sys::TypeError::new(&format!(
                        "Client Context binder {name:?} requires an identity function"
                    ))
                    .into());
                }
                if register_binders.borrow().contains_key(&name) {
                    return Err(js_sys::Error::new(&format!(
                        "Client Context binder {name:?} is already registered"
                    ))
                    .into());
                }
                let id = register_next
                    .get()
                    .checked_add(1)
                    .ok_or_else(|| js_sys::Error::new("Client Context binder ids exhausted"))?;
                register_next.set(id);
                register_binders
                    .borrow_mut()
                    .insert(name.clone(), (id, descriptor));
                let disposal_binders = register_binders.clone();
                let dispose = Closure::wrap(Box::new(move || {
                    if disposal_binders
                        .borrow()
                        .get(&name)
                        .is_some_and(|(current, _)| *current == id)
                    {
                        disposal_binders.borrow_mut().remove(&name);
                    }
                }) as Box<dyn FnMut()>);
                Ok(dispose.into_js_value().unchecked_into())
            },
        )
            as Box<dyn FnMut(String, JsValue) -> Result<Function, JsValue>>);
        let get_binders = binders;
        let get = Closure::wrap(Box::new(move |name: String| {
            get_binders
                .borrow()
                .get(&name)
                .map_or(JsValue::UNDEFINED, |(_, descriptor)| descriptor.clone())
        }) as Box<dyn FnMut(String) -> JsValue>);
        let contexts = object(&[
            ("registerClient", register.into_js_value()),
            ("getClient", get.into_js_value()),
        ])?;
        let remote_rows = Rc::new(RefCell::new(Vec::<(u64, String, JsValue)>::new()));
        let next_remote = Rc::new(Cell::new(0_u64));
        let register_rows = remote_rows.clone();
        let register_next = next_remote;
        let register_remote = Closure::wrap(Box::new(
            move |contribution: JsValue| -> Result<Function, JsValue> {
                let descriptors = Reflect::get(&contribution, &JsValue::from_str("descriptors"))?;
                if !Array::is_array(&descriptors) {
                    return Err(js_sys::TypeError::new(
                        "Typert Remote contribution descriptors must be an array",
                    )
                    .into());
                }
                let id = register_next.get().checked_add(1).ok_or_else(|| {
                    js_sys::Error::new("Typert Remote registration ids exhausted")
                })?;
                register_next.set(id);
                let mut pending = Vec::new();
                for descriptor in Array::from(&descriptors).iter() {
                    let namespace = required_string(&descriptor, "namespace", "Remote descriptor")?;
                    let method = required_string(&descriptor, "method", "Remote descriptor")?;
                    let endpoint = format!("{namespace}/{method}");
                    if pending
                        .iter()
                        .any(|(candidate, _): &(String, JsValue)| candidate == &endpoint)
                        || register_rows
                            .borrow()
                            .iter()
                            .any(|(_, candidate, _)| candidate == &endpoint)
                    {
                        return Err(js_sys::Error::new(&format!(
                            "Typert Remote endpoint {endpoint:?} is already registered"
                        ))
                        .into());
                    }
                    pending.push((endpoint, descriptor));
                }
                register_rows.borrow_mut().extend(
                    pending
                        .iter()
                        .map(|(endpoint, descriptor)| (id, endpoint.clone(), descriptor.clone())),
                );
                let disposal_rows = register_rows.clone();
                let dispose = Closure::wrap(Box::new(move || {
                    disposal_rows
                        .borrow_mut()
                        .retain(|(owner, _, _)| *owner != id);
                }) as Box<dyn FnMut()>);
                Ok(dispose.into_js_value().unchecked_into())
            },
        )
            as Box<dyn FnMut(JsValue) -> Result<Function, JsValue>>);
        let get_rows = remote_rows.clone();
        let get_remote = Closure::wrap(Box::new(move |endpoint: String| {
            get_rows
                .borrow()
                .iter()
                .find(|(_, candidate, _)| candidate == &endpoint)
                .map_or(JsValue::UNDEFINED, |(_, _, descriptor)| descriptor.clone())
        }) as Box<dyn FnMut(String) -> JsValue>);
        let list_rows = remote_rows;
        let list_remote = Closure::wrap(Box::new(move || -> Array {
            list_rows
                .borrow()
                .iter()
                .map(|(_, _, descriptor)| descriptor.clone())
                .collect()
        }) as Box<dyn FnMut() -> Array>);
        let remotes = object(&[
            ("register", register_remote.into_js_value()),
            ("get", get_remote.into_js_value()),
            ("list", list_remote.into_js_value()),
        ])?;
        let typert = object(&[
            ("contexts", contexts.into()),
            ("local", Object::new().into()),
            ("remotes", remotes.into()),
            ("lookups", Object::new().into()),
        ])?;
        crate::wasm_typert::install(&typert, &context)?;
        call_method(
            &context,
            "provide",
            &[JsValue::from_str("typert"), typert.into()],
        )?;
        Ok(())
    })
}

/// Compiled Client API gateway plugin descriptor.
///
/// # Errors
///
/// Returns missing Connection or JavaScript face-construction failures.
#[wasm_bindgen(js_name = clientApiGatewayPlugin)]
pub fn client_api_gateway_plugin() -> Result<JsValue, JsValue> {
    plugin("api-gateway", &["connection", "typert"], |context| {
        let connection = call_method(&context, "get", &[JsValue::from_str("connection")])?;
        let typert = call_method(&context, "get", &[JsValue::from_str("typert")])?;
        let (remote_factory, namespace_factory) = CLIENT_REMOTE_FACTORIES
            .with(|factories| factories.borrow().clone())
            .ok_or_else(|| {
                js_sys::Error::new(
                    "Client API Gateway module factory did not configure Cordis Service bindings",
                )
            })?;
        let core: JsValue = crate::WasmClientRemoteCore::new(
            context.clone(),
            connection,
            typert,
            namespace_factory,
        )?
        .into();
        remote_factory.call2(&JsValue::UNDEFINED, &context, &core)?;
        Ok(())
    })
}

fn plugin(
    name: &str,
    inject: &[&str],
    apply: impl Fn(JsValue) -> Result<(), JsValue> + 'static,
) -> Result<JsValue, JsValue> {
    let apply = Closure::wrap(Box::new(move |context: JsValue| apply(context))
        as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let plugin = Object::new();
    set(&plugin, "name", &JsValue::from_str(name))?;
    let dependencies = Array::new();
    for dependency in inject {
        dependencies.push(&JsValue::from_str(dependency));
    }
    set(&plugin, "inject", &dependencies.into())?;
    set(&plugin, "apply", &apply.into_js_value())?;
    Ok(plugin.into())
}

async fn post_json(path: &str, body: JsValue, signal: JsValue) -> Result<JsValue, JsValue> {
    let options = RequestInit::new();
    options.set_method("POST");
    options.set_body(&JSON::stringify(&body)?.into());
    if !signal.is_undefined() && !signal.is_null() {
        options.set_signal(signal.dyn_ref::<web_sys::AbortSignal>());
    }
    let origin = Reflect::get(&js_sys::global(), &JsValue::from_str("location"))
        .ok()
        .filter(JsValue::is_object)
        .and_then(|location| Reflect::get(&location, &JsValue::from_str("origin")).ok())
        .and_then(|origin| origin.as_string())
        .unwrap_or_else(|| "http://seekdeep.internal".to_owned());
    let url = web_sys::Url::new_with_base(path, &origin)?.href();
    let request = Request::new_with_str_and_init(&url, &options)?;
    request.headers().set("content-type", "application/json")?;
    let global = js_sys::global();
    let fetch = Reflect::get(&global, &JsValue::from_str("fetch"))?.dyn_into::<Function>()?;
    let response = JsFuture::from(Promise::resolve(&fetch.call1(&global, &request)?))
        .await?
        .dyn_into::<Response>()?;
    if !response.ok() {
        return Err(js_sys::Error::new(&format!(
            "transport failure for {path}: HTTP {}",
            response.status()
        ))
        .into());
    }
    JsFuture::from(response.json()?).await
}

fn random_uuid() -> Result<String, JsValue> {
    let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))?;
    call_method(&crypto, "randomUUID", &[])?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("crypto.randomUUID returned a non-string").into())
}

fn is_loopback_hostname(hostname: &str) -> bool {
    if hostname.eq_ignore_ascii_case("localhost") || hostname == "[::1]" || hostname == "::1" {
        return true;
    }
    let parts = hostname.split('.').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0] == "127"
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u16>().is_ok_and(|value| value <= 255)
        })
}

#[allow(clippy::too_many_lines)]
fn open_browser_socket(
    path: &'static str,
    sink_name: &'static str,
    callbacks: ConnectionCallbacks,
) -> Result<(BrowserSocket, Promise), JsValue> {
    let socket = WebSocket::new(&websocket_url(path)?)?;
    let resolve_slot = Rc::new(RefCell::new(None::<Function>));
    let reject_slot = Rc::new(RefCell::new(None::<Function>));
    let resolve_capture = resolve_slot.clone();
    let reject_capture = reject_slot.clone();
    let readiness = Promise::new(&mut move |resolve, reject| {
        *resolve_capture.borrow_mut() = Some(resolve);
        *reject_capture.borrow_mut() = Some(reject);
    });
    let resolve = resolve_slot
        .borrow_mut()
        .take()
        .ok_or_else(|| js_sys::Error::new("WebSocket readiness omitted resolve"))?;
    let reject = reject_slot
        .borrow_mut()
        .take()
        .ok_or_else(|| js_sys::Error::new("WebSocket readiness omitted reject"))?;

    let on_open = Closure::wrap(Box::new(move |_event: Event| {
        let _ = resolve.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut(Event)>);
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

    let message_callbacks = callbacks.clone();
    let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
        let result = event
            .data()
            .as_string()
            .ok_or_else(|| js_sys::Error::new("binary WebSocket frame").into())
            .and_then(|text| decode_websocket_frame(path, &text));
        match result {
            Ok((full, narrow)) => {
                notify(&message_callbacks.listeners, &full);
                let payload = Reflect::get(&narrow, &JsValue::from_str("payload"))
                    .unwrap_or(JsValue::UNDEFINED);
                if Reflect::get(&payload, &JsValue::from_str("type"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .as_deref()
                    == Some("stream/error")
                {
                    let detail = JSON::stringify(&payload)
                        .ok()
                        .and_then(|value| value.as_string())
                        .unwrap_or_else(|| "stream/error".to_owned());
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[client-connection] {path} ended: {detail}"
                    )));
                    publish_connection_loss(&message_callbacks);
                    return;
                }
                if path == "/api/events.mux"
                    && Reflect::get(&payload, &JsValue::from_str("sessionId"))
                        .ok()
                        .and_then(|value| value.as_string())
                        .is_none()
                {
                    let detail = JSON::stringify(&payload)
                        .ok()
                        .and_then(|value| value.as_string())
                        .unwrap_or_else(|| "unknown mux payload".to_owned());
                    web_sys::console::error_1(&JsValue::from_str(&format!(
                        "[client-connection] dropping mux frame without sessionId: {detail}"
                    )));
                    return;
                }
                call_optional(&message_callbacks.sinks, sink_name, &[narrow]);
            }
            Err(error) => web_sys::console::error_2(
                &JsValue::from_str(&format!(
                    "[client-connection] dropping malformed WebSocket frame on {path}:"
                )),
                &error,
            ),
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

    let close_callbacks = callbacks.clone();
    let close_reject = reject.clone();
    let on_close = Closure::wrap(Box::new(move |_event: Event| {
        let _ = close_reject.call1(
            &JsValue::UNDEFINED,
            &js_sys::Error::new(&format!("WebSocket {path} closed before readiness")),
        );
        publish_connection_loss(&close_callbacks);
    }) as Box<dyn FnMut(Event)>);
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

    let error_callbacks = callbacks;
    let on_error = Closure::wrap(Box::new(move |_event: Event| {
        let _ = reject.call1(
            &JsValue::UNDEFINED,
            &js_sys::Error::new(&format!("WebSocket {path} failed before readiness")),
        );
        publish_connection_loss(&error_callbacks);
    }) as Box<dyn FnMut(Event)>);
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    Ok((
        BrowserSocket {
            socket,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
            _on_error: on_error,
        },
        readiness,
    ))
}

fn websocket_url(path: &str) -> Result<String, JsValue> {
    let origin = Reflect::get(&js_sys::global(), &JsValue::from_str("location"))
        .ok()
        .filter(JsValue::is_object)
        .and_then(|location| Reflect::get(&location, &JsValue::from_str("origin")).ok())
        .and_then(|origin| origin.as_string())
        .unwrap_or_else(|| "http://seekdeep.internal".to_owned());
    let url = web_sys::Url::new_with_base(path, &origin)?;
    let protocol = if url.protocol() == "https:" {
        "wss:"
    } else {
        "ws:"
    };
    url.set_protocol(protocol);
    Ok(url.href())
}

fn decode_websocket_frame(path: &str, text: &str) -> Result<(JsValue, JsValue), JsValue> {
    let full = JSON::parse(text)?;
    let kind = Reflect::get(&full, &JsValue::from_str("type"))?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("WebSocket envelope omitted type"))?;
    if kind != "server-request" {
        return Err(js_sys::Error::new(&format!(
            "WebSocket {path} expected server-request, received {kind:?}"
        ))
        .into());
    }
    let rpc_id = Reflect::get(&full, &JsValue::from_str("rpcId"))?;
    if rpc_id.as_string().is_none() {
        return Err(js_sys::Error::new("WebSocket envelope omitted rpcId").into());
    }
    let payload = Reflect::get(&full, &JsValue::from_str("payload"))?;
    if !payload.is_object() || payload.is_null() {
        return Err(js_sys::Error::new("WebSocket envelope payload must be an object").into());
    }
    let payload_kind = Reflect::get(&payload, &JsValue::from_str("type"))?;
    if payload_kind.as_string().is_none() {
        return Err(js_sys::Error::new("WebSocket frame omitted payload type").into());
    }
    let narrow = object(&[("rpcId", rpc_id), ("payload", payload)])?;
    Ok((full, narrow.into()))
}

fn publish_connection_loss(callbacks: &ConnectionCallbacks) {
    if callbacks.stopped.get() || callbacks.lost.replace(true) {
        return;
    }
    publish_description(callbacks, JsValue::UNDEFINED);
    call_optional(
        &callbacks.sinks,
        "onStateChange",
        &[JsValue::from_str("reconnecting")],
    );
}

fn publish_description(callbacks: &ConnectionCallbacks, value: JsValue) {
    let mut current = callbacks.description.lock();
    if Object::is(&current, &value) {
        return;
    }
    *current = value;
    drop(current);
    notify_empty(&callbacks.description_listeners);
}

fn notify(listeners: &Listeners, value: &JsValue) {
    let batch = Array::of1(value);
    for (_, listener) in listeners.lock().clone() {
        if let Err(error) = listener.call1(&JsValue::UNDEFINED, &batch) {
            web_sys::console::error_2(
                &JsValue::from_str("[connection] envelope listener threw"),
                &error,
            );
        }
    }
}

fn notify_empty(listeners: &Listeners) {
    for (_, listener) in listeners.lock().clone() {
        let _ = listener.call0(&JsValue::UNDEFINED);
    }
}

fn call_optional(value: &JsValue, name: &str, arguments: &[JsValue]) {
    let Ok(function) = Reflect::get(value, &JsValue::from_str(name)) else {
        return;
    };
    let Ok(function) = function.dyn_into::<Function>() else {
        return;
    };
    let arguments: Array = arguments.iter().cloned().collect();
    if let Err(error) = function.apply(value, &arguments) {
        let label = Reflect::get(&function, &JsValue::from_str("__seekdeepSinkLabel"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_else(|| name.to_owned());
        let detail = Reflect::get(&error, &JsValue::from_str("message"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_else(|| format!("{error:?}"));
        web_sys::console::error_1(&JsValue::from_str(&format!(
            "[connection] sink {label} threw: {detail}"
        )));
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().cloned().collect();
    method.apply(value, &arguments)
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (name, value) in entries {
        set(&object, name, value)?;
    }
    Ok(object)
}

fn set(object: &Object, name: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(name), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set browser foundation member {name:?}")).into())
    }
}
