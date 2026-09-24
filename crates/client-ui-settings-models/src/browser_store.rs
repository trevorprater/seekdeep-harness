//! Browser API adapters and observable faces for model settings and welcome state.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Array, Function, JSON, Map, Object, Promise, Reflect};
use seekdeep_client_runtime::{SnapshotStore, SnapshotStoreSubscription};
use serde::de::DeserializeOwned;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ConfigurableProviderView, CredentialView, ModelsSettingsState, ModelsSettingsStore,
    ModelsStatus, ModelsTransport, ProviderRow, SettingsNamespaceView, WelcomeNoticeState,
    WelcomeNoticeStore, WelcomePersistence, WelcomeStatus, WelcomeTransport,
    browser::{call_async, call_method, future_promise, object, rejection_text, required, set},
};

#[derive(Clone)]
struct BrowserModelsTransport {
    api: JsValue,
}

impl ModelsTransport for BrowserModelsTransport {
    fn providers(&self) -> LocalBoxFuture<'static, Result<Vec<ConfigurableProviderView>, String>> {
        let api = self.api.clone();
        async move {
            let llm = required(&api, "llm", "API client").map_err(js_error)?;
            let response = call_async(&llm, "providers", &[Object::new().into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            from_js(&required(&value, "providers", "llm.providers value").map_err(js_error)?)
        }
        .boxed_local()
    }

    fn settings(
        &self,
    ) -> LocalBoxFuture<'static, Result<(bool, Vec<SettingsNamespaceView>), String>> {
        let api = self.api.clone();
        async move {
            let settings = required(&api, "settings", "API client").map_err(js_error)?;
            let response = call_async(&settings, "describe", &[Object::new().into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            let writable = required(&value, "writable", "settings.describe value")
                .map_err(js_error)?
                .as_bool()
                .ok_or_else(|| "settings.describe writable must be a boolean".to_owned())?;
            let namespaces = from_js(
                &required(&value, "namespaces", "settings.describe value").map_err(js_error)?,
            )?;
            Ok((writable, namespaces))
        }
        .boxed_local()
    }

    fn credentials(
        &self,
        references: Vec<String>,
    ) -> LocalBoxFuture<'static, Result<BTreeMap<String, CredentialView>, String>> {
        let api = self.api.clone();
        async move {
            let credentials = required(&api, "credentials", "API client").map_err(js_error)?;
            let refs = Array::new();
            for reference in references {
                refs.push(&JsValue::from_str(&reference));
            }
            let response = call_async(
                &credentials,
                "describe",
                &[object(&[("refs", refs.into())]).map_err(js_error)?.into()],
            )
            .await
            .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            from_js(
                &required(&value, "credentials", "credentials.describe value").map_err(js_error)?,
            )
        }
        .boxed_local()
    }
}

#[derive(Clone)]
struct BrowserWelcomeTransport {
    api: JsValue,
}

impl WelcomeTransport for BrowserWelcomeTransport {
    fn describe(&self) -> LocalBoxFuture<'static, Result<Option<String>, String>> {
        let api = self.api.clone();
        async move {
            let settings = required(&api, "settings", "API client").map_err(js_error)?;
            let response = call_async(&settings, "describe", &[Object::new().into()])
                .await
                .map_err(|error| rejection_text(&error))?;
            let value = rpc_value(&response)?;
            let namespaces = required(&value, "namespaces", "settings.describe value")
                .map_err(js_error)?
                .dyn_into::<Array>()
                .map_err(|_| "settings.describe namespaces must be an array".to_owned())?;
            let view = namespaces.iter().find(|candidate| {
                Reflect::get(candidate, &JsValue::from_str("ns"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .as_deref()
                    == Some(crate::WELCOME_NOTICE_SETTINGS_NAMESPACE)
            });
            let Some(view) = view else {
                return Err("welcome acknowledgement settings are unavailable".to_owned());
            };
            let value = required(&view, "value", "welcome settings namespace").map_err(js_error)?;
            if value.is_null() || !value.is_object() {
                return Ok(None);
            }
            Ok(
                Reflect::get(&value, &JsValue::from_str(crate::WELCOME_NOTICE_ACK_FIELD))
                    .ok()
                    .and_then(|value| value.as_string()),
            )
        }
        .boxed_local()
    }

    fn acknowledge(
        &self,
        namespace: &'static str,
        field: &'static str,
        version: &'static str,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        let api = self.api.clone();
        async move {
            let settings = required(&api, "settings", "API client").map_err(js_error)?;
            let path = Array::of1(&JsValue::from_str(field));
            let operation = object(&[
                ("op", JsValue::from_str("set")),
                ("path", path.into()),
                ("value", JsValue::from_str(version)),
            ])
            .map_err(js_error)?;
            let response = call_async(
                &settings,
                "mutate",
                &[object(&[
                    ("ns", JsValue::from_str(namespace)),
                    ("ops", Array::of1(operation.as_ref()).into()),
                ])
                .map_err(js_error)?
                .into()],
            )
            .await
            .map_err(|error| rejection_text(&error))?;
            rpc_value(&response).map(|_| ())
        }
        .boxed_local()
    }
}

/// Creates the Rust-owned Models page controller and its observable store face.
///
/// # Errors
///
/// Returns when the browser API value cannot be captured into the controller face.
#[wasm_bindgen(js_name = createModelsSettingsController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_models_settings_controller(api: JsValue) -> Result<JsValue, JsValue> {
    let controller = ModelsSettingsStore::new(Rc::new(BrowserModelsTransport { api }));
    let store = models_store_face(controller.store.clone())?;
    let output = Object::new();
    set(&output, "store", &store)?;
    let loader = controller.clone();
    let load = Closure::wrap(Box::new(move || -> Promise {
        let controller = loader.clone();
        future_promise(async move {
            controller.load().await;
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut() -> Promise>);
    set(&output, "load", &load.into_js_value())?;
    Ok(output.into())
}

/// Creates the Rust-owned welcome acknowledgement controller and observable store face.
///
/// # Errors
///
/// Returns when the browser API value cannot be captured into the controller face.
#[wasm_bindgen(js_name = createWelcomeNoticeController)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_welcome_notice_controller(
    api: JsValue,
    persistence: String,
) -> Result<JsValue, JsValue> {
    let persistence = match persistence.as_str() {
        "host" => WelcomePersistence::Host,
        "memory" => WelcomePersistence::Memory,
        _ => {
            return Err(
                js_sys::TypeError::new("welcome persistence must be host or memory").into(),
            );
        }
    };
    let controller = WelcomeNoticeStore::new(Rc::new(BrowserWelcomeTransport { api }), persistence);
    let store = welcome_store_face(controller.store.clone())?;
    let output = Object::new();
    set(&output, "store", &store)?;
    let loader = controller.clone();
    let load = Closure::wrap(Box::new(move || -> Promise {
        let controller = loader.clone();
        future_promise(async move {
            controller.load().await;
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut() -> Promise>);
    set(&output, "load", &load.into_js_value())?;
    let acknowledger = controller;
    let acknowledge = Closure::wrap(Box::new(move || -> Promise {
        let controller = acknowledger.clone();
        future_promise(async move { Ok(JsValue::from_bool(controller.acknowledge().await)) })
    }) as Box<dyn FnMut() -> Promise>);
    set(&output, "acknowledge", &acknowledge.into_js_value())?;
    Ok(output.into())
}

/// Refetches a Models controller only after its first load.
///
/// # Errors
///
/// Returns for malformed controller/store faces or a synchronous load failure.
#[wasm_bindgen(js_name = refreshModelsIfLoaded)]
#[allow(clippy::needless_pass_by_value)]
pub fn refresh_models_if_loaded(controller: JsValue) -> Result<(), JsValue> {
    refresh_if_loaded(&controller)
}

/// Refetches a welcome controller only after its first load.
///
/// # Errors
///
/// Returns for malformed controller/store faces or a synchronous load failure.
#[wasm_bindgen(js_name = refreshWelcomeIfLoaded)]
#[allow(clippy::needless_pass_by_value)]
pub fn refresh_welcome_if_loaded(controller: JsValue) -> Result<(), JsValue> {
    refresh_if_loaded(&controller)
}

fn refresh_if_loaded(controller: &JsValue) -> Result<(), JsValue> {
    let store = required(controller, "store", "settings controller")?;
    let snapshot = call_method(&store, "getSnapshot", &[])?;
    if required(&snapshot, "status", "settings snapshot")?
        .as_string()
        .as_deref()
        != Some("idle")
    {
        call_method(controller, "load", &[])?;
    }
    Ok(())
}

fn models_store_face(store: Rc<SnapshotStore<ModelsSettingsState>>) -> Result<JsValue, JsValue> {
    snapshot_face(store, models_state_value)
}

fn welcome_store_face(store: Rc<SnapshotStore<WelcomeNoticeState>>) -> Result<JsValue, JsValue> {
    snapshot_face(store, welcome_state_value)
}

fn snapshot_face<T: Clone + 'static>(
    store: Rc<SnapshotStore<T>>,
    convert: fn(&T) -> Result<JsValue, JsValue>,
) -> Result<JsValue, JsValue> {
    let output = Object::new();
    let cache = Rc::new(RefCell::new(None::<(Rc<T>, JsValue)>));
    let getter_store = store.clone();
    let getter_cache = cache;
    let getter = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = getter_store.snapshot();
        if let Some((cached, value)) = getter_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = convert(&snapshot)?;
        *getter_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&output, "getSnapshot", &getter.into_js_value())?;
    let subscriber_store = store;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let callback = listener.clone();
        let subscription = subscriber_store.subscribe(Rc::new(move || {
            let _ = callback.call0(&JsValue::UNDEFINED);
        }));
        subscription_disposer(subscription)
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&output, "subscribe", &subscribe.into_js_value())?;
    Ok(output.into())
}

fn subscription_disposer<T: 'static>(subscription: SnapshotStoreSubscription<T>) -> Function {
    Closure::wrap(Box::new(move || subscription.dispose()) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn models_state_value(state: &ModelsSettingsState) -> Result<JsValue, JsValue> {
    let rows = Array::new();
    for row in &state.rows {
        rows.push(&provider_row_value(row)?);
    }
    let namespaces = Map::new();
    for (namespace, view) in &state.namespaces {
        namespaces.set(&JsValue::from_str(namespace), &to_js(view)?);
    }
    Ok(object(&[
        ("status", JsValue::from_str(models_status(state.status))),
        ("error", option_string(state.error.as_deref())),
        (
            "credentialError",
            option_string(state.credential_error.as_deref()),
        ),
        ("writable", JsValue::from_bool(state.writable)),
        ("rows", rows.into()),
        ("namespaces", namespaces.into()),
    ])?
    .into())
}

fn provider_row_value(row: &ProviderRow) -> Result<JsValue, JsValue> {
    Ok(object(&[
        ("entry", to_js(&row.entry)?),
        ("configured", JsValue::from_bool(row.configured)),
        ("removable", JsValue::from_bool(row.removable)),
        ("apiKeyEnv", option_undefined(row.api_key_env.as_deref())),
        (
            "credential",
            row.credential
                .as_ref()
                .map_or(Ok(JsValue::UNDEFINED), to_js)?,
        ),
    ])?
    .into())
}

fn welcome_state_value(state: &WelcomeNoticeState) -> Result<JsValue, JsValue> {
    Ok(object(&[
        ("status", JsValue::from_str(welcome_status(state.status))),
        ("acknowledged", JsValue::from_bool(state.acknowledged)),
        ("error", option_string(state.error.as_deref())),
    ])?
    .into())
}

fn rpc_value(response: &JsValue) -> Result<JsValue, String> {
    let result = required(response, "result", "RPC response").map_err(js_error)?;
    let ok = required(&result, "ok", "RPC result")
        .map_err(js_error)?
        .as_bool()
        .ok_or_else(|| "RPC result ok must be a boolean".to_owned())?;
    if ok {
        required(&result, "value", "successful RPC result").map_err(js_error)
    } else {
        let error = required(&result, "error", "failed RPC result").map_err(js_error)?;
        required(&error, "message", "RPC error")
            .map_err(js_error)?
            .as_string()
            .ok_or_else(|| "RPC error message must be a string".to_owned())
            .and_then(Err)
    }
}

fn from_js<T: DeserializeOwned>(value: &JsValue) -> Result<T, String> {
    let encoded = JSON::stringify(value)
        .map_err(js_error)?
        .as_string()
        .ok_or_else(|| "wire value is not JSON-compatible".to_owned())?;
    serde_json::from_str(&encoded).map_err(|error| error.to_string())
}

fn to_js(value: &impl serde::Serialize) -> Result<JsValue, JsValue> {
    JSON::parse(
        &serde_json::to_string(value)
            .map_err(|error| js_sys::TypeError::new(&error.to_string()))?,
    )
}

#[allow(clippy::needless_pass_by_value)] // `map_err` supplies owned JavaScript errors.
fn js_error(error: JsValue) -> String {
    rejection_text(&error)
}

fn option_string(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::NULL, JsValue::from_str)
}

fn option_undefined(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::UNDEFINED, JsValue::from_str)
}

const fn models_status(status: ModelsStatus) -> &'static str {
    match status {
        ModelsStatus::Idle => "idle",
        ModelsStatus::Loading => "loading",
        ModelsStatus::Ready => "ready",
        ModelsStatus::Error => "error",
    }
}

const fn welcome_status(status: WelcomeStatus) -> &'static str {
    match status {
        WelcomeStatus::Idle => "idle",
        WelcomeStatus::Loading => "loading",
        WelcomeStatus::Ready => "ready",
        WelcomeStatus::Saving => "saving",
        WelcomeStatus::Error => "error",
    }
}
