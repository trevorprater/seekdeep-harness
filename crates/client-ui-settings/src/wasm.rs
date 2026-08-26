//! Browser WASM facade, Cordis service binding, and generated settings API adapter.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt, future::LocalBoxFuture};
use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_settings_contract::{
    ClientSettingsDecoder, ClientSettingsDisposer, ClientSettingsMode, ClientSettingsNamespace,
    ClientSettingsScopeSnapshot, ClientSettingsScopeSpec, ClientSettingsStatus,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use crate::{
    ClientSettingsDescribeValue, ClientSettingsMutateRequest, ClientSettingsNamespaceView,
    ClientSettingsOperationError, ClientSettingsScopeController, ClientSettingsTaskSpawner,
    ClientSettingsTransport, SettingsRpcResult,
};

thread_local! {
    static BINDER_CONSTRUCTOR: RefCell<Option<Function>> = const { RefCell::new(None) };
}

struct BrowserSettingsSpawner;

impl ClientSettingsTaskSpawner for BrowserSettingsSpawner {
    fn spawn(&self, task: LocalBoxFuture<'static, ()>) {
        spawn_local(task);
    }
}

struct BrowserSettingsTransport {
    settings: JsValue,
}

impl ClientSettingsTransport for BrowserSettingsTransport {
    fn describe(
        &self,
    ) -> LocalBoxFuture<'static, Result<SettingsRpcResult<ClientSettingsDescribeValue>, String>>
    {
        let settings = self.settings.clone();
        async move {
            let request = Object::new();
            let response = await_method(&settings, "describe", &[request.into()]).await?;
            parse_rpc_response(&response)
        }
        .boxed_local()
    }

    fn mutate(
        &self,
        request: ClientSettingsMutateRequest,
    ) -> LocalBoxFuture<'static, Result<SettingsRpcResult<ClientSettingsNamespaceView>, String>>
    {
        let settings = self.settings.clone();
        async move {
            let request = json_to_js(&request).map_err(|error| error_text(&error))?;
            let response = await_method(&settings, "mutate", &[request]).await?;
            parse_rpc_response(&response)
        }
        .boxed_local()
    }
}

type SnapshotCache = Rc<RefCell<Option<(Rc<ClientSettingsScopeSnapshot<Value>>, JsValue)>>>;

/// Compiled implementation of the source `SettingsScopeController` class.
#[wasm_bindgen(js_name = __SettingsScopeController)]
pub struct WasmSettingsScopeController {
    controller: Rc<ClientSettingsScopeController>,
    snapshot_cache: SnapshotCache,
}

#[wasm_bindgen(js_class = __SettingsScopeController)]
impl WasmSettingsScopeController {
    /// Creates a controller over a generated API face.
    ///
    /// # Errors
    ///
    /// Returns malformed API, spec, decoder, or persistence-mode failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        api: JsValue,
        spec: JsValue,
        persistence: Option<String>,
    ) -> Result<WasmSettingsScopeController, JsValue> {
        let mode = match persistence.as_deref().unwrap_or("host") {
            "host" => ClientSettingsMode::Host,
            "memory" => ClientSettingsMode::Memory,
            value => {
                return Err(js_error(&format!(
                    "ui-settings: unknown persistence mode {value:?}"
                )));
            }
        };
        Self::from_api(&api, &spec, mode)
    }

    /// Current frozen, reference-stable JavaScript snapshot.
    ///
    /// # Errors
    ///
    /// Returns JSON conversion or property-definition failures.
    #[wasm_bindgen(js_name = getSnapshot)]
    pub fn get_snapshot(&self) -> Result<JsValue, JsValue> {
        self.snapshot_value()
    }

    /// Observes committed snapshot replacements.
    #[wasm_bindgen]
    #[must_use]
    pub fn subscribe(&self, listener: Function) -> Function {
        let subscription = self.controller.subscribe_fallible(Rc::new(move || {
            listener
                .call0(&JsValue::UNDEFINED)
                .map(|_| ())
                .map_err(|error| ClientSettingsOperationError::new(error_text(&error)))
        }));
        disposer(subscription)
    }

    /// Queues one Host refresh.
    #[wasm_bindgen]
    pub fn load(&self) -> Promise {
        operation_promise(self.controller.load())
    }

    /// Queues one field replacement.
    ///
    /// # Errors
    ///
    /// Returns a synchronous JSON conversion failure before the queued Promise exists.
    #[wasm_bindgen]
    #[allow(clippy::needless_pass_by_value)]
    pub fn set(&self, field: String, value: JsValue) -> Result<Promise, JsValue> {
        let value =
            serde_wasm_bindgen::from_value(value).map_err(|error| js_error(&error.to_string()))?;
        Ok(operation_promise(self.controller.set_field(field, value)))
    }

    /// Queues one user-layer field removal.
    #[wasm_bindgen]
    pub fn unset(&self, field: String) -> Promise {
        operation_promise(self.controller.unset_field(field))
    }

    /// Stops queued work and settles after the crossing Host call.
    #[wasm_bindgen]
    pub fn dispose(&self) -> Promise {
        let future = self.controller.dispose();
        future_to_promise(async move {
            future.await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

impl WasmSettingsScopeController {
    fn from_api(api: &JsValue, spec: &JsValue, mode: ClientSettingsMode) -> Result<Self, JsValue> {
        let settings = required_property(api, "settings", "generated API")?;
        let spec = settings_spec(spec)?;
        let controller = ClientSettingsScopeController::new(
            Rc::new(BrowserSettingsTransport { settings }),
            Rc::new(BrowserSettingsSpawner),
            spec,
            mode,
        );
        Ok(Self {
            controller,
            snapshot_cache: Rc::new(RefCell::new(None)),
        })
    }

    fn snapshot_value(&self) -> Result<JsValue, JsValue> {
        let snapshot = self.controller.snapshot();
        if let Some((current, value)) = self.snapshot_cache.borrow().as_ref()
            && Rc::ptr_eq(current, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = snapshot_to_js(&snapshot)?;
        *self.snapshot_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }
}

/// Stores the compatibility `SettingsScopeBinder` constructor materialized by the module factory.
#[wasm_bindgen(js_name = configureClientUiSettings)]
pub fn configure_client_ui_settings(constructor: Function) {
    BINDER_CONSTRUCTOR.with(|current| {
        *current.borrow_mut() = Some(constructor);
    });
}

/// Browser Client plugin apply function. The Cordis `Service` constructor provides exact tracing.
///
/// # Errors
///
/// Returns missing module configuration or Binder construction failures.
#[wasm_bindgen(js_name = applyClientUiSettings)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_settings(ctx: JsValue) -> Result<(), JsValue> {
    let constructor = BINDER_CONSTRUCTOR.with(|current| current.borrow().clone());
    let constructor = constructor.ok_or_else(|| {
        js_error("client-ui-settings module factory did not configure SettingsScopeBinder")
    })?;
    let arguments = Array::new();
    arguments.push(&ctx);
    Reflect::construct(&constructor, &arguments)?;
    Ok(())
}

/// Binds one namespace through the caller's traced Cordis Context.
///
/// # Errors
///
/// Returns missing services, malformed API/spec, listener, effect, or conversion failures.
#[wasm_bindgen(js_name = bindSettingsScope)]
#[allow(clippy::needless_pass_by_value)]
pub fn bind_settings_scope(caller: JsValue, spec: JsValue) -> Result<JsValue, JsValue> {
    let connection = required_service(&caller, "connection")?;
    let api = required_property(&connection, "api", "connection")?;
    let loopback = Reflect::get(&connection, &JsValue::from_str("isLoopback"))?
        .as_bool()
        .unwrap_or(false);
    let mode = if loopback {
        ClientSettingsMode::Host
    } else {
        ClientSettingsMode::Memory
    };
    let scope = WasmSettingsScopeController::from_api(&api, &spec, mode)?;
    let controller = scope.controller.clone();
    let namespace = required_string(&spec, "namespace", "settings scope spec")?;
    let remote = required_service(&caller, "remote")?;

    let remote_controller = controller.clone();
    let remote_namespace = namespace.clone();
    let remote_listener = Closure::wrap(Box::new(move |updated_namespace: JsValue| {
        if updated_namespace
            .as_string()
            .is_some_and(|updated| updated != remote_namespace)
        {
            return;
        }
        drop(remote_controller.load());
    }) as Box<dyn FnMut(JsValue)>);
    let remote_disposer = call_method(
        &remote,
        "$on",
        &[
            JsValue::from_str("settings/document-updated"),
            remote_listener.into_js_value(),
        ],
    )?
    .dyn_into::<Function>()?;

    let reset_controller = controller.clone();
    let reset_listener = Closure::wrap(Box::new(move || {
        drop(reset_controller.load());
    }) as Box<dyn FnMut()>);
    let reset_disposer = match call_method(
        &caller,
        "on",
        &[
            JsValue::from_str("connection/reset"),
            reset_listener.into_js_value(),
        ],
    ) {
        Ok(value) => match value.dyn_into::<Function>() {
            Ok(disposer) => disposer,
            Err(error) => {
                let _ = remote_disposer.call0(&JsValue::UNDEFINED);
                controller.cancel();
                return Err(error);
            }
        },
        Err(error) => {
            let _ = remote_disposer.call0(&JsValue::UNDEFINED);
            controller.cancel();
            return Err(error);
        }
    };

    let pending_disposers = Rc::new(RefCell::new(Some(vec![remote_disposer, reset_disposer])));
    let owned_disposers = pending_disposers.clone();
    let dispose_controller = controller.clone();
    let installer = Closure::wrap(Box::new(move || -> Function {
        let disposers = owned_disposers.borrow_mut().take().unwrap_or_default();
        let controller = dispose_controller.clone();
        Closure::wrap(Box::new(move || -> Promise {
            for dispose in &disposers {
                if let Err(error) = dispose.call0(&JsValue::UNDEFINED) {
                    return Promise::reject(&error);
                }
            }
            let future = controller.dispose();
            future_to_promise(async move {
                future.await;
                Ok(JsValue::UNDEFINED)
            })
        }) as Box<dyn FnMut() -> Promise>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut() -> Function>);
    if let Err(error) = call_method(
        &caller,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str(&format!("ui-settings: {namespace} settings scope")),
        ],
    ) {
        if let Some(disposers) = pending_disposers.borrow_mut().take() {
            for dispose in disposers {
                let _ = dispose.call0(&JsValue::UNDEFINED);
            }
        }
        controller.cancel();
        return Err(error);
    }
    drop(controller.load());
    Ok(scope.into())
}

/// Exact empty Client plugin inject list.
#[wasm_bindgen(js_name = settingsInject)]
pub fn settings_inject() -> Array {
    Array::new()
}

fn settings_spec(spec: &JsValue) -> Result<ClientSettingsScopeSpec<Value>, JsValue> {
    let namespace = required_string(spec, "namespace", "settings scope spec")?;
    let decode = Reflect::get(spec, &JsValue::from_str("decode"))?;
    let decode = if decode.is_undefined() {
        None
    } else {
        let decode = decode
            .dyn_into::<Function>()
            .map_err(|_| js_error("ui-settings: settings scope spec decode must be a function"))?;
        let decoder: ClientSettingsDecoder<Value> = Rc::new(move |value| {
            let value = json_to_js(value).map_err(|error| error_text(&error))?;
            let decoded = decode
                .call1(&JsValue::UNDEFINED, &value)
                .map_err(|error| error_text(&error))?;
            if decoded.is_undefined() {
                return Ok(None);
            }
            serde_wasm_bindgen::from_value(decoded)
                .map(Some)
                .map_err(|error| error.to_string())
        });
        Some(decoder)
    };
    Ok(ClientSettingsScopeSpec {
        namespace: ClientSettingsNamespace::new(namespace),
        decode,
    })
}

fn snapshot_to_js(snapshot: &ClientSettingsScopeSnapshot<Value>) -> Result<JsValue, JsValue> {
    let object = Object::new();
    let status = match snapshot.status {
        ClientSettingsStatus::Loading => "loading",
        ClientSettingsStatus::Ready => "ready",
        ClientSettingsStatus::Unavailable => "unavailable",
    };
    set(&object, "status", &JsValue::from_str(status))?;
    let value = snapshot
        .value
        .as_deref()
        .map_or(Ok(JsValue::UNDEFINED), json_to_js)?;
    set(&object, "value", &value)?;
    let base = snapshot
        .base
        .as_ref()
        .map_or(Ok(JsValue::UNDEFINED), json_to_js)?;
    set(&object, "base", &base)?;
    let user = snapshot
        .user
        .as_ref()
        .map_or(Ok(JsValue::UNDEFINED), json_to_js)?;
    set(&object, "user", &user)?;
    set(
        &object,
        "revision",
        &snapshot
            .revision
            .map_or(JsValue::UNDEFINED, JsValue::from_f64),
    )?;
    set(&object, "writable", &JsValue::from_bool(snapshot.writable))?;
    set(
        &object,
        "mode",
        &JsValue::from_str(match snapshot.mode {
            ClientSettingsMode::Host => "host",
            ClientSettingsMode::Memory => "memory",
        }),
    )?;
    Object::freeze(&object);
    Ok(object.into())
}

fn operation_promise(
    future: LocalBoxFuture<'static, Result<(), ClientSettingsOperationError>>,
) -> Promise {
    future_to_promise(async move {
        future
            .await
            .map(|()| JsValue::UNDEFINED)
            .map_err(|error| js_error(&error.to_string()))
    })
}

fn disposer(disposer: ClientSettingsDisposer) -> Function {
    Closure::wrap(Box::new(move || disposer.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, String> {
    let result = call_method(value, name, arguments).map_err(|error| error_text(&error))?;
    JsFuture::from(Promise::resolve(&result))
        .await
        .map_err(|error| error_text(&error))
}

fn parse_rpc_response<T: DeserializeOwned>(
    response: &JsValue,
) -> Result<SettingsRpcResult<T>, String> {
    let result =
        Reflect::get(response, &JsValue::from_str("result")).map_err(|error| error_text(&error))?;
    let ok = Reflect::get(&result, &JsValue::from_str("ok"))
        .map_err(|error| error_text(&error))?
        .as_bool()
        .ok_or_else(|| "ui-settings: RPC result omitted boolean ok".to_owned())?;
    if !ok {
        return Ok(SettingsRpcResult::Rejected);
    }
    let value =
        Reflect::get(&result, &JsValue::from_str("value")).map_err(|error| error_text(&error))?;
    serde_wasm_bindgen::from_value(value)
        .map(SettingsRpcResult::Success)
        .map_err(|error| error.to_string())
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() {
        Err(js_error(&format!(
            "client-ui-settings requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_error(&format!(
            "ui-settings: {owner} omitted required property {key:?}"
        )))
    } else {
        Ok(property)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_string()
        .ok_or_else(|| {
            js_error(&format!(
                "ui-settings: {owner} omitted string property {key:?}"
            ))
        })
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn json_to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|error| js_error(&error.to_string()))
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn error_text(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}
