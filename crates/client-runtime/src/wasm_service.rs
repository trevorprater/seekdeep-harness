//! Cordis caller tracing for Rust-owned registry service methods.

use js_sys::{Function, Object, Reflect, Symbol};
use wasm_bindgen::JsValue;

pub(crate) fn caller_face(caller: &JsValue, name: &str) -> Result<Object, JsValue> {
    let face = Object::new();
    Reflect::set(&face, &"ctx".into(), caller)?;
    Reflect::set(&face, &"name".into(), &name.into())?;
    let tracker = Object::new();
    Reflect::set(&tracker, &"associate".into(), &name.into())?;
    Reflect::set(&tracker, &"property".into(), &"ctx".into())?;
    let descriptor = Object::new();
    Reflect::set(&descriptor, &"value".into(), &tracker)?;
    Reflect::define_property(&face, &Symbol::for_("cordis.tracker"), &descriptor)?;
    Ok(face)
}

pub(crate) fn caller_method(invoke: &JsValue) -> Result<JsValue, JsValue> {
    Function::new_with_args(
        "invoke",
        "'use strict'; return function(...args) { return invoke(this.ctx, ...args); };",
    )
    .call1(&JsValue::UNDEFINED, invoke)
}
