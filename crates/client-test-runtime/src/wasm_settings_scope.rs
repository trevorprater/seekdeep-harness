//! Browser settings-scope double with spy-backed write methods.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

struct BrowserSettingsScopeState {
    snapshot: RefCell<JsValue>,
    listeners: RefCell<Vec<Function>>,
}

/// Creates a source-shaped settings-scope handle using the supplied test spy factory.
///
/// # Errors
///
/// Returns malformed spy-factory results, object construction, or listener failures.
#[wasm_bindgen(js_name = createStubSettingsScope)]
#[allow(clippy::needless_pass_by_value)]
pub fn create_stub_settings_scope(spy_factory: JsValue) -> Result<JsValue, JsValue> {
    let spy_factory = spy_factory
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("stubSettingsScope requires a spy factory"))?;
    let state = Rc::new(BrowserSettingsScopeState {
        snapshot: RefCell::new(initial_snapshot()?.into()),
        listeners: RefCell::new(Vec::new()),
    });

    let set_impl = Closure::wrap(Box::new(|_field: JsValue, _value: JsValue| -> Promise {
        Promise::resolve(&JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    let set_spy = spy_factory
        .call1(&JsValue::UNDEFINED, &set_impl.into_js_value())?
        .dyn_into::<Function>()?;
    let unset_impl = Closure::wrap(Box::new(|_field: JsValue| -> Promise {
        Promise::resolve(&JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let unset_spy = spy_factory
        .call1(&JsValue::UNDEFINED, &unset_impl.into_js_value())?
        .dyn_into::<Function>()?;

    let scope = Object::new();
    let snapshot_state = state.clone();
    let get_snapshot =
        Closure::wrap(Box::new(move || snapshot_state.snapshot.borrow().clone())
            as Box<dyn FnMut() -> JsValue>);
    set(&scope, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_state = state.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> Function {
        let mut listeners = subscribe_state.listeners.borrow_mut();
        if !listeners
            .iter()
            .any(|registered| Object::is(registered, &listener))
        {
            listeners.push(listener.clone());
        }
        drop(listeners);
        let cleanup_state = subscribe_state.clone();
        Closure::wrap(Box::new(move || {
            cleanup_state
                .listeners
                .borrow_mut()
                .retain(|registered| !Object::is(registered, &listener));
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut(Function) -> Function>);
    set(&scope, "subscribe", &subscribe.into_js_value())?;
    set(&scope, "set", set_spy.as_ref())?;
    set(&scope, "unset", unset_spy.as_ref())?;

    let handle = Object::new();
    set(&handle, "scope", scope.as_ref())?;
    set(&handle, "set", set_spy.as_ref())?;
    set(&handle, "unset", unset_spy.as_ref())?;
    let count_state = state.clone();
    let listener_count = Closure::wrap(
        Box::new(move || count_state.listeners.borrow().len()) as Box<dyn FnMut() -> usize>
    );
    set(&handle, "listenerCount", &listener_count.into_js_value())?;
    let publish_state = state;
    let publish = Closure::wrap(Box::new(move |patch: JsValue| -> Result<(), JsValue> {
        if !patch.is_object() || patch.is_null() {
            return Err(
                js_sys::TypeError::new("settings-scope publication must be an object").into(),
            );
        }
        let next = Object::assign(
            &Object::new(),
            &Object::from(publish_state.snapshot.borrow().clone()),
        );
        Object::assign(&next, &Object::from(patch));
        *publish_state.snapshot.borrow_mut() = next.into();
        let listeners = publish_state.listeners.borrow().clone();
        for listener in listeners {
            listener.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&handle, "publish", &publish.into_js_value())?;
    Ok(handle.into())
}

fn initial_snapshot() -> Result<Object, JsValue> {
    object(&[
        ("status", JsValue::from_str("loading")),
        ("value", JsValue::UNDEFINED),
        ("base", JsValue::UNDEFINED),
        ("user", JsValue::UNDEFINED),
        ("revision", JsValue::UNDEFINED),
        ("writable", JsValue::FALSE),
        ("mode", JsValue::from_str("host")),
    ])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(target, &JsValue::from_str(key), value).map(|_| ())
}
