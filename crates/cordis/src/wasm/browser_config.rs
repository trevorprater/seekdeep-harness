//! Browser plugin configuration retains JavaScript identity across update veto and activation.

use parking_lot::Mutex;
use std::cell::RefCell;
use std::sync::Arc;

use js_sys::{Array, JsString, Object, Reflect};
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
    let mut lines = Vec::new();
    for issue in issues.iter() {
        let message = String::from(JsString::from(Reflect::get(&issue, &"message".into())?));
        let path = Reflect::get(&issue, &"path".into())?;
        lines.push(if path.is_truthy() {
            let path = required_function(&path, "join")?.call1(&path, &".".into())?;
            format!("  - {message} (at {})", String::from(JsString::from(path)))
        } else {
            format!("  - {message}")
        });
    }
    Ok(format!("invalid config:\n{}", lines.join("\n")))
}

#[derive(Clone)]
pub(super) struct BrowserConfig {
    descriptor: JsValue,
    activation: FaceSlot,
    lifecycle: Arc<Mutex<Option<super::browser_fiber::BrowserLifecycle>>>,
}

impl BrowserConfig {
    pub(super) fn new(descriptor: JsValue) -> Self {
        Self {
            descriptor,
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

    pub(super) fn runtime_name(&self) -> Result<JsValue, JsValue> {
        Reflect::get(&self.descriptor, &"name".into())
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
    if Reflect::has(&result, &"then".into())? {
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
    error.set_name("ValidationError");
    Err(error.into())
}
