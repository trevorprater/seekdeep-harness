//! Browser-engine closure evaluation owned and assembled by Rust/WASM.

use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

use crate::{
    CLIENT_INVALID_PLUGIN, CLIENT_MISSING_RETURN, DYNAMIC_CLIENT_REDIRECTS, client_parse_failure,
};

/// Evaluates one Client function body with the exact closure symbol surface.
///
/// `react` must be the page's existing React object. `styles` is the
/// package-owned style binding. `invoke` receives `(method, args)`, and
/// `note_error` receives the bounded mirrored `console.error` line.
///
/// # Errors
///
/// Returns construction, syntax, execution, rejection, or plugin-shape errors
/// with the browser's original JavaScript value.
#[wasm_bindgen(js_name = evaluateCordisClientHalf)]
pub async fn evaluate_client_half(
    plugin_id: String,
    client_code: String,
    react: JsValue,
    styles: JsValue,
    invoke: Function,
    note_error: Function,
) -> Result<JsValue, JsValue> {
    let returned = begin_evaluate_client_half(
        &plugin_id,
        &client_code,
        &react,
        &styles,
        &invoke,
        &note_error,
    )?;
    classify_evaluated_client_value(JsFuture::from(returned).await?)
}

pub(crate) fn begin_evaluate_client_half(
    plugin_id: &str,
    client_code: &str,
    react: &JsValue,
    styles: &JsValue,
    invoke: &Function,
    note_error: &Function,
) -> Result<Promise, JsValue> {
    let parameters = [
        "React",
        "console",
        "styles",
        "host",
        "harness",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
        "fetch",
        "require",
        "process",
        "Buffer",
    ];
    let body = format!("return (async () => {{\n{client_code}\n}})()");
    let closure = construct_function(&parameters, &body).map_err(|error| {
        if js_error_name(&error).as_deref() == Some("SyntaxError") {
            js_sys::Error::new(&client_parse_failure(&js_error_message(&error))).into()
        } else {
            error
        }
    })?;
    let console = tagged_console(plugin_id, note_error)?;
    let host = host_binding(invoke)?;
    let harness = harness_trap()?;
    let traps = DYNAMIC_CLIENT_REDIRECTS
        .iter()
        .map(|(name, redirect)| {
            construct_function(
                &[],
                &format!(
                    "throw new Error({})",
                    serde_json::to_string(&format!(
                        "{name} is not available in a dynamic client half — {redirect}"
                    ))
                    .expect("trap text is JSON")
                ),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let arguments = Array::new();
    arguments.push(react);
    arguments.push(&console);
    arguments.push(styles);
    arguments.push(&host);
    arguments.push(&harness);
    for trap in &traps {
        arguments.push(trap);
    }
    arguments.push(&JsValue::UNDEFINED);
    arguments.push(&JsValue::UNDEFINED);
    Ok(Promise::resolve(
        &closure.apply(&JsValue::UNDEFINED, &arguments)?,
    ))
}

pub(crate) fn classify_evaluated_client_value(returned: JsValue) -> Result<JsValue, JsValue> {
    if returned.is_undefined() {
        return Err(js_sys::Error::new(CLIENT_MISSING_RETURN).into());
    }
    if returned.is_function() {
        return Ok(returned);
    }
    if returned.is_object() && Reflect::get(&returned, &JsValue::from_str("apply"))?.is_function() {
        return Ok(returned);
    }
    Err(js_sys::Error::new(CLIENT_INVALID_PLUGIN).into())
}

fn construct_function(parameters: &[&str], body: &str) -> Result<Function, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("Function"))?.dyn_into::<Function>()?;
    let arguments = Array::new();
    for parameter in parameters {
        arguments.push(&JsValue::from_str(parameter));
    }
    arguments.push(&JsValue::from_str(body));
    Reflect::construct(&constructor, &arguments)?.dyn_into::<Function>()
}

fn tagged_console(plugin_id: &str, note_error: &Function) -> Result<JsValue, JsValue> {
    let factory = construct_function(
        &["pluginId", "noteError"],
        r"
const tag = `[cordis:${pluginId}]`;
const stringify = value => {
  if (value instanceof Error) return value.message;
  if (typeof value === 'string') return value;
  if (value === undefined) return 'undefined';
  try { return JSON.stringify(value); } catch { return '[unserializable console argument]'; }
};
const result = { ...console };
for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
  result[level] = (...values) => {
    console[level](tag, ...values);
    if (level === 'error') noteError(values.map(stringify).join(' ').slice(0, 500));
  };
}
return result;
",
    )?;
    let arguments = Array::new();
    arguments.push(&JsValue::from_str(plugin_id));
    arguments.push(note_error);
    factory.apply(&JsValue::UNDEFINED, &arguments)
}

fn host_binding(invoke: &Function) -> Result<JsValue, JsValue> {
    let factory = construct_function(
        &["invoke"],
        "return { call(method, args = null) { return invoke(method, args); } };",
    )?;
    let arguments = Array::new();
    arguments.push(invoke);
    factory.apply(&JsValue::UNDEFINED, &arguments)
}

fn harness_trap() -> Result<JsValue, JsValue> {
    construct_function(
        &[],
        r"
return new Proxy({}, {
  get(_target, property) {
    throw new Error(`harness.${String(property)} belongs to the HOST half (\`code\`): register handlers there with harness.handle(method, fn); the browser half calls them via host.call(method, args).`);
  },
});
",
    )?
    .call0(&JsValue::UNDEFINED)
}

fn js_error_name(error: &JsValue) -> Option<String> {
    Reflect::get(error, &JsValue::from_str("name"))
        .ok()
        .and_then(|value| value.as_string())
}

fn js_error_message(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}
