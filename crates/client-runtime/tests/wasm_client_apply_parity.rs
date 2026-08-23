//! Live browser assembly and wire-sink parity for `applyClientRuntime`.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::apply_client_runtime;
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
fn response(value: JsValue) -> JsValue {
    let result = Object::new();
    set(&result, "ok", &JsValue::TRUE);
    set(&result, "value", &value);
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("fake"));
    set(&response, "result", &result);
    response.into()
}

fn api() -> JsValue {
    let api = Object::new();
    let sessions = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "items", &Array::new());
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "list", &list.into_js_value());
    let history = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "events", &Array::new());
        set(&value, "hasMore", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "history", &history.into_js_value());
    let create = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "sessionId", &JsValue::from_str("fk-new"));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "create", &create.into_js_value());
    set(&api, "sessions", &sessions);
    let subagents = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "entries", &Array::new());
        set(&value, "parentAvailable", &JsValue::FALSE);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&subagents, "list", &list.into_js_value());
    set(&api, "subagents", &subagents);
    let workspace = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "items", &Array::new());
        set(&value, "archivedSessionIds", &Array::new());
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&workspace, "list", &list.into_js_value());
    set(&api, "workspace", &workspace);
    api.into()
}

struct Bench {
    root: JsValue,
    services: Object,
    emissions: Rc<RefCell<Vec<Vec<JsValue>>>>,
    dispatches: Rc<RefCell<Vec<Vec<JsValue>>>>,
    sinks: Rc<RefCell<JsValue>>,
    cleanups: Rc<RefCell<Vec<Function>>>,
    stopped: Rc<RefCell<u64>>,
}

