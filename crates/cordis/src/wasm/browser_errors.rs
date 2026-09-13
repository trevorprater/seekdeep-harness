//! Stable Cordis error codes use the public JavaScript constructor and mutable code table.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

thread_local! {
    static CONSTRUCTOR: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Supplies the public constructor used for errors raised by compiled browser methods.
#[wasm_bindgen(js_name = configureCordisErrorConstructor)]
pub fn configure_cordis_error_constructor(constructor: Function) {
    CONSTRUCTOR.with(|slot| *slot.borrow_mut() = Some(constructor));
}

/// Creates the source's mutable framework error-code table.
///
/// # Errors
/// Returns property construction failures.
#[wasm_bindgen(js_name = cordisErrorCodes)]
pub fn cordis_error_codes() -> Result<Object, JsValue> {
    super::object(&[(
        "INACTIVE_EFFECT",
        crate::CordisError::InactiveEffect.to_string().into(),
    )])
}

/// Resolves the default message without converting unknown codes into messages.
///
/// # Errors
/// Propagates code conversion and table getter failures unchanged.
#[wasm_bindgen(js_name = cordisErrorMessage)]
pub fn cordis_error_message(
    codes: &JsValue,
    code: &JsValue,
    message: &JsValue,
) -> Result<JsValue, JsValue> {
    if message.is_null() || message.is_undefined() {
        Reflect::get(codes, code)
    } else {
        Ok(message.clone())
    }
}

pub(super) fn inactive() -> JsValue {
    let constructor = CONSTRUCTOR.with(|slot| slot.borrow().clone());
    if let Some(constructor) = constructor {
        return Reflect::construct(&constructor, &Array::of1(&"INACTIVE_EFFECT".into()))
            .unwrap_or_else(|error| error);
    }
    let error = js_sys::Error::new(&crate::CordisError::InactiveEffect.to_string());
    if let Err(error) = Reflect::set(&error, &"code".into(), &"INACTIVE_EFFECT".into()) {
        return error;
    }
    error.into()
}
