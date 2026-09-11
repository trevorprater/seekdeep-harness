//! JavaScript property and prototype operations shared by browser Cordis bindings.

use js_sys::{Array, Function, Object, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

pub(super) fn template_string(value: &JsValue) -> Result<String, JsValue> {
    Function::new_with_args("value", "return `${value}`;")
        .call1(&JsValue::UNDEFINED, value)
        .map(|value| value.as_string().unwrap_or_default())
}

pub(super) fn get(value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Reflect::get(value, key);
    }
    super::get_with_receiver(super::boxed_object(value).as_ref(), key, value)
}

pub(super) fn for_each(
    value: &JsValue,
    mut body: impl FnMut(JsValue) -> Result<(), JsValue>,
) -> Result<(), JsValue> {
    let iterator = get(value, &Symbol::iterator())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("value is not iterable"))?;
    let iterator = Reflect::apply(&iterator, value, &Array::new())?;
    let next = get(&iterator, &"next".into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("iterator.next is not a function"))?;
    loop {
        let record = Reflect::apply(&next, &iterator, &Array::new())?;
        if !record.is_object() && !record.is_function() {
            return Err(js_sys::TypeError::new("iterator result is not an object").into());
        }
        if get(&record, &"done".into())?.is_truthy() {
            return Ok(());
        }
        let value = get(&record, &"value".into())?;
        if let Err(error) = body(value) {
            // IteratorClose preserves the original throw even if return also fails.
            let _ = super::browser_registry::method(&iterator, "return", &Array::new());
            return Err(error);
        }
    }
}

pub(super) fn set(value: &JsValue, key: &JsValue, field: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(value, key, field)? {
        Ok(())
    } else {
        Err(js_sys::TypeError::new("Cannot assign to read only property").into())
    }
}

pub(super) fn assign(target: &JsValue, sources: &Array) -> Result<JsValue, JsValue> {
    let object = Reflect::get(&js_sys::global(), &"Object".into())?;
    let args = Array::of1(target);
    for source in sources.iter() {
        args.push(&source);
    }
    super::browser_registry::method(&object, "assign", &args)
}

pub(super) fn define(value: &JsValue, key: &JsValue, field: &JsValue) -> Result<(), JsValue> {
    let descriptor = super::object(&[
        ("value", field.clone()),
        ("writable", true.into()),
        ("enumerable", false.into()),
    ])?;
    if Reflect::define_property(value.unchecked_ref::<Object>(), key, &descriptor)? {
        Ok(())
    } else {
        Err(js_sys::TypeError::new("Cannot define property").into())
    }
}

/// Returns the source object predicate, retaining falsy primitive results.
#[wasm_bindgen(js_name = isObject)]
pub fn is_object(value: &JsValue) -> JsValue {
    if value.is_truthy() {
        JsValue::from_bool(value.is_object() || value.is_function())
    } else {
        value.clone()
    }
}

/// Merges a prototype chain onto another while retaining own property descriptors.
///
/// # Errors
/// Propagates prototype, descriptor, and definition failures.
#[wasm_bindgen(js_name = joinPrototype)]
pub fn join_prototype(first: &JsValue, second: &JsValue) -> Result<JsValue, JsValue> {
    let object = Reflect::get(&js_sys::global(), &"Object".into())?;
    if *first == Reflect::get(&object, &"prototype".into())? {
        return Ok(second.clone());
    }
    let parent = super::browser_registry::method(&object, "getPrototypeOf", &Array::of1(first))?;
    let parent = join_prototype(&parent, second)?;
    let result = Object::create(parent.unchecked_ref::<Object>());
    for key in Reflect::own_keys(first)?.iter() {
        let descriptor =
            Reflect::get_own_property_descriptor(first.unchecked_ref::<Object>(), &key)?;
        Reflect::define_property(&result, &key, descriptor.unchecked_ref::<Object>())?;
    }
    Ok(result.into())
}

/// Looks up a property descriptor through the source's prototype walk.
///
/// # Errors
/// Propagates key conversion and prototype failures.
#[wasm_bindgen(js_name = getPropertyDescriptor)]
pub fn property_descriptor(target: &JsValue, key: &JsValue) -> Result<JsValue, JsValue> {
    let object = Reflect::get(&js_sys::global(), &"Object".into())?;
    let mut target = target.clone();
    while target.is_truthy() {
        let descriptor = super::browser_registry::method(
            &object,
            "getOwnPropertyDescriptor",
            &Array::of2(&target, key),
        )?;
        if descriptor.is_truthy() {
            return Ok(descriptor);
        }
        target = super::browser_registry::method(&object, "getPrototypeOf", &Array::of1(&target))?;
    }
    Ok(JsValue::UNDEFINED)
}

pub(super) fn proxy(target: &JsValue, handler: &Object) -> Result<JsValue, JsValue> {
    let proxy = Reflect::get(&js_sys::global(), &"Proxy".into())?.dyn_into::<Function>()?;
    Reflect::construct(&proxy, &Array::of2(target, handler))
}
