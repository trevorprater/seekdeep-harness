//! Context service lookup and assignment with the caller's diagnostic carrier.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::{browser_symbols::get as symbol, browser_values as values, tracing::Tracer};

fn method(receiver: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let callback = values::get(receiver, &name.into())?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{name} is not a function")))?;
    Reflect::apply(&callback, receiver, args)
}

/// Formats the source's Node inspection label through the caller's current Fiber.
///
/// # Errors
/// Propagates Fiber and name getter failures.
#[wasm_bindgen(js_name = contextInspect)]
pub fn inspect(context: &JsValue) -> Result<String, JsValue> {
    let fiber = values::get(context, &"fiber".into())?;
    let name = values::template_string(&values::get(&fiber, &"name".into())?)?;
    Ok(format!("Context <{name}>"))
}

/// Tests properties that bypass service reflection in the source Context proxy.
///
/// # Errors
/// Propagates JavaScript numeric-string conversion failures.
#[wasm_bindgen(js_name = contextSpecialProperty)]
pub fn special_property(key: &JsValue) -> Result<bool, JsValue> {
    if key.is_symbol() {
        return Ok(true);
    }
    if key
        .as_string()
        .is_some_and(|key| key.starts_with('_') || matches!(key.as_str(), "prototype" | "then"))
    {
        return Ok(true);
    }
    Function::new_with_args("key", "return parseInt(key).toString() === key;")
        .call1(&JsValue::UNDEFINED, key)
        .map(|value| value.is_truthy())
}

/// Tests raw properties and declarations without evaluating computed accessors.
///
/// # Errors
/// Propagates property-table and prototype failures.
#[wasm_bindgen(js_name = contextHas)]
pub fn has(target: &JsValue, key: &JsValue) -> Result<bool, JsValue> {
    if special_property(key)? {
        return Reflect::has(target, key);
    }
    if Reflect::has(target, key)? {
        return Ok(true);
    }
    let reflect = values::get(target, &"reflect".into())?;
    values::get(&values::get(&reflect, &"props".into())?, key).map(|value| value.is_truthy())
}