#[allow(clippy::too_many_lines)]
fn bench() -> Bench {
    let services = Object::new();
    let root = Object::new();
    let cleanups = Rc::new(RefCell::new(Vec::<Function>::new()));
    let cleanups_for_effect = cleanups.clone();
    let effect = Closure::wrap(Box::new(move |installer: Function, _label: JsValue| {
        let cleanup = installer.call0(&JsValue::UNDEFINED).unwrap();
        if let Ok(cleanup) = cleanup.dyn_into::<Function>() {
            cleanups_for_effect.borrow_mut().push(cleanup);
        }
        Function::new_no_args("")
    }) as Box<dyn FnMut(Function, JsValue) -> Function>);
    set(&root, "effect", &effect.into_js_value());
    let services_for_get = services.clone();
    let get_service = Closure::wrap(Box::new(move |name: String| {
        Reflect::get(&services_for_get, &JsValue::from_str(&name)).unwrap()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&root, "get", &get_service.into_js_value());
    let services_for_provide = services.clone();
    let provide = Closure::wrap(
        Box::new(move |name: String, value: JsValue, _meta: JsValue| {
            assert!(
                Reflect::set(&services_for_provide, &JsValue::from_str(&name), &value).unwrap()
            );
        }) as Box<dyn FnMut(String, JsValue, JsValue)>,
    );
    let reflect = Object::new();
    set(&reflect, "provide", &provide.into_js_value());
    set(&root, "reflect", &reflect);

    let emissions = Rc::new(RefCell::new(Vec::new()));
    let emissions_for_emit = emissions.clone();
    let emit = Closure::wrap(Box::new(move |event: String, args: JsValue| {
        emissions_for_emit
            .borrow_mut()
            .push(vec![JsValue::from_str(&event), args]);
    }) as Box<dyn FnMut(String, JsValue)>);
    set(&root, "emit", &emit.into_js_value());

    let symbol = Function::new_with_args("description", "return Symbol(description)")
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("Context.filter"))
        .unwrap();
    let constructor = Object::new();
    set(&constructor, "filter", &symbol);
    let base = Object::new();
    set(&base, "constructor", &constructor);
    set(
        &base,
        "extend",
        &Function::new_with_args(
            "extension",
            "Object.setPrototypeOf(extension, this); return extension",
        ),
    );
    let base_for_plugin = base;
    let plugin = Closure::wrap(Box::new(move |_plugin: JsValue| {
        let fiber = Object::new();
        set(&fiber, "ctx", &base_for_plugin);
        set(
            &fiber,
            "dispose",
            &Function::new_no_args("return Promise.resolve()"),
        );
        fiber
    }) as Box<dyn FnMut(JsValue) -> Object>);
    set(&root, "plugin", &plugin.into_js_value());

    let contexts = Object::new();
    let register_client =
        Closure::wrap(Box::new(move |_name: String, _descriptor: JsValue| {})
            as Box<dyn FnMut(String, JsValue)>);
    set(
        &contexts,
        "registerClient",
        &register_client.into_js_value(),
    );
    let typert = Object::new();
    set(&typert, "contexts", &contexts);
    set(&services, "typert", &typert);

    let dispatches = Rc::new(RefCell::new(Vec::new()));
    let dispatches_for_remote = dispatches.clone();
    let dispatch = Closure::wrap(Box::new(move |event: JsValue, args: JsValue| {
        let mut row = vec![event];
        row.extend(Array::from(&args).iter());
        dispatches_for_remote.borrow_mut().push(row);
    }) as Box<dyn FnMut(JsValue, JsValue)>);
    let remote = Object::new();
    set(&remote, "$dispatch", &dispatch.into_js_value());
    let commands = Object::new();
    set(
        &commands,
        "execute",
        &Function::new_with_args("sessionId, line", "return Promise.resolve({ ok: true })"),
    );
    set(&remote, "commands", &commands);
    set(&services, "remote", &remote);

    let sinks = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let sinks_for_start = sinks.clone();
    let stopped = Rc::new(RefCell::new(0));
    let stopped_for_loop = stopped.clone();
    let start = Closure::wrap(Box::new(move |installed: JsValue| {
        *sinks_for_start.borrow_mut() = installed;
        let loop_handle = Object::new();
        let stopped = stopped_for_loop.clone();
        let stop = Closure::wrap(Box::new(move || {
            *stopped.borrow_mut() += 1;
        }) as Box<dyn FnMut()>);
        set(&loop_handle, "stop", &stop.into_js_value());
        loop_handle
    }) as Box<dyn FnMut(JsValue) -> Object>);
    let connection = Object::new();
    set(&connection, "api", &api());
    set(&connection, "start", &start.into_js_value());
    set(&services, "connection", &connection);
    Bench {
        root: root.into(),
        services,
        emissions,
        dispatches,
        sinks,
        cleanups,
        stopped,
    }
}

async fn flush() {
    for _ in 0..4 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

fn envelope(frame_type: &str) -> (Object, Object) {
    let payload = Object::new();
    set(&payload, "type", &JsValue::from_str(frame_type));
    let envelope = Object::new();
    set(&envelope, "rpcId", &JsValue::from_str("frame"));
    set(&envelope, "payload", &payload);
    (envelope, payload)
}

#[wasm_bindgen_test(async)]
async fn apply_mounts_services_and_fans_host_frames_into_both_rust_managers() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    for service in [
        "slots",
        "conversationEvents",
        "conversationViews",
        "sessions",
        "workspaces",
    ] {
        assert!(!get(&bench.services, service).is_undefined());
    }
    assert!(!bench.sinks.borrow().is_undefined());
    let sinks = bench.sinks.borrow().clone();
    let (session_envelope, session_payload) = envelope("host/session-added");
    set(&session_payload, "sessionId", &JsValue::from_str("s-new"));
    set(&session_payload, "blank", &JsValue::TRUE);
    get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&sinks, &session_envelope)
        .unwrap();
    let (workspace_envelope, workspace_payload) = envelope("host/workspace-changed");
    let workspace = Object::new();
    set(&workspace, "workspaceId", &JsValue::from_str("w-new"));
    set(&workspace, "path", &JsValue::from_str("/w/new"));
    set(&workspace, "title", &JsValue::from_str("new"));
    set(&workspace, "sessionIds", &Array::new());
    set(
        &workspace,
        "createdAt",
        &JsValue::from_str("2026-01-01T00:00:00.000Z"),
    );
    set(
        &workspace,
        "updatedAt",
        &JsValue::from_str("2026-01-01T00:00:00.000Z"),
    );
    set(&workspace_payload, "workspace", &workspace);
    get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&sinks, &workspace_envelope)
        .unwrap();
    flush().await;
    let sessions = get(&runtime, "sessions");
    let session_list = get(&sessions, "list");
    let session_snapshot = get(&session_list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session_list)
        .unwrap();
    assert_eq!(
        get(&session_snapshot, "ids")
            .dyn_into::<Array>()
            .unwrap()
            .get(0)
            .as_string()
            .as_deref(),
        Some("s-new")
    );
    let workspaces = get(&runtime, "workspaces");
    let workspace_list = get(&workspaces, "list");
    let workspace_snapshot = get(&workspace_list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&workspace_list)
        .unwrap();
    assert_eq!(
        get(
            &get(&workspace_snapshot, "items")
                .dyn_into::<Array>()
                .unwrap()
                .get(0),
            "workspaceId",
        )
        .as_string()
        .as_deref(),
        Some("w-new")
    );
}

