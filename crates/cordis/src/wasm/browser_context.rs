//! Context metadata operations and their optional native bindings.

use js_sys::{Array, Function, Object, Reflect, WeakMap};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

use super::{
    browser_registry::method, browser_symbols::get as symbol, browser_values as values, object,
    tracing::Tracer,
};

thread_local! {
    static EXTENSIONS: WeakMap = WeakMap::new();
}

pub(super) fn alias_extension(source: &JsValue, alias: &JsValue) {
    let fields = EXTENSIONS.with(|extensions| extensions.get(source.unchecked_ref::<Object>()));
    if !fields.is_undefined() {
        EXTENSIONS.with(|extensions| extensions.set(alias.unchecked_ref::<Object>(), &fields));
    }
}

/// Extends a Context or structural receiver using its own metadata descriptors.
///
/// # Errors
/// Propagates metadata, shadow, and prototype failures.
#[wasm_bindgen(js_name = contextExtend)]
pub fn extend(owner: &JsValue, metadata: &JsValue) -> Result<JsValue, JsValue> {
    let result = extend_metadata(owner, metadata)?;
    let native = super::context_core(owner, &JsValue::UNDEFINED)?;
    if !native.is_undefined() {
        method(&native, "bindContextMetadata", &Array::of1(&result))?;
    }
    Ok(result)
}

pub(super) fn extend_metadata(owner: &JsValue, metadata: &JsValue) -> Result<JsValue, JsValue> {
    let shadow_key = symbol("shadow")?;
    let descriptor =
        Reflect::get_own_property_descriptor(owner.unchecked_ref::<Object>(), &shadow_key)?;
    let shadow = if descriptor.is_undefined() {
        JsValue::UNDEFINED
    } else {
        values::get(&descriptor, &"value".into())?
    };
    let prototype = Tracer::new(owner.clone()).trace(owner)?;
    let result = Object::create(prototype.unchecked_ref::<Object>());
    let fields = Object::create(&Object::from(JsValue::NULL));
    let metadata = if metadata.is_undefined() {
        Object::new().into()
    } else {
        metadata.clone()
    };
    for key in Reflect::own_keys(&metadata)?.iter() {
        let descriptor =
            Reflect::get_own_property_descriptor(metadata.unchecked_ref::<Object>(), &key)?;
        Reflect::define_property(&result, &key, descriptor.unchecked_ref::<Object>())?;
        values::set(&fields, &key, &descriptor)?;
    }
    let result = if shadow.is_truthy() {
        let outer = Object::create(&result);
        values::set(&outer, &shadow_key, &shadow)?;
        outer
    } else {
        result
    };
    EXTENSIONS.with(|extensions| extensions.set(&result, &fields));
    Ok(result.into())
}

/// Creates an isolation map through the receiver's current extension method.
///
/// # Errors
/// Propagates scope, label, extension, and native binding failures.
#[wasm_bindgen(js_name = contextIsolate)]
pub fn isolate(owner: &JsValue, name: &JsValue, label: &JsValue) -> Result<JsValue, JsValue> {
    let inherited = values::get(owner, &symbol("isolate")?)?;
    let scopes = create(&inherited)?;
    let label = if label.is_null() || label.is_undefined() {
        let constructor =
            values::get(&js_sys::global(), &"Symbol".into())?.dyn_into::<Function>()?;
        Reflect::apply(&constructor, &JsValue::UNDEFINED, &Array::of1(name))?
    } else {
        label.clone()
    };
    values::set(&scopes, name, &label)?;
    let key = symbol("isolate")?;
    let metadata = object(&[])?;
    values::set(&metadata, &key, &scopes)?;
    let result = method(owner, "extend", &Array::of1(&metadata))?;
    if name.is_string() && contains_extension(&result, &key, &scopes)? {
        let native = super::context_core(owner, &JsValue::UNDEFINED)?;
        if !native.is_undefined() {
            method(
                &native,
                "bindContextIsolation",
                &Array::of3(&result, name, &label),
            )?;
        }
    }
    Ok(result)
}

/// Retains intercept config by identity and delegates metadata extension to the receiver.
///
/// # Errors
/// Propagates intercept-map and extension failures.
#[wasm_bindgen(js_name = contextIntercept)]
pub fn intercept(owner: &JsValue, name: &JsValue, config: &JsValue) -> Result<JsValue, JsValue> {
    let inherited = values::get(owner, &symbol("intercept")?)?;
    let intercepts = create(&inherited)?;
    values::set(&intercepts, name, config)?;
    let metadata = object(&[])?;
    values::set(&metadata, &symbol("intercept")?, &intercepts)?;
    method(owner, "extend", &Array::of1(&metadata))
}

fn create(prototype: &JsValue) -> Result<JsValue, JsValue> {
    method(
        &values::get(&js_sys::global(), &"Object".into())?,
        "create",
        &Array::of1(prototype),
    )
}

pub(super) fn contains_extension(
    result: &JsValue,
    key: &JsValue,
    value: &JsValue,
) -> Result<bool, JsValue> {
    if !result.is_object() && !result.is_function() {
        return Ok(false);
    }
    let fields = EXTENSIONS.with(|extensions| extensions.get(result.unchecked_ref::<Object>()));
    if fields.is_undefined() {
        return Ok(false);
    }
    let descriptor = values::get(&fields, key)?;
    if descriptor.is_undefined()
        || !Object::has_own(descriptor.unchecked_ref::<Object>(), &"value".into())
    {
        return Ok(false);
    }
    Ok(values::get(&descriptor, &"value".into())? == *value)
}
