//! Browser Agent-scope primitive over the page's Cordis Context.

use js_sys::{Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

thread_local! {
    static SCOPE_KEY: JsValue = new_symbol("seekdeep.client.scope");
}

/// Mints one no-op Cordis fiber and its tagged Agent-scoped context.
///
/// # Errors
///
/// Returns missing Cordis `plugin`, `ctx`, `extend`, or `Context.filter` members.
#[wasm_bindgen(js_name = createScope)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_client_scope(root: JsValue, key: String) -> Result<JsValue, JsValue> {
    let plugin = required_function(&root, "plugin", "Client root Context")?;
    let no_op = Function::new_no_args("");
    let fiber = plugin.call1(&root, &no_op)?;
    let context = required(&fiber, "ctx", "Agent scope fiber")?;
    let constructor = required(&context, "constructor", "Cordis Context")?;
    let filter = required(&constructor, "filter", "Cordis Context constructor")?;
    let extension = Object::new();
    SCOPE_KEY.with(|scope| set_symbol(&extension, scope, &JsValue::from_str(&key)))?;
    let expected = key;
    let predicate = Closure::wrap(Box::new(move |listener: JsValue| {
        scope_of_value(&listener).is_none_or(|tag| tag == expected)
    }) as Box<dyn FnMut(JsValue) -> bool>);
    set_symbol(&extension, &filter, &predicate.into_js_value())?;
    let extend = required_function(&context, "extend", "Cordis Context")?;
    let scoped = extend.call1(&context, &extension)?;
    let result = Object::new();
    set(&result, "fiber", &fiber)?;
    set(&result, "ctx", &scoped)?;
    Ok(result.into())
}

/// Reads the nearest inherited Client Agent-scope identity.
#[wasm_bindgen(js_name = scopeOf)]
#[allow(clippy::needless_pass_by_value)]
pub fn scope_of(context: JsValue) -> Option<String> {
    scope_of_value(&context)
}

fn scope_of_value(context: &JsValue) -> Option<String> {
    SCOPE_KEY.with(|scope| {
        Reflect::get(context, scope)
            .ok()
            .and_then(|tag| tag.as_string())
    })
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let member = Reflect::get(value, &JsValue::from_str(key))?;
    if member.is_undefined() || member.is_null() {
        Err(js_sys::Error::new(&format!("{owner} requires {key:?}")).into())
    } else {
        Ok(member)
    }
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into::<Function>()
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set Agent scope member {key:?}")).into())
    }
}

fn set_symbol(object: &Object, key: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, key, value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new("failed to set Agent scope symbol").into())
    }
}

fn new_symbol(description: &str) -> JsValue {
    let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str("Symbol"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    constructor
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(description))
        .unwrap()
}
