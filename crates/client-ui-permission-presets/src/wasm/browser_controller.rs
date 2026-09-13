//! JavaScript API/store adapter for the portable permission Settings controller.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use js_sys::{Array, Function, Object, Promise};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use super::{
    call_method, js_error_string, object, optional, required, required_bool, required_string, set,
};
use crate::{
    PermissionNamespaceView, PermissionPresetSettingsController, PermissionSettingsDescription,
    PermissionSettingsMutation, PermissionSettingsState, PermissionSettingsStatus,
    PermissionSettingsTransport, permission_default_of,
};

struct BrowserTransport {
    settings: JsValue,
}

impl PermissionSettingsTransport for BrowserTransport {
    fn describe(&self) -> LocalBoxFuture<'static, Result<PermissionSettingsDescription, String>> {
        let settings = self.settings.clone();
        async move {
            let response = await_method(&settings, "describe", &[Object::new().into()]).await?;
            parse_description(&result_value(&response)?)
        }
        .boxed_local()
    }

    fn mutate(
        &self,
        request: PermissionSettingsMutation,
    ) -> LocalBoxFuture<'static, Result<PermissionNamespaceView, String>> {
        let settings = self.settings.clone();
        async move {
            let path = Array::of1(&JsValue::from_str("defaultPreset"));
            let operation = object(&[
                ("op", JsValue::from_str("set")),
                ("path", path.into()),
                ("value", JsValue::from_str(&request.preset)),
            ])
            .map_err(|error| js_error_string(&error))?;
            let operations = Array::of1(&operation);
            let payload = object(&[
                ("ns", JsValue::from_str("permission")),
                ("ops", operations.into()),
                (
                    "expectedRevision",
                    JsValue::from_f64(u64_as_f64(request.expected_revision)),
                ),
            ])
            .map_err(|error| js_error_string(&error))?;
            let response = await_method(&settings, "mutate", &[payload.into()]).await?;
            parse_namespace(&result_value(&response)?)
        }
        .boxed_local()
    }
}

type SnapshotCache = Rc<RefCell<Option<(Rc<PermissionSettingsState>, JsValue)>>>;

/// Compiled permission default Settings controller.
#[wasm_bindgen(js_name = __PermissionPresetSettingsController)]
pub struct WasmPermissionPresetSettingsController {
    pub(crate) controller: Rc<PermissionPresetSettingsController>,
    pub(crate) store_face: JsValue,
}

#[wasm_bindgen(js_class = __PermissionPresetSettingsController)]
impl WasmPermissionPresetSettingsController {
    /// Creates an idle controller over the generated Settings API.
    ///
    /// # Errors
    ///
    /// Returns a malformed generated API face.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(api: JsValue) -> Result<WasmPermissionPresetSettingsController, JsValue> {
        Self::from_api(&api)
    }

    /// uSES-safe observable Store face.
    #[wasm_bindgen(getter)]
    pub fn store(&self) -> JsValue {
        self.store_face.clone()
    }

    /// Loads the permission descriptor.
    pub fn load(&self) -> Promise {
        operation_promise(self.controller.load())
    }

    /// Writes one advertised default preset.
    pub fn select(&self, preset: String) -> Promise {
        operation_promise(self.controller.select(preset))
    }

    /// Suppresses in-flight publication and releases the descriptor.
    pub fn dispose(&self) {
        self.controller.dispose();
    }
}

impl WasmPermissionPresetSettingsController {
    pub(crate) fn from_api(api: &JsValue) -> Result<Self, JsValue> {
        let settings = required(api, "settings", "generated API")?;
        let controller =
            PermissionPresetSettingsController::new(Rc::new(BrowserTransport { settings }));
        let store_face = store_face(&controller)?;
        Ok(Self {
            controller,
            store_face,
        })
    }
}

/// Refetches a controller only after its first load attempt.
#[wasm_bindgen(js_name = refreshPermissionIfLoaded)]
pub fn refresh_permission_if_loaded(controller: &WasmPermissionPresetSettingsController) {
    if let Some(future) = controller.controller.refresh_if_loaded() {
        spawn_local(future);
    }
}

/// Resolves one descriptor's dynamic permission default.
///
/// # Errors
///
/// Returns malformed descriptor or schema diagnostics.
#[wasm_bindgen(js_name = permissionDefaultOf)]
#[allow(clippy::needless_pass_by_value)]
pub fn permission_default_of_js(view: JsValue) -> Result<JsValue, JsValue> {
    let view = parse_namespace(&view).map_err(|message| js_sys::Error::new(&message))?;
    let resolved = permission_default_of(&view.schema, &view.value)
        .map_err(|message| js_sys::Error::new(&message))?;
    let options = Array::new();
    for option in resolved.options {
        let value: JsValue = object(&[
            ("id", JsValue::from_str(&option.id)),
            ("label", JsValue::from_str(&option.label)),
        ])?
        .into();
        options.push(&value);
    }
    object(&[
        ("currentValue", JsValue::from_str(&resolved.current_value)),
        ("options", options.into()),
    ])
    .map(Into::into)
}

