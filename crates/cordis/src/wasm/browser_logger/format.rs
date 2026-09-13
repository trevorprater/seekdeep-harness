//! Logger formatting retains JavaScript values, UTF-16 strings, and mutable palettes.

use js_sys::{Array, Function, Object, Reflect, RegExp};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use super::super::browser_values as values;

thread_local! {
    static C16: Array = [6, 2, 3, 4, 5, 1].into_iter().map(JsValue::from).collect();
    static C256: Array = [20,21,26,27,32,33,38,39,40,41,42,43,44,45,56,57,62,63,68,69,74,75,76,77,78,79,80,81,92,93,98,99,112,113,129,134,135,148,149,160,161,162,163,164,165,166,167,168,169,170,171,172,173,178,179,184,185,196,197,198,199,200,201,202,203,204,205,206,207,208,209,214,215,220,221].into_iter().map(JsValue::from).collect();
    static FORMATTERS: std::cell::RefCell<Option<Object>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn method(value: &JsValue, name: &str, args: &Array) -> Result<JsValue, JsValue> {
    let function = values::get(value, &name.into())?.dyn_into::<Function>()?;
    Reflect::apply(&function, value, args)
}

/// Returns the shared source 16-color palette.
#[wasm_bindgen(js_name = loggerColors16)]
pub fn colors16() -> Array {
    C16.with(Clone::clone)
}

/// Returns the shared source 256-color palette.
#[wasm_bindgen(js_name = loggerColors256)]
pub fn colors256() -> Array {
    C256.with(Clone::clone)
}

/// Returns the mutable default logger formatter table.
///
/// # Errors
/// Propagates formatter function construction failures.
#[wasm_bindgen(js_name = loggerFormatters)]
pub fn default_formatters() -> Result<Object, JsValue> {
    if let Some(formatters) = FORMATTERS.with(|slot| slot.borrow().clone()) {
        return Ok(formatters);
    }
    let formatters = Object::new();
    for name in ["s", "d", "i", "f", "o", "O", "c", "C"] {
        let invoke = Closure::wrap(Box::new(
            move |value: JsValue, exporter: JsValue, message: JsValue| {
                default_format(name, &value, &exporter, &message)
            },
        )
            as Box<dyn Fn(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value();
        let (parameters, args) = match name {
            "c" => ("", "undefined,undefined,undefined"),
            "C" => ("value,exporter,message", "value,exporter,message"),
            _ => ("value", "value,undefined,undefined"),
        };
        let callback = Function::new_with_args(
            "invoke",
            &format!("return ({{ {name}: ({parameters}) => invoke({args}) }}).{name};"),
        )
        .call1(&JsValue::UNDEFINED, &invoke)?;
        values::set(&formatters, &name.into(), &callback)?;
    }
    FORMATTERS.with(|slot| *slot.borrow_mut() = Some(formatters.clone()));
    Ok(formatters)
}

fn default_format(
    name: &str,
    value: &JsValue,
    exporter: &JsValue,
    message: &JsValue,
) -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    match name {
        "s" => {
            let function = values::get(&global, &"String".into())?.dyn_into::<Function>()?;
            Reflect::apply(&function, &JsValue::UNDEFINED, &Array::of1(value))
        }
        "d" | "i" | "f" => {
            let number = values::get(&global, &"Number".into())?.dyn_into::<Function>()?;
            let value = Reflect::apply(&number, &JsValue::UNDEFINED, &Array::of1(value))?;
            if name == "f" {
                Ok(value)
            } else {
                method(
                    &values::get(&global, &"Math".into())?,
                    "trunc",
                    &Array::of1(&value),
                )
            }
        }
        "o" | "O" => method(
            &values::get(&global, &"JSON".into())?,
            "stringify",
            &Array::of1(value),
        ),
        "c" => Ok("".into()),
        "C" => {
            let class = super::logger_class()?;
            let code = method(
                &class,
                "code",
                &Array::of2(
                    &values::get(message, &"name".into())?,
                    &values::get(exporter, &"colors".into())?,
                ),
            )?;
            method(&class, "color", &Array::of3(exporter, &code, value))
        }
        _ => unreachable!(),
    }
}

/// Applies source ANSI color selection and decoration rules.
///
/// # Errors
/// Propagates exporter and string-conversion failures.
#[wasm_bindgen(js_name = loggerColor)]
pub fn color(
    exporter: &JsValue,
    code: &JsValue,
    value: &JsValue,
    decoration: &JsValue,
) -> Result<JsValue, JsValue> {
    if !values::get(exporter, &"colors".into())?.is_truthy() {
        return Function::new_with_args("value", "return '' + value;")
            .call1(&JsValue::UNDEFINED, value);
    }
    let prefix = if Function::new_with_args("code", "return code < 8;")
        .call1(&JsValue::UNDEFINED, code)?
        .is_truthy()
    {
        code.clone()
    } else {
        Function::new_with_args("code", "return '8;5;' + code;").call1(&JsValue::UNDEFINED, code)?
    };
    let colors = values::get(exporter, &"colors".into())?;
    let decoration = if Function::new_with_args("colors", "return colors >= 2;")
        .call1(&JsValue::UNDEFINED, &colors)?
        .is_truthy()
    {
        decoration.clone()
    } else {
        "".into()
    };
    Function::new_with_args(
        "prefix,decoration,value",
        r"return `\u001b[3${prefix}${decoration}m${value}\u001b[0m`;",
    )
    .call3(&JsValue::UNDEFINED, &prefix, &decoration, value)
}

/// Computes the source's signed 32-bit UTF-16 logger-name color hash.
///
/// # Errors
/// Propagates string and palette access failures.
#[wasm_bindgen(js_name = loggerCode)]
pub fn code(name: &JsValue, level: &JsValue) -> Result<JsValue, JsValue> {
    let mut hash = 0_i32;
    let mut index = 0_u32;
    let before = Function::new_with_args("index,name", "return index < name.length;");
    let accumulate = Function::new_with_args(
        "hash,unit",
        "return (((hash << 3) - hash) + unit + 13) | 0;",
    );
    while before
        .call2(&JsValue::UNDEFINED, &index.into(), name)?
        .is_truthy()
    {
        let value = method(name, "charCodeAt", &Array::of1(&index.into()))?;
        let value = accumulate.call2(&JsValue::UNDEFINED, &hash.into(), &value)?;
        #[allow(clippy::cast_possible_truncation)]
        {
            hash = value.as_f64().unwrap_or_default() as i32;
        }
        index += 1;
    }
    if !level.is_truthy() {
        return Ok(JsValue::UNDEFINED);
    }
    let palette = if Function::new_with_args("level", "return level >= 2;")
        .call1(&JsValue::UNDEFINED, level)?
        .is_truthy()
    {
        colors256()
    } else {
        colors16()
    };
    if palette.length() == 0 {
        return Ok(JsValue::UNDEFINED);
    }
    Ok(palette.get(hash.unsigned_abs() % palette.length()))
}

fn formatter(exporter: &JsValue, name: &JsValue) -> Result<JsValue, JsValue> {
    let formatters = values::get(exporter, &"formatters".into())?;
    let formatter = if formatters.is_null() || formatters.is_undefined() {
        JsValue::UNDEFINED
    } else {
        values::get(&formatters, name)?
    };
    if formatter.is_null() || formatter.is_undefined() {
        values::get(default_formatters()?.as_ref(), name)
    } else {
        Ok(formatter)
    }
}

/// Formats placeholders, remaining arguments, and per-line output limits.
///
/// # Errors
/// Propagates formatter, value, and string-operation failures unchanged.
#[wasm_bindgen(js_name = loggerFormat)]
pub fn format(exporter: &JsValue, message: &JsValue) -> Result<JsValue, JsValue> {
    let args = method(
        &values::get(message, &"args".into())?,
        "slice",
        &Array::new(),
    )?;
    let first = values::get(&args, &0.into())?;
    if super::is_error(&first)? {
        let stack = values::get(&first, &"stack".into())?;
        let first = if stack.is_truthy() {
            stack
        } else {
            values::get(&first, &"message".into())?
        };
        values::set(&args, &0.into(), &first)?;
        method(&args, "unshift", &Array::of1(&"%s".into()))?;
    } else if !first.is_string() {
        method(&args, "unshift", &Array::of1(&"%o".into()))?;
    }
    let text = method(&args, "shift", &Array::new())?;
    let (exporter_copy, message_copy, remaining) =
        (exporter.clone(), message.clone(), args.clone());
    let replace = Closure::wrap(Box::new(move |matched: JsValue, character: JsValue| {
        if matched.as_string().as_deref() == Some("%%") {
            return Ok("%".into());
        }
        let callback = formatter(&exporter_copy, &character)?;
        if let Some(callback) = callback.dyn_ref::<Function>() {
            let value = method(&remaining, "shift", &Array::new())?;
            Reflect::apply(
                callback,
                &JsValue::UNDEFINED,
                &Array::of3(&value, &exporter_copy, &message_copy),
            )
        } else {
            Ok(matched)
        }
    })
        as Box<dyn Fn(JsValue, JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let mut text = method(
        &text,
        "replace",
        &Array::of2(&RegExp::new("%([a-zA-Z%])", "g"), &replace),
    )?;
    let object_formatter = formatter(exporter, &"o".into())?;
    values::for_each(&args, |mut value| {
        if !value.is_null() && value.is_object() && !value.is_function() {
            value = Reflect::apply(
                &object_formatter
                    .clone()
                    .dyn_into::<Function>()
                    .map_err(|_| js_sys::TypeError::new("oFormatter is not a function"))?,
                &JsValue::UNDEFINED,
                &Array::of3(&value, exporter, message),
            )?;
        }
        text = Function::new_with_args("text,value", "return text + ' ' + value;").call2(
            &JsValue::UNDEFINED,
            &text,
            &value,
        )?;
        Ok(())
    })?;
    let limit = values::get(exporter, &"maxLength".into())?;
    let limit = if limit.is_undefined() {
        10240.into()
    } else {
        limit
    };
    let lines = method(&text, "split", &Array::of1(&RegExp::new("\\r?\\n", "g")))?;
    let trim = Closure::wrap(Box::new(move |line: JsValue| {
        let trimmed = method(&line, "slice", &Array::of2(&0.into(), &limit))?;
        let longer = Function::new_with_args("line,limit", "return line.length > limit;")
            .call2(&JsValue::UNDEFINED, &line, &limit)?
            .is_truthy();
        let suffix: JsValue = if longer { "..." } else { "" }.into();
        Function::new_with_args("line,suffix", "return line + suffix;").call2(
            &JsValue::UNDEFINED,
            &trimmed,
            &suffix,
        )
    }) as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let output = method(&lines, "map", &Array::of1(&trim))?;
    method(&output, "join", &Array::of1(&"\n".into()))
}

#[allow(clippy::float_cmp)] // JavaScript charCodeAt returns exact code units or NaN.
pub(super) fn hyphenate(name: &JsValue) -> Result<JsValue, JsValue> {
    let mut state = 0_u8;
    let output = Array::new();
    let length = values::get(name, &"length".into())?.as_f64().unwrap_or(0.0);
    let mut index = 0_u32;
    while f64::from(index) < length {
        let code = method(name, "charCodeAt", &Array::of1(&index.into()))?
            .as_f64()
            .unwrap_or(f64::NAN);
        if (65.0..=90.0).contains(&code) {
            if state == 2 {
                let next = method(name, "charCodeAt", &Array::of1(&(index + 1).into()))?
                    .as_f64()
                    .unwrap_or(f64::NAN);
                if (97.0..=122.0).contains(&next) {
                    output.push(&45.into());
                }
            } else if state != 0 {
                output.push(&45.into());
            }
            output.push(&(code + 32.0).into());
            state = 2;
        } else if (97.0..=122.0).contains(&code) {
            output.push(&code.into());
            state = 1;
        } else if code == 45.0 || code == 95.0 {
            if state != 0 {
                output.push(&45.into());
            }
            state = 0;
        } else {
            output.push(&code.into());
        }
        index += 1;
    }
    method(
        &values::get(&js_sys::global(), &"String".into())?,
        "fromCharCode",
        &output,
    )
}