#[wasm_bindgen_test]
fn wire_bridge_forwards_only_remote_events_and_emits_each_connection_reset() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    let sinks = get(&runtime, "sinks");
    let host = get(&sinks, "onHostEnvelope")
        .dyn_into::<Function>()
        .unwrap();
    let (remote_envelope, remote_payload) = envelope("host/remote-event");
    set(
        &remote_payload,
        "event",
        &JsValue::from_str("settings/document-updated"),
    );
    let args = Array::new();
    args.push(&JsValue::from_str("llm"));
    args.push(&JsValue::from_f64(7.0));
    set(&remote_payload, "args", &args);
    host.call1(&sinks, &remote_envelope).unwrap();
    let (ordinary, ordinary_payload) = envelope("host/session-status");
    set(&ordinary_payload, "sessionId", &JsValue::from_str("ghost"));
    set(&ordinary_payload, "running", &JsValue::FALSE);
    host.call1(&sinks, &ordinary).unwrap();
    assert_eq!(bench.dispatches.borrow().len(), 1);
    assert_eq!(
        bench.dispatches.borrow()[0][0].as_string().as_deref(),
        Some("settings/document-updated")
    );
    let connected = get(&sinks, "onConnected").dyn_into::<Function>().unwrap();
    connected.call1(&sinks, &Object::new()).unwrap();
    connected.call1(&sinks, &Object::new()).unwrap();
    assert_eq!(
        bench
            .emissions
            .borrow()
            .iter()
            .filter(|row| row[0].as_string().as_deref() == Some("connection/reset"))
            .count(),
        2
    );
}

#[wasm_bindgen_test]
fn registry_changes_reach_resident_session_rebuild_and_cleanup_stops_loop_once() {
    let bench = bench();
    let runtime = apply_client_runtime(bench.root.clone()).unwrap();
    let sessions = get(&runtime, "sessions");
    let rebuilds = Rc::new(RefCell::new(0));
    let observed = rebuilds.clone();
    let original = get(&sessions, "rebuildConversationRegistry")
        .dyn_into::<Function>()
        .unwrap();
    let sessions_for_call = sessions.clone();
    let wrapper = Closure::wrap(Box::new(move || {
        *observed.borrow_mut() += 1;
        original.call0(&sessions_for_call).unwrap();
    }) as Box<dyn FnMut()>);
    assert!(
        Reflect::set(
            &sessions,
            &JsValue::from_str("rebuildConversationRegistry"),
            &wrapper.into_js_value()
        )
        .unwrap()
    );
    let definition = Object::new();
    set(&definition, "kind", &JsValue::from_str("probe"));
    set(
        &definition,
        "match",
        &Function::new_with_args("event", "return null"),
    );
    set(
        &definition,
        "start",
        &Function::new_with_args("context, match, reader", "return null"),
    );
    set(
        &definition,
        "update",
        &Function::new_with_args("context, match", "return context.state"),
    );
    get(&runtime, "conversationEvents")
        .dyn_into::<Object>()
        .ok()
        .map(|events| {
            get(&events, "register")
                .dyn_into::<Function>()
                .unwrap()
                .call1(&events, &definition)
                .unwrap()
        });
    assert_eq!(*rebuilds.borrow(), 1);
    for cleanup in bench.cleanups.borrow().iter().rev() {
        cleanup.call0(&JsValue::UNDEFINED).unwrap();
    }
    assert_eq!(*bench.stopped.borrow(), 1);
}
