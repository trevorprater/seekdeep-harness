//! Live JavaScript `SessionManager` list, Session identity, catalog, and cancellation parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::WasmSessionManager;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[allow(clippy::needless_pass_by_value)]
fn ok(value: JsValue) -> JsValue {
    let result = Object::new();
    set(&result, "ok", &JsValue::TRUE);
    set(&result, "value", &value);
    result.into()
}

fn response(value: JsValue) -> JsValue {
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("fake"));
    set(&response, "result", &ok(value));
    response.into()
}

struct Fake {
    api: JsValue,
    remote: JsValue,
    search_signals: Rc<RefCell<Vec<JsValue>>>,
}

fn fake() -> Fake {
    let api = Object::new();
    let sessions = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let item = Object::new();
        set(&item, "sessionId", &JsValue::from_str("s1"));
        set(&item, "updatedAt", &JsValue::from_f64(1.0));
        set(&item, "running", &JsValue::FALSE);
        set(&item, "blank", &JsValue::FALSE);
        let items = Array::new();
        items.push(&item);
        let value = Object::new();
        set(&value, "items", &items);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "list", &list.into_js_value());
    let search_signals = Rc::new(RefCell::new(Vec::new()));
    let observed = search_signals.clone();
    let search = Closure::wrap(Box::new(move |_payload: JsValue, signal: JsValue| {
        observed.borrow_mut().push(signal);
        let value = Object::new();
        set(&value, "items", &Array::new());
        set(&value, "hasMore", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    set(&sessions, "search", &search.into_js_value());
    let create = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "sessionId", &JsValue::from_str("created"));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "create", &create.into_js_value());
    let fork = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "sessionId", &JsValue::from_str("forked"));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "fork", &fork.into_js_value());
    let history = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "events", &Array::new());
        set(&value, "hasMore", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "history", &history.into_js_value());
    set(&api, "sessions", &sessions);
    let subagents = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let child = Object::new();
        set(&child, "kind", &JsValue::from_str("child"));
        set(&child, "id", &JsValue::from_str("child"));
        set(&child, "mode", &JsValue::from_str("continuable"));
        set(&child, "label", &JsValue::from_str("worker"));
        set(&child, "activity", &JsValue::from_str("inactive"));
        set(&child, "hasChildren", &JsValue::FALSE);
        let entries = Array::new();
        entries.push(&child);
        let value = Object::new();
        set(&value, "entries", &entries);
        set(&value, "parentAvailable", &JsValue::TRUE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&subagents, "list", &list.into_js_value());
    let history = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "events", &Array::new());
        set(&value, "hasMore", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&subagents, "history", &history.into_js_value());
    set(&api, "subagents", &subagents);
    let remote = Object::new();
    let commands = Object::new();
    set(
        &commands,
        "execute",
        &Function::new_with_args("sessionId, line", "return Promise.resolve({ ok: true })"),
    );
    set(&remote, "commands", &commands);
    Fake {
        api: api.into(),
        remote: remote.into(),
        search_signals,
    }
}

#[wasm_bindgen_test(async)]
async fn list_refresh_session_wrapper_and_raw_frame_routes_keep_cached_shapes() {
    let fake = fake();
    let manager = WasmSessionManager::new(fake.api, fake.remote, None, JsValue::UNDEFINED).unwrap();
    let refresh = manager.refresh_list();
    assert_eq!(
        get(&manager.get_list_snapshot().unwrap(), "state")
            .as_string()
            .as_deref(),
        Some("loading")
    );
    JsFuture::from(refresh).await.unwrap();
    let snapshot = manager.get_list_snapshot().unwrap();
    assert_eq!(
        get(&snapshot, "phase").as_string().as_deref(),
        Some("ready")
    );
    assert_eq!(
        get(&snapshot, "items")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );
    assert!(Object::is(&snapshot, &manager.get_list_snapshot().unwrap()));
    let first = manager.get("s1".to_owned()).unwrap();
    let second = manager.get("s1".to_owned()).unwrap();
    assert!(Object::is(&first, &second));

    let host = Object::new();
    set(&host, "type", &JsValue::from_str("host/session-status"));
    set(&host, "sessionId", &JsValue::from_str("s1"));
    set(&host, "running", &JsValue::TRUE);
    let envelope = Object::new();
    set(&envelope, "payload", &host);
    manager.handle_host_envelope(envelope.into()).unwrap();
    let items = get(&manager.get_list_snapshot().unwrap(), "items")
        .dyn_into::<Array>()
        .unwrap();
    assert_eq!(get(&items.get(0), "running").as_bool(), Some(true));

    let projection = Object::new();
    set(
        &projection,
        "type",
        &JsValue::from_str("session/projection"),
    );
    set(&projection, "sessionId", &JsValue::from_str("s1"));
    set(&projection, "key", &JsValue::from_str("title"));
    set(&projection, "value", &JsValue::from_str("Projected"));
    set(&projection, "seq", &JsValue::from_f64(2.0));
    let envelope = Object::new();
    set(&envelope, "rpcId", &JsValue::from_str("projection"));
    set(&envelope, "payload", &projection);
    manager.handle_mux_envelope(envelope.into()).unwrap();
    let items = get(&manager.get_list_snapshot().unwrap(), "items")
        .dyn_into::<Array>()
        .unwrap();
    assert_eq!(
        get(&items.get(0), "title").as_string().as_deref(),
        Some("Projected")
    );
}

#[wasm_bindgen_test(async)]
async fn catalog_selection_and_search_preserve_address_and_abort_signal_identity() {
    let fake = fake();
    let signals = fake.search_signals.clone();
    let manager = WasmSessionManager::new(fake.api, fake.remote, None, JsValue::UNDEFINED).unwrap();
    JsFuture::from(manager.refresh_subagents("parent".to_owned()))
        .await
        .unwrap();
    let address = Object::new();
    set(&address, "parentSessionId", &JsValue::from_str("parent"));
    set(&address, "childSessionId", &JsValue::from_str("child"));
    set(&address, "mode", &JsValue::from_str("continuable"));
    manager.select_subagent(address.into()).unwrap();
    let snapshot = manager.get_list_snapshot().unwrap();
    assert_eq!(
        get(&get(&snapshot, "currentAddress"), "childSessionId")
            .as_string()
            .as_deref(),
        Some("child")
    );
    let signal: JsValue = Object::new().into();
    let search = manager.search("needle".to_owned(), signal.clone()).unwrap();
    JsFuture::from(search).await.unwrap();
    assert!(Object::is(&signals.borrow()[0], &signal));
}
