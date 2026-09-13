//! Compiled queue projection over one resident Session face.

use js_sys::{Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

/// Projects a Session's queue as a bare observable without copying it.
///
/// # Errors
///
/// Returns if the JavaScript observable face cannot be populated. Missing
/// Session methods are reported when the projected closures are invoked.
#[wasm_bindgen(js_name = queueReadFaceOf)]
#[allow(clippy::needless_pass_by_value)]
pub fn queue_read_face_of_browser(session: JsValue) -> Result<JsValue, JsValue> {
    let snapshot_session = session.clone();
    let get_snapshot = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let snapshot = required_function(&snapshot_session, "getSnapshot", "Session face")?
            .call0(&snapshot_session)?;
        Reflect::get(&snapshot, &JsValue::from_str("queue"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value();
    let subscribe_session = session;
    let subscribe = Closure::wrap(
        Box::new(move |listener: JsValue| -> Result<JsValue, JsValue> {
            required_function(&subscribe_session, "subscribe", "Session face")?
                .call1(&subscribe_session, &listener)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    let face = Object::new();
    Reflect::set(&face, &JsValue::from_str("getSnapshot"), &get_snapshot)?;
    Reflect::set(&face, &JsValue::from_str("subscribe"), &subscribe)?;
    Ok(face.into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        property.dyn_into()
    }
}