fn store_face(controller: &Rc<PermissionPresetSettingsController>) -> Result<JsValue, JsValue> {
    let face = Object::new();
    let cache: SnapshotCache = Rc::new(RefCell::new(None));
    let get_controller = controller.clone();
    let get_cache = cache;
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = get_controller.snapshot();
        if let Some((cached, value)) = get_cache.borrow().as_ref()
            && Rc::ptr_eq(cached, &snapshot)
        {
            return Ok(value.clone());
        }
        let value = state_to_js(&snapshot)?;
        *get_cache.borrow_mut() = Some((snapshot, value.clone()));
        Ok(value)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "getSnapshot", &get_snapshot.into_js_value())?;

    let subscribe_controller = controller.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> JsValue {
        let subscription = subscribe_controller.subscribe(Rc::new(move || {
            let _ = listener.call0(&JsValue::UNDEFINED);
        }));
        let subscription = Rc::new(RefCell::new(Some(subscription)));
        Closure::wrap(Box::new(move || {
            if let Some(mut subscription) = subscription.borrow_mut().take() {
                subscription.dispose();
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&face, "subscribe", &subscribe.into_js_value())?;
    Ok(face.into())
}

fn state_to_js(state: &PermissionSettingsState) -> Result<JsValue, JsValue> {
    let options = Array::new();
    for option in &state.options {
        let value: JsValue = object(&[
            ("id", JsValue::from_str(&option.id)),
            ("label", JsValue::from_str(&option.label)),
        ])?
        .into();
        options.push(&value);
    }
    object(&[
        (
            "status",
            JsValue::from_str(match state.status {
                PermissionSettingsStatus::Idle => "idle",
                PermissionSettingsStatus::Loading => "loading",
                PermissionSettingsStatus::Ready => "ready",
                PermissionSettingsStatus::Saving => "saving",
                PermissionSettingsStatus::Unavailable => "unavailable",
                PermissionSettingsStatus::Error => "error",
            }),
        ),
        (
            "error",
            state
                .error
                .as_ref()
                .map_or(JsValue::NULL, |error| JsValue::from_str(error)),
        ),
        ("writable", JsValue::from_bool(state.writable)),
        ("currentValue", JsValue::from_str(&state.current_value)),
        ("options", options.into()),
        ("revision", JsValue::from_f64(u64_as_f64(state.revision))),
    ])
    .map(Into::into)
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, String> {
    let returned = call_method(value, name, arguments).map_err(|error| js_error_string(&error))?;
    JsFuture::from(Promise::resolve(&returned))
        .await
        .map_err(|error| js_error_string(&error))
}

fn result_value(response: &JsValue) -> Result<JsValue, String> {
    let result = required(response, "result", "Settings response")
        .map_err(|error| js_error_string(&error))?;
    if required_bool(&result, "ok", "Settings result").map_err(|error| js_error_string(&error))? {
        return required(&result, "value", "Settings result")
            .map_err(|error| js_error_string(&error));
    }
    let error =
        required(&result, "error", "Settings result").map_err(|error| js_error_string(&error))?;
    Err(required_string(&error, "message", "Settings error")
        .unwrap_or_else(|error| js_error_string(&error)))
}

fn parse_description(value: &JsValue) -> Result<PermissionSettingsDescription, String> {
    let writable = required_bool(value, "writable", "Settings description")
        .map_err(|error| js_error_string(&error))?;
    let namespaces = Array::from(
        &required(value, "namespaces", "Settings description")
            .map_err(|error| js_error_string(&error))?,
    )
    .iter()
    .map(|value| parse_namespace(&value))
    .collect::<Result<Vec<_>, _>>()?;
    Ok(PermissionSettingsDescription {
        writable,
        namespaces,
    })
}

fn parse_namespace(value: &JsValue) -> Result<PermissionNamespaceView, String> {
    let namespace = required_string(value, "ns", "Settings namespace")
        .map_err(|error| js_error_string(&error))?;
    let schema = serde_wasm_bindgen::from_value(
        required(value, "schema", "Settings namespace").map_err(|error| js_error_string(&error))?,
    )
    .map_err(|error| error.to_string())?;
    let current = optional(value, "value")
        .map_err(|error| js_error_string(&error))?
        .unwrap_or(JsValue::NULL);
    let current_value =
        serde_wasm_bindgen::from_value(current).map_err(|error| error.to_string())?;
    let revision = required(value, "revision", "Settings namespace")
        .map_err(|error| js_error_string(&error))?
        .as_f64()
        .ok_or_else(|| "Settings namespace revision must be numeric".to_owned())?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let revision = revision as u64;
    Ok(PermissionNamespaceView {
        namespace,
        schema,
        value: current_value,
        revision,
    })
}

fn operation_promise(future: LocalBoxFuture<'static, ()>) -> Promise {
    future_to_promise(async move {
        future.await;
        Ok(JsValue::UNDEFINED)
    })
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
