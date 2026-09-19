//! Receiver-owned reflection records and asynchronous withdrawal.

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{method, object, symbol, values};

pub(super) fn register(
    service: &JsValue,
    name: &JsValue,
    value: &JsValue,
    check: &JsValue,
) -> Result<JsValue, JsValue> {
    declare(service, name)?;
    let root = values::get(&values::get(service, &"ctx".into())?, &"root".into())?;
    let root_scopes = values::get(&root, &symbol("isolate")?)?;
    let label = values::get(&root_scopes, name)?;
    if label.is_null() || label.is_undefined() {
        values::set(&root_scopes, name, &scope_symbol(&root, name)?)?;
    }
    let context = values::get(service, &"ctx".into())?;
    let key = values::get(&values::get(&context, &symbol("isolate")?)?, name)?;
    let implementation = object(&[
        ("name", name.clone()),
        ("value", value.clone()),
        (
            "fiber",
            values::get(&values::get(service, &"ctx".into())?, &"fiber".into())?,
        ),
        ("check", check.clone()),
    ])?;
    let previous = values::get(&values::get(service, &"store".into())?, &key)?;
    if previous.is_truthy() {
        let previous = values::get(&values::get(service, &"store".into())?, &key)?;
        let owner = values::get(&previous, &"fiber".into())?;
        let label = values::template_string(&values::get(&owner, &"name".into())?)?;
        let name = values::template_string(name)?;
        return Err(js_sys::Error::new(&format!(
            "service \"{name}\" has been registered at <{label}>"
        ))
        .into());
    }
    values::set(
        &values::get(service, &"store".into())?,
        &key,
        &implementation,
    )?;
    values::set(&fiber_store(service)?, name, &implementation)?;
    let fiber = values::get(&values::get(service, &"ctx".into())?, &"fiber".into())?;
    if values::get(&fiber, &"state".into())?.as_f64() == Some(2.0) {
        method(service, "notify", &Array::of1(&Array::of1(name)))?;
    }
    let (service, name) = (service.clone(), name.clone());
    Ok(
        Closure::wrap(Box::new(move || match withdraw(&service, &name, &key) {
            Ok(promise) => promise,
            Err(error) => Promise::reject(&error),
        }) as Box<dyn Fn() -> Promise>)
        .into_js_value(),
    )
}

fn declare(service: &JsValue, name: &JsValue) -> Result<(), JsValue> {
    let definition = values::get(&values::get(service, &"props".into())?, name)?;
    if definition.is_truthy() {
        let definition = values::get(&values::get(service, &"props".into())?, name)?;
        if values::get(&definition, &"type".into())?
            .as_string()
            .as_deref()
            != Some("service")
        {
            let definition = values::get(&values::get(service, &"props".into())?, name)?;
            let kind = values::template_string(&values::get(&definition, &"type".into())?)?;
            let name = values::template_string(name)?;
            return Err(js_sys::Error::new(&format!(
                "property \"{name}\" is already declared as {kind}"
            ))
            .into());
        }
    } else {
        let props = values::get(service, &"props".into())?;
        let existing = values::get(&props, name)?;
        if existing.is_null() || existing.is_undefined() {
            values::set(&props, name, &object(&[("type", "service".into())])?.into())?;
        }
    }
    values::set(
        &values::get(service, &"props".into())?,
        name,
        &object(&[("type", "service".into())])?.into(),
    )
}

fn scope_symbol(root: &JsValue, name: &JsValue) -> Result<JsValue, JsValue> {
    let core = super::super::context_core(root, &JsValue::UNDEFINED)?;
    if name.is_string() && !core.is_undefined() {
        return method(&core, "serviceScope", &Array::of1(name));
    }
    let symbol = values::get(&js_sys::global(), &"Symbol".into())?.dyn_into::<Function>()?;
    Reflect::apply(&symbol, &JsValue::UNDEFINED, &Array::of1(name))
}

fn fiber_store(service: &JsValue) -> Result<JsValue, JsValue> {
    let context = values::get(service, &"ctx".into())?;
    let fiber = values::get(&context, &"fiber".into())?;
    values::get(&fiber, &"store".into())
}

fn withdraw(service: &JsValue, name: &JsValue, key: &JsValue) -> Result<Promise, JsValue> {
    let store = values::get(service, &"store".into())?;
    Reflect::delete_property(store.unchecked_ref::<Object>(), key)?;
    let fibers = method(service, "notify", &Array::of1(&Array::of1(name)))?;
    let wait = Closure::wrap(
        Box::new(move |fiber: JsValue| method(&fiber, "await", &Array::new()))
            as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    let pending = method(&fibers, "map", &Array::of1(&wait))?;
    let (service, name) = (service.clone(), name.clone());
    let finish = Closure::wrap(Box::new(move |_: JsValue| {
        Reflect::delete_property(fiber_store(&service)?.unchecked_ref::<Object>(), &name)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    method(
        &Promise::all_settled(pending.unchecked_ref()),
        "then",
        &Array::of1(&finish),
    )
    .map(wasm_bindgen::JsCast::unchecked_into)
}
