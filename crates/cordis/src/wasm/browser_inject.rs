//! Class dependency metadata and dependency-owned method initialization.

use js_sys::{Array, Function, Object};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_registry::method, browser_values as values};

use super::browser_symbols::get as symbol;

/// Creates a class or method decorator retaining the supplied dependency config.
///
/// # Errors
/// Propagates wrapper construction failures.
#[wasm_bindgen(js_name = injectDecorator)]
pub fn inject_decorator(name: JsValue, config: JsValue) -> Result<Function, JsValue> {
    let invoke = Closure::wrap(Box::new(move |value: JsValue, decorator: JsValue| {
        if values::get(&decorator, &"kind".into())?
            .as_string()
            .as_deref()
            == Some("class")
        {
            decorate_class(&value, &name, &config)
        } else if values::get(&decorator, &"kind".into())?
            .as_string()
            .as_deref()
            == Some("method")
        {
            decorate_method(&value, &decorator, &name, &config)
        } else {
            Err(js_sys::Error::new("@Inject() can only be used on class or class methods").into())
        }
    })
        as Box<dyn Fn(JsValue, JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    Function::new_with_args(
        "invoke",
        "return function(value,decorator) { return invoke(value,decorator); };",
    )
    .call1(&JsValue::UNDEFINED, &invoke)?
    .dyn_into()
}

fn decorate_class(value: &JsValue, name: &JsValue, config: &JsValue) -> Result<(), JsValue> {
    if !Object::has_own(value.unchecked_ref::<Object>(), &"inject".into()) {
        let parent = Object::get_prototype_of(value.unchecked_ref::<Object>());
        let inherited = values::get(&parent, &"inject".into())?;
        let inherited = if inherited.is_null() || inherited.is_undefined() {
            JsValue::NULL
        } else {
            inherited
        };
        let inject = Object::create(inherited.unchecked_ref::<Object>());
        values::define(value, &"inject".into(), &inject)?;
        let inject = values::get(value, &"inject".into())?;
        values::define(&inject, &symbol("checkProto")?, &JsValue::TRUE)?;
    }
    values::set(&values::get(value, &"inject".into())?, name, config)
}

fn default_field(
    owner: &JsValue,
    key: &JsValue,
    create: impl FnOnce() -> JsValue,
) -> Result<JsValue, JsValue> {
    let value = values::get(owner, key)?;
    if !value.is_null() && !value.is_undefined() {
        return Ok(value);
    }
    let value = create();
    values::set(owner, key, &value)?;
    Ok(value)
}

fn decorate_method(
    value: &JsValue,
    decorator: &JsValue,
    name: &JsValue,
    config: &JsValue,
) -> Result<(), JsValue> {
    let metadata = default_field(value, &symbol("metadata")?, || Object::new().into())?;
    let inject = default_field(&metadata, &"inject".into(), || {
        Object::create(&Object::from(JsValue::NULL)).into()
    })?;
    values::set(&inject, name, config)?;
    let value = value.clone();
    let initializer = Closure::wrap(Box::new(move |instance: JsValue| {
        initialize_method(instance, value.clone(), inject.clone())
    }) as Box<dyn Fn(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let initializer =
        Function::new_with_args("invoke", "return function() { return invoke(this); };")
            .call1(&JsValue::UNDEFINED, &initializer)?;
    method(decorator, "addInitializer", &Array::of1(&initializer))?;
    Ok(())
}

fn initialize_method(instance: JsValue, value: JsValue, inject: JsValue) -> Result<(), JsValue> {
    let tracker = values::get(&instance, &symbol("tracker")?)?;
    let property = if tracker.is_null() || tracker.is_undefined() {
        JsValue::UNDEFINED
    } else {
        values::get(&tracker, &"property".into())?
    };
    let hooks = default_field(&instance, &symbol("initHooks")?, || Array::new().into())?;
    let hook = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let (owner, value, property) = (instance.clone(), value.clone(), property.clone());
        let callback = Closure::wrap(Box::new(move |context: JsValue| {
            let receiver = if property.is_truthy() {
                let props = Object::new();
                js_sys::Reflect::define_property(
                    &props,
                    &property,
                    &super::object(&[
                        ("value", context),
                        ("writable", true.into()),
                        ("enumerable", true.into()),
                        ("configurable", true.into()),
                    ])?,
                )?;
                super::tracing::with_props(&owner, &props)?
            } else {
                owner.clone()
            };
            method(&value, "call", &Array::of1(&receiver))
        })
            as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let callback = Function::new_with_args("invoke", "return ctx => invoke(ctx);")
            .call1(&JsValue::UNDEFINED, &callback)?;
        let context = values::get(&instance, &"ctx".into())?;
        method(&context, "inject", &Array::of2(&inject, &callback))?;
        Ok(())
    }) as Box<dyn Fn() -> Result<(), JsValue>>)
    .into_js_value();
    let hook = Function::new_with_args("invoke", "return () => invoke();")
        .call1(&JsValue::UNDEFINED, &hook)?;
    method(&hooks, "push", &Array::of1(&hook))?;
    Ok(())
}
