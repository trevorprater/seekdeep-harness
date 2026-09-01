//! JavaScript faces over Rust-owned same-origin unary transport and services.

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use parking_lot::Mutex;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};
use web_sys::{Request, RequestInit, Response};

thread_local! {
    static API_PROXY_FACTORY: Function = Function::new_with_args(
        "client",
        r"
const namespaces = new Map();
const prefixes = { sessions: 'session', subagents: 'subagent', skills: 'skill', agentPresets: 'agentPreset', goals: 'goal' };
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
}

type Listeners = Rc<Mutex<Vec<(u64, Function)>>>;

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
        let client: JsValue = WasmBrowserApiClient::new().into();
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
        let active_sinks = Rc::new(Mutex::new(None::<JsValue>));
        let start = {
            let started = started.clone();
            let active_sinks = active_sinks.clone();
            let api = api.clone();
            let description = description.clone();
            let description_listeners = description_listeners.clone();
            Closure::wrap(Box::new(
                move |sinks: JsValue, _config: JsValue| -> Result<JsValue, JsValue> {
                    if started.replace(true) {
                        return Err(js_sys::Error::new(
                            "connection: the stream loop is already owned by another consumer",
                        )
                        .into());
                    }
                    *active_sinks.lock() = Some(sinks.clone());
                    let host = Reflect::get(&api, &JsValue::from_str("host"))?;
                    let describe = Reflect::get(&host, &JsValue::from_str("describe"))?
                        .dyn_into::<Function>()?;
                    let request = describe.call1(&host, &Object::new())?;
                    let sinks_for_task = sinks.clone();
                    let description_for_task = description.clone();
                    let listeners_for_task = description_listeners.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let Ok(response) = JsFuture::from(Promise::resolve(&request)).await else {
                            call_optional(
                                &sinks_for_task,
                                "onStateChange",
                                &[JsValue::from_str("reconnecting")],
                            );
                            return;
                        };
                        let Ok(result) = Reflect::get(&response, &JsValue::from_str("result"))
                        else {
                            return;
                        };
                        if Reflect::get(&result, &JsValue::from_str("ok"))
                            .ok()
                            .and_then(|value| value.as_bool())
                            != Some(true)
                        {
                            call_optional(
                                &sinks_for_task,
                                "onStateChange",
                                &[JsValue::from_str("reconnecting")],
                            );
                            return;
                        }
                        let value = Reflect::get(&result, &JsValue::from_str("value"))
                            .unwrap_or(JsValue::UNDEFINED);
                        *description_for_task.lock() = value.clone();
                        notify_empty(&listeners_for_task);
                        call_optional(
                            &sinks_for_task,
                            "onStateChange",
                            &[JsValue::from_str("connected")],
                        );
                        call_optional(&sinks_for_task, "onConnected", &[value]);
                    });
                    let stopped = started.clone();
                    let active_sinks = active_sinks.clone();
                    let description = description.clone();
                    let listeners = description_listeners.clone();
                    let stop = Closure::wrap(Box::new(move || {
                        stopped.set(false);
                        *active_sinks.lock() = None;
                        *description.lock() = JsValue::UNDEFINED;
                        notify_empty(&listeners);
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
        let connection = object(&[
            ("api", api),
            ("rpc", Object::new().into()),
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

/// Compiled Client Typert registry plugin descriptor.
///
/// # Errors
///
/// Returns JavaScript face-construction failures.
#[wasm_bindgen(js_name = clientTypertRegistryPlugin)]
pub fn client_typert_registry_plugin() -> Result<JsValue, JsValue> {
    plugin("typert-registry", &[], |context| {
        let register = Closure::wrap(Box::new(move |_name: String, _descriptor: JsValue| {
            let dispose = Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>);
            dispose.into_js_value().unchecked_into::<Function>()
        }) as Box<dyn FnMut(String, JsValue) -> Function>);
        let contexts = object(&[("registerClient", register.into_js_value())])?;
        let typert = object(&[
            ("contexts", contexts.into()),
            ("local", Object::new().into()),
            ("remotes", Object::new().into()),
            ("lookups", Object::new().into()),
        ])?;
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
        let api = Reflect::get(&connection, &JsValue::from_str("api"))?;
        let mount = Closure::wrap(Box::new(move |_contribution: JsValue| -> Promise {
            let dispose = Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>);
            Promise::resolve(&dispose.into_js_value())
        }) as Box<dyn FnMut(JsValue) -> Promise>);
        Reflect::set(&api, &JsValue::from_str("$mount"), &mount.into_js_value())?;
        let dispatch = Closure::wrap(Box::new(|_event: JsValue| {}) as Box<dyn FnMut(JsValue)>);
        Reflect::set(
            &api,
            &JsValue::from_str("$dispatch"),
            &dispatch.into_js_value(),
        )?;
        let on = Closure::wrap(
            Box::new(move |_event: String, _listener: Function| -> Function {
                Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>)
                    .into_js_value()
                    .unchecked_into()
            }) as Box<dyn FnMut(String, Function) -> Function>,
        );
        Reflect::set(&api, &JsValue::from_str("$on"), &on.into_js_value())?;
        provide(&context, "remote", &api)?;
        for namespace in [
            "commands",
            "goals",
            "dynamicCordisRunner",
            "pluginInventory",
            "messageFeedback",
        ] {
            let service = Reflect::get(&api, &JsValue::from_str(namespace))?;
            provide(&context, &format!("remote.{namespace}"), &service)?;
        }
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

fn provide(context: &JsValue, name: &str, value: &JsValue) -> Result<(), JsValue> {
    call_method(
        context,
        "provide",
        &[JsValue::from_str(name), value.clone()],
    )?;
    Ok(())
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
