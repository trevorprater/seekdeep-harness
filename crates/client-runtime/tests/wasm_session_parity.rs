//! Live JavaScript Session construction, Promise, snapshot, mux, and operation parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::WasmClientSession;
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

struct BrowserFake {
    api: JsValue,
    remote: JsValue,
    history_calls: Rc<Cell<u64>>,
    prompt_calls: Rc<Cell<u64>>,
    responses: Rc<std::cell::RefCell<Vec<JsValue>>>,
}

fn fake() -> BrowserFake {
    let history_calls = Rc::new(Cell::new(0));
    let prompt_calls = Rc::new(Cell::new(0));
    let responses = Rc::new(std::cell::RefCell::new(Vec::new()));
    let api = Object::new();
    let sessions = Object::new();
    let observed = history_calls.clone();
    let history = Closure::wrap(Box::new(move |_payload: JsValue| {
        observed.set(observed.get() + 1);
        let page = Object::new();
        set(&page, "events", &Array::new());
        set(&page, "hasMore", &JsValue::FALSE);
        Promise::resolve(&response(page.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "history", &history.into_js_value());
    let observed = prompt_calls.clone();
    let prompt = Closure::wrap(Box::new(move |_payload: JsValue| {
        observed.set(observed.get() + 1);
        Promise::resolve(&response({
            let value = Object::new();
            set(&value, "accepted", &JsValue::TRUE);
            value.into()
        }))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "prompt", &prompt.into_js_value());
    for method in ["cancel", "updateQueue"] {
        let callback = Closure::wrap(Box::new(move |_payload: JsValue| {
            Promise::resolve(&response({
                let value = Object::new();
                set(&value, "accepted", &JsValue::TRUE);
                value.into()
            }))
        }) as Box<dyn FnMut(JsValue) -> Promise>);
        set(&sessions, method, &callback.into_js_value());
    }
    let rename = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "title", &JsValue::from_str("Renamed"));
        set(&value, "seq", &JsValue::from_f64(9.0));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "rename", &rename.into_js_value());
    let attachment = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        let attachment = Object::new();
        set(&attachment, "attachmentId", &JsValue::from_str("a"));
        set(&value, "attachment", &attachment);
        set(&value, "data", &JsValue::from_str("AAE="));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "attachment", &attachment.into_js_value());
    set(&api, "sessions", &sessions);

    let subagents = Object::new();
    for method in ["history", "prompt", "interrupt"] {
        let method_name = method.to_owned();
        let callback = Closure::wrap(Box::new(move |_payload: JsValue| {
            if method_name == "history" {
                let page = Object::new();
                set(&page, "events", &Array::new());
                set(&page, "hasMore", &JsValue::FALSE);
                Promise::resolve(&response(page.into()))
            } else {
                Promise::resolve(&response(Object::new().into()))
            }
        }) as Box<dyn FnMut(JsValue) -> Promise>);
        set(&subagents, method, &callback.into_js_value());
    }
    set(&api, "subagents", &subagents);
    let observed = responses.clone();
    let respond = Closure::wrap(Box::new(move |message: JsValue| {
        observed.borrow_mut().push(message);
        Promise::resolve(&response(Object::new().into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&api, "respond", &respond.into_js_value());

    let remote = Object::new();
    let commands = Object::new();
    let execute = Closure::wrap(Box::new(move |_session: String, _line: String| {
        Promise::resolve(&ok(Object::new().into()))
    }) as Box<dyn FnMut(String, String) -> Promise>);
    set(&commands, "execute", &execute.into_js_value());
    set(&remote, "commands", &commands);
    BrowserFake {
        api: api.into(),
        remote: remote.into(),
        history_calls,
        prompt_calls,
        responses,
    }
}

#[wasm_bindgen_test(async)]
async fn open_promise_and_snapshot_identity_follow_the_browser_contract() {
    let fake = fake();
    let session =
        WasmClientSession::new("s1".to_owned(), fake.api, fake.remote, JsValue::UNDEFINED).unwrap();
    let cold = session.get_snapshot().unwrap();
    assert_eq!(get(&cold, "openState").as_string().as_deref(), Some("cold"));
    let first = session.open();
    let second = session.open();
    assert!(Object::is(&first, &second));
    assert_eq!(
        get(&session.get_snapshot().unwrap(), "openState")
            .as_string()
            .as_deref(),
        Some("loading")
    );
    JsFuture::from(first).await.unwrap();
    let opened = session.get_snapshot().unwrap();
    assert_eq!(
        get(&opened, "openState").as_string().as_deref(),
        Some("open")
    );
    assert!(Object::is(&opened, &session.get_snapshot().unwrap()));
    assert_eq!(fake.history_calls.get(), 1);
    session.bind_scope(Object::new().into()).unwrap();
    assert!(session.bind_scope(Object::new().into()).is_err());
    session.unbind_scope();
    session.bind_scope(Object::new().into()).unwrap();

    let ticks = Rc::new(Cell::new(0));
    let observed = ticks.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    let dispose = session.subscribe(listener.into_js_value().unchecked_into());
    session.handle_running(true);
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
    assert_eq!(ticks.get(), 1);
    assert_eq!(
        get(&session.get_snapshot().unwrap(), "composerPhase")
            .as_string()
            .as_deref(),
        Some("active")
    );
    dispose.call0(&JsValue::UNDEFINED).unwrap();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // One live flow checks prompt, wait, response, queue, and reset shapes.
async fn prompt_pending_queue_and_response_envelope_keep_live_javascript_shapes() {
    let fake = fake();
    let session =
        WasmClientSession::new("s1".to_owned(), fake.api, fake.remote, JsValue::UNDEFINED).unwrap();
    let content = Array::new();
    let text = Object::new();
    set(&text, "type", &JsValue::from_str("text"));
    set(&text, "text", &JsValue::from_str("hello"));
    content.push(&text);
    let prompt = session.prompt(content, "queue".to_owned()).unwrap();
    assert_eq!(
        get(&session.get_snapshot().unwrap(), "composerPhase")
            .as_string()
            .as_deref(),
        Some("engaging")
    );
    let result = JsFuture::from(prompt).await.unwrap();
    assert_eq!(get(&result, "ok").as_bool(), Some(true));
    assert_eq!(fake.prompt_calls.get(), 1);
    let attachment = JsFuture::from(session.read_attachment("a".to_owned()))
        .await
        .unwrap();
    assert!(get(&get(&attachment, "value"), "data").is_instance_of::<js_sys::Uint8Array>());

    let approval = Object::new();
    set(&approval, "type", &JsValue::from_str("approval/requested"));
    set(&approval, "sessionId", &JsValue::from_str("s1"));
    set(&approval, "approvalId", &JsValue::from_str("a1"));
    session
        .handle_mux_envelope("rpc-a".to_owned(), approval.into())
        .unwrap();
    let snapshot = session.get_snapshot().unwrap();
    let pending = get(&snapshot, "pending").dyn_into::<Array>().unwrap();
    assert_eq!(pending.length(), 1);
    let respond = get(&pending.get(0), "respond")
        .dyn_into::<Function>()
        .unwrap();
    let promise = respond
        .call1(&pending.get(0), &Object::new())
        .unwrap()
        .dyn_into::<Promise>()
        .unwrap();
    JsFuture::from(promise).await.unwrap();
    assert_eq!(
        get(&fake.responses.borrow()[0], "rpcId")
            .as_string()
            .as_deref(),
        Some("rpc-a")
    );

    let resolved = Object::new();
    set(&resolved, "type", &JsValue::from_str("approval/resolved"));
    set(&resolved, "approvalId", &JsValue::from_str("a1"));
    session
        .handle_mux_envelope("resolved".to_owned(), resolved.into())
        .unwrap();
    assert_eq!(
        get(&session.get_snapshot().unwrap(), "pending")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        0
    );

    let queue = Object::new();
    set(&queue, "type", &JsValue::from_str("session/queue"));
    let items = Array::new();
    let item = Object::new();
    set(&item, "id", &JsValue::from_str("item"));
    set(&item, "placement", &JsValue::from_str("queued"));
    let message = Object::new();
    set(&message, "id", &JsValue::from_str("message"));
    set(&message, "content", &{
        let content = Array::new();
        content.push(&text);
        content.into()
    });
    set(&item, "message", &message);
    items.push(&item);
    set(&queue, "items", &items);
    session
        .handle_mux_envelope("queue".to_owned(), queue.into())
        .unwrap();
    let queued = get(&session.get_snapshot().unwrap(), "queue")
        .dyn_into::<Array>()
        .unwrap();
    assert_eq!(
        get(&queued.get(0), "preview").as_string().as_deref(),
        Some("hello")
    );
    let subscribed = Object::new();
    set(
        &subscribed,
        "type",
        &JsValue::from_str("session/subscribed"),
    );
    set(&subscribed, "lastSeq", &JsValue::from_f64(0.0));
    session
        .handle_mux_envelope("subscribed".to_owned(), subscribed.into())
        .unwrap();
    assert_eq!(
        get(&session.get_snapshot().unwrap(), "queue")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        0
    );
}
