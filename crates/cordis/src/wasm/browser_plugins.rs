//! Class plugin construction and initialization use the source's callback classification.

use std::cell::RefCell;

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

thread_local! {
    static GENERATORS: RefCell<Option<Array>> = const { RefCell::new(None) };
}

pub(super) fn initialize_constructors() -> Result<(), JsValue> {
    if GENERATORS.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    let constructors = Function::new_no_args(
        "return [(function*() {}).constructor, (async function*() {}).constructor];",
    )
    .call0(&JsValue::UNDEFINED)?
    .dyn_into::<Array>()?;
    GENERATORS.with(|slot| *slot.borrow_mut() = Some(constructors));
    Ok(())
}

/// Tests the source's plugin constructor rule, including generator exclusions.
///
/// # Errors
/// Propagates prototype and instance-check failures.
#[wasm_bindgen(js_name = isConstructor)]
pub fn is_constructor(callback: &JsValue) -> Result<bool, JsValue> {
    initialize_constructors()?;
    if !super::browser_values::get(callback, &"prototype".into())?.is_truthy() {
        return Ok(false);
    }
    let constructors = GENERATORS
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| js_sys::Error::new("generator constructors are not initialized"))?;
    let instance_of =
        Function::new_with_args("value,constructor", "return value instanceof constructor;");
    if instance_of
        .call2(&JsValue::UNDEFINED, callback, &constructors.get(0))?
        .is_truthy()
    {
        return Ok(false);
    }
    if constructors.get(1) != Reflect::get(&js_sys::global(), &"Function".into())?
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
        super::browser_values::for_each(&hooks, |hook| {
            composition
                .call(&hook, &JsValue::UNDEFINED, &Array::new())
                .map(|_| ())
        })?;
    }
    let init = Reflect::get(&instance, &super::browser_symbols::get("init")?)?;
    if init.is_null() || init.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    composition.call(&init, &instance, &Array::new())
}
