//! Compiled browser clock and duration helpers for conversation chrome.

use js_sys::{Array, Date, Function, Math, Number, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

/// Returns local midnight for the calendar day containing `milliseconds`.
///
/// # Errors
///
/// Returns if the browser's `Date` methods cannot be invoked.
#[wasm_bindgen(js_name = startOfLocalDay)]
pub fn start_of_local_day_browser(milliseconds: f64) -> Result<f64, JsValue> {
    let date = Date::new(&JsValue::from_f64(milliseconds));
    set_hours(&date, 0.0)?;
    date_number(&date, "getTime")
}

/// Returns the delay to the next local midnight, clamped to at least one millisecond.
///
/// # Errors
///
/// Returns if the browser's `Date` methods cannot be invoked.
#[wasm_bindgen(js_name = msUntilNextLocalMidnight)]
pub fn milliseconds_until_next_local_midnight_browser(milliseconds: f64) -> Result<f64, JsValue> {
    let date = Date::new(&JsValue::from_f64(milliseconds));
    set_hours(&date, 24.0)?;
    Ok(Math::max(
        date_number(&date, "getTime")? - milliseconds,
        1.0,
    ))
}

/// Formats a localized whole-second run duration.
///
/// # Errors
///
/// Returns if the translate seat throws or a JavaScript number cannot be stringified.
#[wasm_bindgen(js_name = formatRunDuration)]
#[allow(clippy::needless_pass_by_value)]
pub fn format_run_duration_browser(
    milliseconds: f64,
    translate: Function,
) -> Result<JsValue, JsValue> {
    let total = Math::max(0.0, Math::floor(milliseconds / 1_000.0));
    let minutes = Math::floor(total / 60.0);
    let seconds = total % 60.0;
    if minutes > 0.0 {
        translate_value(
            &translate,
            "duration.minutes",
            &object(&[
                ("minutes", JsValue::from_f64(minutes)),
                ("seconds", JsValue::from_str(&pad2(seconds)?)),
            ])?,
        )
    } else {
        translate_value(
            &translate,
            "duration.seconds",
            &object(&[("seconds", JsValue::from_f64(seconds))])?,
        )
    }
}

/// Formats sub-turn latency without a unit using JavaScript rounding semantics.
///
/// # Errors
///
/// Returns if the resulting JavaScript number cannot be stringified.
#[wasm_bindgen(js_name = formatLatencySeconds)]
pub fn format_latency_seconds_browser(milliseconds: f64) -> Result<String, JsValue> {
    let seconds = Math::max(0.0, milliseconds) / 1_000.0;
    let rounded = if seconds < 10.0 {
        Math::round(seconds * 10.0) / 10.0
    } else {
        Math::round(seconds)
    };
    number_string(rounded)
}

/// Formats decode throughput without a unit using JavaScript rounding semantics.
///
/// # Errors
///
/// Returns if the resulting JavaScript number cannot be stringified.
#[wasm_bindgen(js_name = formatTokensPerSecond)]
pub fn format_tokens_per_second_browser(tokens_per_second: f64) -> Result<String, JsValue> {
    let clamped = Math::max(0.0, tokens_per_second);
    let rounded = if clamped >= 10.0 {
        Math::round(clamped)
    } else {
        Math::round(clamped * 10.0) / 10.0
    };
    number_string(rounded)
}

/// Formats a date-aware local message clock.
///
/// Omitted `now` uses the browser wall clock, matching the compatibility helper.
///
/// # Errors
///
/// Returns if the browser's `Date` methods or translate seat cannot be invoked.
#[wasm_bindgen(js_name = formatMessageClock)]
#[allow(clippy::float_cmp, clippy::needless_pass_by_value)]
pub fn format_message_clock_browser(
    time: f64,
    translate: Function,
    now: Option<f64>,
) -> Result<String, JsValue> {
    let date = Date::new(&JsValue::from_f64(time));
    let reference = Date::new(&JsValue::from_f64(now.unwrap_or_else(Date::now)));
    let year = date_number(&date, "getFullYear")?;
    let month = date_number(&date, "getMonth")?;
    let day = date_number(&date, "getDate")?;
    let reference_year = date_number(&reference, "getFullYear")?;
    let clock = format!(
        "{}:{}",
        pad2(date_number(&date, "getHours")?)?,
        pad2(date_number(&date, "getMinutes")?)?
    );
    if year == reference_year
        && month == date_number(&reference, "getMonth")?
        && day == date_number(&reference, "getDate")?
    {
        return Ok(clock);
    }
    let parameters = object(&[
        ("y", JsValue::from_f64(year)),
        ("m", JsValue::from_f64(month + 1.0)),
        ("d", JsValue::from_f64(day)),
    ])?;
    let key = if year == reference_year {
        "clock.md"
    } else {
        "clock.ymd"
    };
    let prefix = javascript_string(&translate_value(&translate, key, &parameters)?)?;
    Ok(format!("{prefix} {clock}"))
}

fn set_hours(date: &Date, hours: f64) -> Result<(), JsValue> {
    let arguments = [
        JsValue::from_f64(hours),
        JsValue::from_f64(0.0),
        JsValue::from_f64(0.0),
        JsValue::from_f64(0.0),
    ];
    call_method(date.as_ref(), "setHours", &arguments)?;
    Ok(())
}

fn date_number(date: &Date, method: &str) -> Result<f64, JsValue> {
    call_method(date.as_ref(), method, &[])?
        .as_f64()
        .ok_or_else(|| {
            js_sys::TypeError::new(&format!("Date.{method}() did not return a number")).into()
        })
}

fn translate_value(
    translate: &Function,
    key: &str,
    parameters: &Object,
) -> Result<JsValue, JsValue> {
    translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(&JsValue::from_str(key), parameters.as_ref()),
    )
}

fn number_string(value: f64) -> Result<String, JsValue> {
    Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() did not return a string").into())
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("String"))?.dyn_into::<Function>()?;
    constructor
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() did not return a string").into())
}

fn pad2(value: f64) -> Result<String, JsValue> {
    let value = number_string(value)?;
    Ok(if value.len() < 2 {
        format!("0{value}")
    } else {
        value
    })
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
