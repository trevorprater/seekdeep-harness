//! Source `DisposableList` storage: update hooks survive activation teardown until removed.

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Map, Object, Reflect, Symbol, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{object, set};

pub(super) fn disposable_list() -> Result<JsValue, JsValue> {
    let list = Object::new();
    let entries = Map::new();
    let latest = WeakMap::new();
    let serial = Rc::new(Cell::new(0.0));
    let pushed = entries.clone();
    let indexed = latest.clone();
    let push = Closure::wrap(Box::new(move |value: JsValue| {
        let id = serial.get() + 1.0;
        serial.set(id);
        let id = JsValue::from_f64(id);
        pushed.set(&id, &value);
        indexed.set(value.unchecked_ref::<Object>(), &id);
        let entries = pushed.clone();
        Closure::wrap(Box::new(move || entries.delete(&id)) as Box<dyn Fn() -> bool>)
            .into_js_value()
    }) as Box<dyn Fn(JsValue) -> JsValue>)
    .into_js_value();
    set(&list, "push", &push)?;
    let deleted = entries.clone();
    let delete = Closure::wrap(Box::new(move |value: JsValue| {
        let id = latest.get(value.unchecked_ref::<Object>());
        if id.is_undefined() {
            JsValue::UNDEFINED
        } else {
            JsValue::from_bool(deleted.delete(&id))
        }
    }) as Box<dyn Fn(JsValue) -> JsValue>)
    .into_js_value();
    set(&list, "delete", &delete)?;
    let cleared = entries.clone();
    let clear = Closure::wrap(Box::new(move || {
        let values = Array::from(&JsValue::from(cleared.values()));
        cleared.clear();
        values.reverse()
    }) as Box<dyn Fn() -> Array>)
    .into_js_value();
    set(&list, "clear", &clear)?;
    let sized = entries.clone();
    let length =
        Closure::wrap(Box::new(move || sized.size()) as Box<dyn Fn() -> u32>).into_js_value();
    Reflect::define_property(&list, &"length".into(), &object(&[("get", length)])?)?;
    let values = Closure::wrap(
        Box::new(move || JsValue::from(entries.values())) as Box<dyn Fn() -> JsValue>
    )
    .into_js_value();
    Reflect::set(&list, &Symbol::iterator(), &values)?;
    Ok(list.into())
}
