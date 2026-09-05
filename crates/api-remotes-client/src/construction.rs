//! Executes Rust-emitted schema construction plans against the pinned Zod dependency.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

pub(crate) fn generate(zod: &JsValue) -> Result<Array, JsValue> {
    let plans = js_sys::JSON::parse(include_str!("../contracts/remote-plans.json"))?;
    let result = Array::new();
    for module in Array::from(&field(&plans, "modules")?).iter() {
        let env = Object::create(&JsValue::NULL.unchecked_into());
        define(&env, "z", zod)?;
        for binding in Array::from(&field(&module, "bindings")?).iter() {
            let binding = Array::from(&binding);
            let name = text(&binding.get(0))?;
            let value = evaluate(&binding.get(1), &env)?;
            define(&env, &name, &value)?;
        }
        result.push(&evaluate(&field(&module, "result")?, &env)?);
    }
    Ok(result)
}

fn evaluate(expression: &JsValue, env: &Object) -> Result<JsValue, JsValue> {
    let expression = Array::from(expression);
    let first = expression.get(1);
    match text(&expression.get(0))?.as_str() {
        "literal" => Ok(first),
        "name" => {
            let name = text(&first)?;
            if name == "undefined" {
                return Ok(JsValue::UNDEFINED);
            }
            if !Reflect::has(env, &first)? {
                return Err(js_sys::ReferenceError::new(&format!("{name} is not defined")).into());
            }
            Reflect::get(env, &first)
        }
        "array" => Array::from(&first)
            .iter()
            .map(|value| evaluate(&value, env))
            .collect::<Result<Array, _>>()
            .map(Into::into),
        "object" => {
            let object = Object::new();
            for entry in Array::from(&first).iter() {
                let entry = Array::from(&entry);
                let key = text(&entry.get(0))?;
                let value = evaluate(&entry.get(1), env)?;
                if key == "__proto__" {
                    if value.is_object() || value.is_null() {
                        Reflect::set_prototype_of(&object, &value)?;
                    }
                } else {
                    define(&object, &key, &value)?;
                }
            }
            Ok(object.into())
        }
        "member" => Reflect::get(&evaluate(&first, env)?, &expression.get(2)),
        "call" => {
            let callee = Array::from(&first);
            let (receiver, function) = if text(&callee.get(0))? == "member" {
                let receiver = evaluate(&callee.get(1), env)?;
                let function = Reflect::get(&receiver, &callee.get(2))?;
                (receiver, function)
            } else {
                (JsValue::UNDEFINED, evaluate(&first, env)?)
            };
            let args = Array::from(&expression.get(2))
                .iter()
                .map(|value| evaluate(&value, env))
                .collect::<Result<Array, _>>()?;
            function.dyn_into::<Function>()?.apply(&receiver, &args)
        }
        "lambda" => {
            let names = Array::from(&first);
            let body = expression.get(2);
            let invoke =
                Closure::wrap(Box::new(
                    move |parent: JsValue, args: Array| -> Result<JsValue, JsValue> {
                        let local = Object::create(&Object::from(parent));
                        for (index, name) in names.iter().enumerate() {
                            define(
                                &local,
                                &text(&name)?,
                                &args.get(u32::try_from(index).map_err(|_| {
                                    js_sys::Error::new("too many schema parameters")
                                })?),
                            )?;
                        }
                        evaluate(&body, &local)
                    },
                )
                    as Box<dyn Fn(JsValue, Array) -> Result<JsValue, JsValue>>);
            Function::new_with_args("env,invoke", "return (...args) => invoke(env, args);").call2(
                &JsValue::UNDEFINED,
                env,
                &invoke.into_js_value(),
            )
        }
        "negate" => Ok(JsValue::from_f64(
            -evaluate(&first, env)?
                .as_f64()
                .ok_or_else(|| js_sys::Error::new("generated negation requires a number"))?,
        )),
        other => Err(js_sys::Error::new(&format!(
            "unknown generated construction operation {other}"
        ))
        .into()),
    }
}

fn define(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    let descriptor = Object::new();
    for (key, value) in [
        ("value", value.clone()),
        ("writable", JsValue::TRUE),
        ("enumerable", JsValue::TRUE),
        ("configurable", JsValue::TRUE),
    ] {
        Reflect::set(&descriptor, &key.into(), &value)?;
    }
    Reflect::define_property(object, &key.into(), &descriptor)?;
    Ok(())
}
fn field(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    Reflect::get(value, &key.into())
}
fn text(value: &JsValue) -> Result<String, JsValue> {
    value
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("invalid generated plan string").into())
}
