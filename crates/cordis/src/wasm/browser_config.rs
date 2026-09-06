//! Browser plugin configuration retains JavaScript identity across update veto and activation.

use std::cell::RefCell;

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
    failure: FaceSlot,
}

impl BrowserConfig {
    pub(super) fn new(descriptor: JsValue) -> Self {
        Self {
            descriptor,
            activation: empty_face_slot(),
            failure: empty_face_slot(),
        }
    }

    pub(super) fn activate_as(&self, face: JsValue) {
        *self.activation.lock() = Some(face);
    }

    pub(super) fn activation_face(&self, fallback: &JsValue) -> JsValue {
        self.activation
            .lock()
            .take()
            .unwrap_or_else(|| fallback.clone())
    }

    pub(super) fn retain_failure(&self, error: &JsValue) {
        *self.failure.lock() = Some(error.clone());
    }

    pub(super) fn clear_failure(&self) {
        *self.failure.lock() = None;
    }

    pub(super) fn lifecycle_error(&self, error: anyhow::Error) -> JsValue {
        self.failure
            .lock()
            .clone()
            .unwrap_or_else(|| super::js_error(error))
    }

    pub(super) fn resolve(&self, context: &JsValue, fiber: &JsValue) -> Result<JsValue, JsValue> {
        let raw = Reflect::get(fiber, &"_config".into())?;
        let fallback = raw.clone();
        let next = Closure::wrap(Box::new(move || fallback.clone()) as Box<dyn Fn() -> JsValue>)
            .into_js_value();
        let config = required_function(context, "waterfall")?.apply(
            context,
            &Array::of4(fiber, &"internal/config".into(), &raw, &next),
        )?;
        let schema = Reflect::get(&self.descriptor, &"Config".into())?;
        if !schema.is_truthy() {
            return Ok(config);
        }
        let standard = Reflect::get(&schema, &"~standard".into())?;
        let result = required_function(&standard, "validate")?.call1(&standard, &config)?;
        if Reflect::has(&result, &"then".into())? {
            return Err(js_sys::TypeError::new("Async config validation is not supported").into());
        }
        let issues = Reflect::get(&result, &"issues".into())?;
        if !issues.is_truthy() {
            return Reflect::get(&result, &"value".into());
        }
        let error = js_sys::TypeError::new(&validation_error_message(issues.unchecked_ref())?);
        VALIDATION_ERROR_PROTOTYPE.with(|slot| {
            if let Some(prototype) = slot.borrow().as_ref() {
                Object::set_prototype_of(error.unchecked_ref::<Object>(), prototype);
            }
        });
        error.set_name("ValidationError");
        Err(error.into())
    }
}