/// Reads a declared accessor or dependency using the error captured by the Proxy trap.
///
/// # Errors
/// Preserves hook failures and enhances only the supplied diagnostic carrier.
#[wasm_bindgen(js_name = contextReflectedGet)]
pub fn get(
    target: &JsValue,
    key: &JsValue,
    context: &JsValue,
    error: &JsValue,
) -> Result<JsValue, JsValue> {
    message(error, "get", key, "without inject")?;
    let result = (|| {
        let reflect = values::get(target, &"reflect".into())?;
        let definition = values::get(&values::get(&reflect, &"props".into())?, key)?;
        if !definition.is_null()
            && !definition.is_undefined()
            && values::get(&definition, &"type".into())?
                .as_string()
                .as_deref()
                == Some("accessor")
        {
            let hook = values::get(&definition, &"get".into())?;
            return method(
                &hook,
                "call",
                &Array::of3(context, &values::get(context, &symbol("receiver")?)?, error),
            );
        }
        let fiber = values::get(context, &"fiber".into())?;
        if !values::get(&fiber, &"runtime".into())?.is_truthy() {
            return method(
                &values::get(context, &"reflect".into())?,
                "get",
                &Array::of2(key, &JsValue::FALSE),
            );
        }
        let (target, context, key, error) =
            (target.clone(), context.clone(), key.clone(), error.clone());
        let (next_target, next_context, next_key, next_error) =
            (target, context.clone(), key.clone(), error.clone());
        let next = Closure::wrap(Box::new(move || {
            lookup(&next_target, &next_key, &next_context, &next_error)
        }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
        .into_js_value();
        let args = Array::of4(&"internal/get".into(), &context, &key, &error);
        args.push(&next);
        method(
            &values::get(&context, &"events".into())?,
            "waterfall",
            &args,
        )
    })();
    enhance_result(result, error)
}

fn lookup(
    target: &JsValue,
    key: &JsValue,
    context: &JsValue,
    error: &JsValue,
) -> Result<JsValue, JsValue> {
    let scope = values::get(&values::get(target, &symbol("isolate")?)?, key)?;
    let shadow = values::get(context, &symbol("shadow")?)?;
    let owner = if shadow.is_null() || shadow.is_undefined() {
        context
    } else {
        &shadow
    };
    let mut fiber = values::get(owner, &"fiber".into())?;
    loop {
        let store = values::get(&fiber, &"store".into())?;
        if !store.is_null() && !store.is_undefined() {
            let record = values::get(&store, key)?;
            if record.is_truthy() {
                return Tracer::new(context.clone()).trace(&values::get(&record, &"value".into())?);
            }
        }
        if Reflect::has(&values::get(&fiber, &"inject".into())?, key)? {
            let name = values::template_string(key)?;
            values::set(
                error,
                &"message".into(),
                &format!("cannot get required service \"{name}\" in inactive context").into(),
            )?;
            return Err(error.clone());
        }
        if !values::get(&fiber, &"runtime".into())?.is_truthy() {
            return Err(error.clone());
        }
        let parent = values::get(&fiber, &"parent".into())?;
        if values::get(&values::get(&parent, &symbol("isolate")?)?, key)? != scope {
            return Err(error.clone());
        }
        fiber = values::get(&parent, &"fiber".into())?;
    }
}

/// Assigns an accessor or provider-owned service through the source interceptor chain.
///
/// # Errors
/// Preserves setter failures and enhances only the supplied diagnostic carrier.
#[wasm_bindgen(js_name = contextReflectedSet)]
pub fn set(
    target: &JsValue,
    key: &JsValue,
    value: &JsValue,
    context: &JsValue,
    error: &JsValue,
) -> Result<JsValue, JsValue> {
    message(error, "set", key, "without provide")?;
    let reflect = values::get(target, &"reflect".into())?;
    let definition = values::get(&values::get(&reflect, &"props".into())?, key)?;
    if !definition.is_truthy() {
        let fiber = values::get(context, &"fiber".into())?;
        if !values::get(&fiber, &"runtime".into())?.is_truthy() {
            return Reflect::set_with_receiver(target, key, value, context).map(JsValue::from_bool);
        }
        return Err(enhance(error)?);
    }
    let result = (|| {
        if values::get(&definition, &"type".into())?
            .as_string()
            .as_deref()
            == Some("accessor")
        {
            let hook = values::get(&definition, &"set".into())?;
            if !hook.is_truthy() {
                return Ok(JsValue::FALSE);
            }
            return method(
                &hook,
                "call",
                &Array::of4(
                    context,
                    value,
                    &values::get(context, &symbol("receiver")?)?,
                    error,
                ),
            );
        }
        let (context, key, value, error) =
            (context.clone(), key.clone(), value.clone(), error.clone());
        let (next_context, next_key, next_value, next_error) =
            (context.clone(), key.clone(), value.clone(), error.clone());
        let next = Closure::wrap(Box::new(move || {
            method(
                &values::get(&next_context, &"reflect".into())?,
                "set",
                &Array::of3(&next_key, &next_value, &next_error),
            )
        }) as Box<dyn Fn() -> Result<JsValue, JsValue>>)
        .into_js_value();
        let args = Array::of4(&"internal/set".into(), &context, &key, &value);
        args.push(&error);
        args.push(&next);
        method(
            &values::get(&context, &"events".into())?,
            "waterfall",
            &args,
        )
    })();
    enhance_result(result, error)
}

fn message(error: &JsValue, operation: &str, key: &JsValue, suffix: &str) -> Result<(), JsValue> {
    let key = values::template_string(key)?;
    values::set(
        error,
        &"message".into(),
        &format!("cannot {operation} property \"{key}\" {suffix}").into(),
    )
}

fn enhance_result(result: Result<JsValue, JsValue>, error: &JsValue) -> Result<JsValue, JsValue> {
    match result {
        Err(failure) if failure == *error => Err(enhance(error)?),
        other => other,
    }
}

fn enhance(error: &JsValue) -> Result<JsValue, JsValue> {
    let stack = values::get(error, &"stack".into())?;
    let lines = method(&stack, "split", &Array::of1(&"\n".into()))?;
    let message = values::template_string(&values::get(error, &"message".into())?)?;
    method(
        &lines,
        "splice",
        &Array::of3(&0.into(), &2.into(), &format!("Error: {message}").into()),
    )?;
    values::set(
        error,
        &"stack".into(),
        &method(&lines, "join", &Array::of1(&"\n".into()))?,
    )?;
    Ok(error.clone())
}
