//! Browser plugin configuration retains JavaScript identity across update veto and activation.

use parking_lot::Mutex;
use std::cell::RefCell;
use std::sync::Arc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{FaceSlot, empty_face_slot, required_function};

thread_local! {
    static VALIDATION_ERROR_PROTOTYPE: RefCell<Option<Object>> = const { RefCell::new(None) };
}

/// Supplies the public JavaScript subclass prototype for compiled validation failures.
#[wasm_bindgen(js_name = configureValidationErrorPrototype)]
pub fn configure_validation_error_prototype(prototype: Object) {
    VALIDATION_ERROR_PROTOTYPE.with(|slot| *slot.borrow_mut() = Some(prototype));
}

/// Formats standard-schema issues with the source diagnostic and path rules.
///
/// # Errors
/// Propagates issue getter and path conversion failures.
#[wasm_bindgen(js_name = validationErrorMessage)]
pub fn validation_error_message(issues: &Array) -> Result<String, JsValue> {
    let format = Closure::wrap(Box::new(|issue: JsValue| {
        let has_path = super::browser_values::get(&issue, &"path".into())?.is_truthy();
        let message = super::browser_values::template_string(&super::browser_values::get(
            &issue,
            &"message".into(),
        )?)?;
        let line = if has_path {
            let path = super::browser_values::get(&issue, &"path".into())?;
            let path = super::browser_registry::method(&path, "join", &Array::of1(&".".into()))?;
            format!(
                "  - {message} (at {})",
                super::browser_values::template_string(&path)?,
            )
        } else {
            format!("  - {message}")
        };
        Ok(JsValue::from_str(&line))
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let lines = super::browser_registry::method(issues, "map", &Array::of1(&format))?;
    let joined = super::browser_registry::method(&lines, "join", &Array::of1(&"\n".into()))?;
    Function::new_with_args("value", "return 'invalid config:\\n' + value;")
        .call1(&JsValue::UNDEFINED, &joined)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("validation message is not a string").into())
}

#[derive(Clone)]
pub(super) struct BrowserConfig {
    activation: FaceSlot,
    lifecycle: Arc<Mutex<Option<super::browser_fiber::BrowserLifecycle>>>,
}

impl BrowserConfig {
    pub(super) fn new() -> Self {
        Self {
            activation: empty_face_slot(),
            lifecycle: Arc::default(),
        }
    }

    pub(super) fn activation_face(&self, fallback: &JsValue) -> JsValue {
        self.activation
            .lock()
            .take()
            .unwrap_or_else(|| fallback.clone())
    }

    pub(super) fn attach(&self, native: &Arc<super::PluginFiber>, core: FaceSlot) {
        let lifecycle =
            super::browser_fiber::BrowserLifecycle::new(native, core, self.activation.clone());
        super::browser_fiber_api::bind_lifecycle(&native.browser_face(), lifecycle.clone());
        if super::browser_fiber_api::prototype().is_none() {
            super::browser_fiber_api::install_root_methods(
                native.browser_face().unchecked_ref(),
                &lifecycle,
            )
            .expect("fresh raw Fiber methods are writable");
        }
        *self.lifecycle.lock() = Some(lifecycle.clone());
        let prepared = lifecycle.clone();
        let disposing = lifecycle.clone();
        let settling = lifecycle.clone();
        native
            .fiber()
            .observe_browser(crate::fiber::BrowserFiberObserver {
                changed: Arc::new(move |state| lifecycle.phase(state)),
                prepare: Arc::new(move |explicit| prepared.prepare(explicit)),
                dispose: Arc::new(move || disposing.dispose()),
                settled: Arc::new(move || settling.settled()),
            });
    }

    pub(super) fn initial(&self) -> Result<(), JsValue> {
        let lifecycle = self
            .lifecycle
            .lock()
            .clone()
            .expect("browser Fiber lifecycle attached");
        lifecycle.initial()
    }
}

pub(super) fn resolve_fiber_config(context: &JsValue, fiber: &JsValue) -> Result<JsValue, JsValue> {
    let raw = Reflect::get(fiber, &"_config".into())?;
    resolve_fiber_input(context, fiber, &raw)
}

pub(super) fn resolve_fiber_input(
    context: &JsValue,
    fiber: &JsValue,
    raw: &JsValue,
) -> Result<JsValue, JsValue> {
    let fallback = raw.clone();
    let next = Closure::wrap(Box::new(move || fallback.clone()) as Box<dyn Fn() -> JsValue>)
        .into_js_value();
    let config = required_function(context, "waterfall")?.apply(
        context,
        &Array::of4(fiber, &"internal/config".into(), raw, &next),
    )?;
    let runtime = Reflect::get(fiber, &"runtime".into())?;
    if !runtime.is_truthy() {
        return Ok(config);
    }
    resolve_config(&runtime, &config)
}

/// Validates a runtime config through its standard-schema validator.
///
/// # Errors
/// Propagates schema failures and rejects asynchronous validation with the source error.
#[wasm_bindgen(js_name = resolveConfig)]
pub fn resolve_config(runtime: &JsValue, config: &JsValue) -> Result<JsValue, JsValue> {
    if !super::browser_values::get(runtime, &"Config".into())?.is_truthy() {
        return Ok(config.clone());
    }
    let schema = super::browser_values::get(runtime, &"Config".into())?;
    let standard = Reflect::get(&schema, &"~standard".into())?;
    let result = super::browser_registry::method(&standard, "validate", &Array::of1(config))?;
    if Function::new_with_args("value", "return 'then' in value;")
        .call1(&JsValue::UNDEFINED, &result)?
        .is_truthy()
    {
        return Err(js_sys::TypeError::new("Async config validation is not supported").into());
    }
    let issues = Reflect::get(&result, &"issues".into())?;
    if !issues.is_truthy() {
        return Reflect::get(&result, &"value".into());
    }
    let issues = Reflect::get(&result, &"issues".into())?;
    let error = js_sys::TypeError::new(&validation_error_message(issues.unchecked_ref())?);
    VALIDATION_ERROR_PROTOTYPE.with(|slot| {
        if let Some(prototype) = slot.borrow().as_ref() {
            Object::set_prototype_of(error.unchecked_ref::<Object>(), prototype);
        }
    });
    super::browser_fiber::data_field(error.unchecked_ref(), "name", "ValidationError".into())?;
    Err(error.into())
}
