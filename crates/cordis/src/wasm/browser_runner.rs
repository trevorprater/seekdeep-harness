//! Source Fiber execution and state methods over their current JavaScript receiver.

use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_registry::method, browser_stack::Composition, browser_values as values};

pub(super) const INACTIVE: &str = "__INACTIVE__";

pub(super) fn runner(owner: &JsValue) -> Result<JsValue, JsValue> {
    values::get(owner, &"_runner".into())
}

pub(super) fn epoch(owner: &JsValue) -> Result<JsValue, JsValue> {
    values::get(&runner(owner)?, &"epoch".into())
}

pub(super) fn is_active(epoch: &JsValue) -> bool {
    epoch.as_string().as_deref() != Some(INACTIVE)
}

/// Recomputes dependency epochs through the receiver's current setter.
///
/// # Errors
/// Propagates dependency snapshot and epoch-setter failures.
#[wasm_bindgen(js_name = fiberRefresh)]
pub fn refresh(owner: &JsValue) -> Result<(), JsValue> {
    super::browser_fiber::BrowserLifecycle::refresh(owner)
}

/// Resolves explicit raw config through the receiver's context and runtime schema.
///
/// # Errors
/// Propagates config hooks and schema validation failures.
#[wasm_bindgen(js_name = fiberResolveConfig)]
pub fn resolve_config(owner: &JsValue, config: &JsValue) -> Result<JsValue, JsValue> {
    super::browser_config::resolve_fiber_input(
        &values::get(owner, &"context".into())?,
        owner,
        config,
    )
}

pub(super) fn install(owner: &JsValue, runtime: &JsValue, stack: &JsValue) -> Result<(), JsValue> {
    let runtime_owner = runtime.clone();
    let invoke = Closure::wrap(Box::new(move |owner: JsValue| {
        let composition = match Composition::current() {
            Some(composition) => composition,
            None => Composition::new(values::get(&runner(&owner)?, &"getOuterStack".into())?)?,
        };
        super::browser_plugins::invoke(
            &runtime_owner,
            &values::get(&owner, &"ctx".into())?,
            &values::get(&owner, &"config".into())?,
            &composition,
        )
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let execute = if runtime.is_truthy() {
        Function::new_with_args(
            "invoke",
            "return function execute() { return invoke(this); };",
        )
        .call1(&JsValue::UNDEFINED, &invoke)?
    } else {
        Function::new_no_args("return () => {};").call0(&JsValue::UNDEFINED)?
    };
    let collected = owner.clone();
    let collect = Closure::wrap(Box::new(move |dispose: JsValue| {
        method(
            &values::get(&collected, &"_disposables".into())?,
            "push",
            &Array::of1(&dispose),
        )?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let collect = Function::new_with_args(
        "invoke",
        "return { collect: dispose => invoke(dispose) }.collect;",
    )
    .call1(&JsValue::UNDEFINED, &collect)?;
    let runner = super::object(&[
        (
            "epoch",
            if runtime.is_truthy() {
                INACTIVE.into()
            } else {
                "".into()
            },
        ),
        ("getOuterStack", stack.clone()),
        ("execute", execute),
        ("collect", collect),
    ])?;
    super::browser_fiber::data_field(owner.unchecked_ref::<Object>(), "_runner", runner.into())
}

/// Executes a runner using its current callbacks and captured epoch.
///
/// # Errors
/// Propagates execution, collection, iteration, and stack-composition failures.
#[wasm_bindgen(js_name = fiberExecute)]
pub fn execute(owner: &JsValue, runner: &JsValue) -> Result<JsValue, JsValue> {
    let epoch = values::get(runner, &"epoch".into())?;
    let composition = Composition::new(values::get(runner, &"getOuterStack".into())?)?;
    composition.run(|| {
        let returned = composition.method(
            &values::get(runner, &"execute".into())?,
            &"call".into(),
            &Array::of1(owner),
        )?;
        let collector = runner.clone();
        let collect = Closure::wrap(Box::new(move |value: JsValue| {
            method(&collector, "collect", &Array::of1(&value))
        })
            as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let runner = runner.clone();
        super::browser_effects::execute_result(
            &returned,
            collect,
            Rc::new(move || values::get(&runner, &"epoch".into()).map(|current| current == epoch)),
            composition.clone(),
        )
    })
}

/// Derives a settled state from uid, failure, and the runner's inactive sentinel.
///
/// # Errors
/// Propagates state-field getter failures.
#[wasm_bindgen(js_name = fiberGetState)]
pub fn get_state(owner: &JsValue) -> Result<u8, JsValue> {
    if values::get(owner, &"uid".into())?.is_null() {
        return Ok(4);
    }
    if values::get(owner, &"_error".into())?.is_truthy() {
        return Ok(3);
    }
    let epoch = values::get(&values::get(owner, &"_runner".into())?, &"epoch".into())?;
    Ok(if epoch.as_string().as_deref() == Some(INACTIVE) {
        0
    } else {
        2
    })
}

/// Applies the source state callback and publishes changes through the receiver.
///
/// # Errors
/// Propagates callback, state assignment, event, and notification failures.
#[wasm_bindgen(js_name = fiberUpdateState)]
pub fn update_state(owner: &JsValue, callback: &JsValue) -> Result<(), JsValue> {
    let previous = values::get(owner, &"state".into())?;
    let callback = callback
        .clone()
        .dyn_into::<js_sys::Function>()
        .map_err(|_| js_sys::TypeError::new("callback is not a function"))?;
    let state = Reflect::apply(&callback, &JsValue::UNDEFINED, &Array::new())?;
    let state = if state.is_null() || state.is_undefined() {
        method(owner, "_getState", &Array::new())?
    } else {
        state
    };
    values::set(owner, &"state".into(), &state)?;
    let current = values::get(owner, &"state".into())?;
    super::browser_fiber_api::synchronize_state(owner, &current)?;
    if previous == current {
        return Ok(());
    }
    method(
        &values::get(owner, &"context".into())?,
        "emit",
        &Array::of3(&"internal/status".into(), owner, &previous),
    )?;
    if previous.as_f64() != Some(2.0) && values::get(owner, &"state".into())?.as_f64() != Some(2.0)
    {
        return Ok(());
    }
    let context = values::get(owner, &"ctx".into())?;
    let reflection = values::get(&context, &"reflect".into())?;
    let keys = Reflect::own_keys(&values::get(&reflection, &"store".into())?)?;
    for key in keys.iter() {
        let context = values::get(owner, &"ctx".into())?;
        let reflection = values::get(&context, &"reflect".into())?;
        let implementation = values::get(&values::get(&reflection, &"store".into())?, &key)?;
        if values::get(&implementation, &"fiber".into())? != *owner {
            continue;
        }
        let reflection = values::get(&values::get(owner, &"ctx".into())?, &"reflect".into())?;
        method(
            &reflection,
            "notify",
            &Array::of1(&Array::of1(&values::get(&implementation, &"name".into())?)),
        )?;
    }
    Ok(())
}

use wasm_bindgen::JsCast as _;
