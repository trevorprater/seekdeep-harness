//! Class plugin construction and initialization use the source's callback classification.

use js_sys::{Array, Function, Reflect, Symbol};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

/// Tests the source's plugin constructor rule, including generator exclusions.
///
/// # Errors
/// Propagates prototype and instance-check failures.
#[wasm_bindgen(js_name = isConstructor)]
pub fn is_constructor(callback: &JsValue) -> Result<bool, JsValue> {
    if !Reflect::get(callback, &"prototype".into())?.is_truthy() {
        return Ok(false);
    }
    let constructors = Function::new_no_args(
        "return [(function*() {}).constructor, (async function*() {}).constructor, Function];",
    )
    .call0(&JsValue::UNDEFINED)?
    .dyn_into::<Array>()?;
    let instance_of =
        Function::new_with_args("value,constructor", "return value instanceof constructor;");
    if instance_of
        .call2(&JsValue::UNDEFINED, callback, &constructors.get(0))?
        .is_truthy()
    {
        return Ok(false);
    }
    if constructors.get(1) != constructors.get(2)
        && instance_of
            .call2(&JsValue::UNDEFINED, callback, &constructors.get(1))?
            .is_truthy()
    {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn invoke(
    runtime: &JsValue,
    context: &JsValue,
    config: &JsValue,
    composition: &super::browser_stack::Composition,
) -> Result<JsValue, JsValue> {
    let callback = Reflect::get(runtime, &"callback".into())?;
    if !is_constructor(&callback)? {
        return composition.call(
            &Reflect::get(runtime, &"callback".into())?,
            runtime,
            &Array::of2(context, config),
        );
    }
    let callback = Reflect::get(runtime, &"callback".into())?.dyn_into::<Function>()?;
    let instance = composition.construct(&callback, &Array::of2(context, config))?;
    let hooks = Reflect::get(&instance, &super::browser_symbols::get("initHooks")?)?;
    if !hooks.is_null() && !hooks.is_undefined() {
        let iterator = Reflect::get(&hooks, &Symbol::iterator())?.dyn_into::<Function>()?;
        let iterator = Reflect::apply(&iterator, &hooks, &Array::new())?;
        loop {
            let result = super::browser_registry::method(&iterator, "next", &Array::new())?;
            if !result.is_object() && !result.is_function() {
                return Err(js_sys::TypeError::new("iterator result is not an object").into());
            }
            if Reflect::get(&result, &"done".into())?.is_truthy() {
                break;
            }
            let hook = Reflect::get(&result, &"value".into())?;
            let called = composition.call(&hook, &JsValue::UNDEFINED, &Array::new());
            if let Err(error) = called {
                if let Ok(close) = Reflect::get(&iterator, &"return".into())
                    && let Some(close) = close.dyn_ref::<Function>()
                {
                    let _ = Reflect::apply(close, &iterator, &Array::new());
                }
                return Err(error);
            }
        }
    }
    let init = Reflect::get(&instance, &super::browser_symbols::get("init")?)?;
    if init.is_null() || init.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    composition.call(&init, &instance, &Array::new())
}
