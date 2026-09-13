//! Live JavaScript `SessionRuntime` list, scope, binding, staging, and provide parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::WasmSessionRuntime;
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

fn failure_response(code: &str, message: &str) -> JsValue {
    let details = Object::new();
    let error = Object::new();
    set(&error, "code", &JsValue::from_str(code));
    set(&error, "message", &JsValue::from_str(message));
    set(&error, "details", &details);
    let result = Object::new();
    set(&result, "ok", &JsValue::FALSE);
    set(&result, "error", &error);
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("fake"));
    set(&response, "result", &result);
    response.into()
}

fn root_context() -> JsValue {
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
    let root = Object::new();
    let pruned = Array::new();
    set(&root, "pruned", &pruned);
    let slots = Object::new();
    let pruned_for_call = pruned;
    let prune = Closure::wrap(Box::new(move |session_id: JsValue| {
        pruned_for_call.push(&session_id);
    }) as Box<dyn FnMut(JsValue)>);
    set(&slots, "pruneStoreScope", &prune.into_js_value());
    let slots_for_get = slots;
    let get_service = Closure::wrap(Box::new(move |name: String| {
        if name == "slots" {
            JsValue::from(slots_for_get.clone())
        } else {
            JsValue::UNDEFINED
        }
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&root, "get", &get_service.into_js_value());
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
    root.into()
}

fn api() -> (JsValue, JsValue) {
    let api = Object::new();
    let sessions = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let item = Object::new();
        set(&item, "sessionId", &JsValue::from_str("s1"));
        set(&item, "updatedAt", &JsValue::from_f64(1.0));
        set(&item, "running", &JsValue::FALSE);
        set(&item, "blank", &JsValue::FALSE);
        set(&item, "cwd", &JsValue::from_str("/workspace/project"));
        let items = Array::new();
        items.push(&item);
        let value = Object::new();
        set(&value, "items", &items);
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
    let create = Closure::wrap(Box::new(move |payload: JsValue| {
        let session_id = get(&payload, "sessionId").as_string();
        if session_id.as_deref() == Some("reserved") {
            Promise::resolve(&failure_response("denied", "not allowed"))
        } else {
            let value = Object::new();
            set(&value, "sessionId", &JsValue::from_str("created"));
            Promise::resolve(&response(value.into()))
        }
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "create", &create.into_js_value());
    let fork = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "sessionId", &JsValue::from_str("forked"));
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&sessions, "fork", &fork.into_js_value());
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
    let remote = Object::new();
    let commands = Object::new();
    set(
        &commands,
        "execute",
        &Function::new_with_args("sessionId, line", "return Promise.resolve({ ok: true })"),
    );
    set(&remote, "commands", &commands);
    (api.into(), remote.into())
}

#[wasm_bindgen_test(async)]
async fn service_projects_list_mints_stable_scope_and_stages_open() {
    let (api, remote) = api();
    let runtime = WasmSessionRuntime::new(root_context(), api, remote).unwrap();
    JsFuture::from(runtime.refresh()).await.unwrap();
    let list = runtime.list();
    let get_snapshot = get(&list, "getSnapshot").dyn_into::<Function>().unwrap();
    let snapshot = get_snapshot.call0(&list).unwrap();
    let ids = get(&snapshot, "ids").dyn_into::<Array>().unwrap();
    assert_eq!(ids.get(0).as_string().as_deref(), Some("s1"));
    assert_eq!(
        get(&get(&snapshot, "byId"), "s1")
            .dyn_into::<Object>()
            .ok()
            .and_then(|row| get(&row, "displayTitle").as_string())
            .as_deref(),
        Some("project")
    );
    let first = runtime.binding("s1".to_owned()).unwrap();
    let second = runtime.binding("s1".to_owned()).unwrap();
    assert!(Object::is(&first, &second));
    let context = get(&first, "ctx");
    assert_eq!(runtime.scope_of(context.clone()).as_deref(), Some("s1"));
    assert!(Object::is(&runtime.scope("s1".to_owned()), &context));
    runtime.open("s1".to_owned()).unwrap();
    let selected = get_snapshot.call0(&list).unwrap();
    assert_eq!(get(&selected, "current").as_string().as_deref(), Some("s1"));
    let current = runtime.current_provide_info();
    let current_snapshot = get(&current, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&current)
        .unwrap();
    assert_eq!(
        get(&current_snapshot, "sessionId").as_string().as_deref(),
        Some("s1")
    );
}

#[wasm_bindgen_test(async)]
async fn javascript_provider_rebuilds_current_bundle_under_stable_selection() {
    let (api, remote) = api();
    let runtime = WasmSessionRuntime::new(root_context(), api, remote).unwrap();
    JsFuture::from(runtime.refresh()).await.unwrap();
    runtime.open("s1".to_owned()).unwrap();
    let before_source = runtime.current_provide_info();
    let before = get(&before_source, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&before_source)
        .unwrap();
    let descriptor = Object::new();
    let hooks = Array::new();
    hooks.push(&JsValue::from_str("extra"));
    set(&descriptor, "hooks", &hooks);
    let props = Array::new();
    props.push(&JsValue::from_str("marker"));
    set(&descriptor, "props", &props);
    let source: JsValue = Object::new().into();
    let source_for_resolve = source.clone();
    let resolve = Closure::wrap(Box::new(move |_binding: JsValue| {
        let hooks = Object::new();
        set(&hooks, "extra", &source_for_resolve);
        let props = Object::new();
        set(&props, "marker", &JsValue::from_f64(7.0));
        let value = Object::new();
        set(&value, "hooks", &hooks);
        set(&value, "props", &props);
        value
    }) as Box<dyn FnMut(JsValue) -> Object>);
    set(&descriptor, "resolve", &resolve.into_js_value());
    let dispose = runtime.provide(descriptor.into()).unwrap();
    let added = get(&before_source, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&before_source)
        .unwrap();
    assert!(!Object::is(&before, &added));
    assert!(Object::is(&get(&get(&added, "hooks"), "extra"), &source));
    assert_eq!(get(&get(&added, "props"), "marker").as_f64(), Some(7.0));
    dispose.call0(&JsValue::UNDEFINED).unwrap();
}

#[wasm_bindgen_test(async)]
async fn browser_contract_uses_options_objects_structured_errors_and_exact_row_presence() {
    let (api, remote) = api();
    let runtime = WasmSessionRuntime::new(root_context(), api, remote).unwrap();
    JsFuture::from(runtime.refresh()).await.unwrap();
    assert_eq!(runtime.search_result_limit(), 100);
    let list = runtime.list();
    let snapshot = get(&list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&list)
        .unwrap();
    let row = get(&get(&snapshot, "byId"), "s1");
    for absent in [
        "title",
        "agentPreset",
        "parentId",
        "origin",
        "completed",
        "pendingInteraction",
        "projectionValues",
    ] {
        assert!(!Reflect::has(&row, &JsValue::from_str(absent)).unwrap());
    }
    assert!(
        runtime
            .subagent_address("s1".to_owned())
            .unwrap()
            .is_undefined()
    );

    let created = JsFuture::from(runtime.create(JsValue::UNDEFINED).unwrap())
        .await
        .unwrap();
    assert_eq!(created.as_string().as_deref(), Some("created"));
    let fork_options = Object::new();
    set(&fork_options, "sessionId", &JsValue::from_str("s1"));
    set(&fork_options, "atSeq", &JsValue::from_f64(4.9));
    let forked = JsFuture::from(runtime.fork(fork_options.into()).unwrap())
        .await
        .unwrap();
    assert_eq!(forked.as_string().as_deref(), Some("forked"));
    let invalid_fork = Object::new();
    set(&invalid_fork, "sessionId", &JsValue::from_str("s1"));
    set(&invalid_fork, "atSeq", &JsValue::from_f64(-1.0));
    let error = JsFuture::from(runtime.fork(invalid_fork.into()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(
        get(&error, "name").as_string().as_deref(),
        Some("SessionForkError")
    );
    assert_eq!(
        get(&get(&error, "rpcError"), "code").as_string().as_deref(),
        Some("bad-request")
    );

    let create_options = Object::new();
    set(&create_options, "sessionId", &JsValue::from_str("reserved"));
    let error = JsFuture::from(runtime.create(create_options.into()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(
        get(&error, "name").as_string().as_deref(),
        Some("SessionCreateError")
    );
    assert_eq!(
        get(&error, "requestedSessionId").as_string().as_deref(),
        Some("reserved")
    );
    assert_eq!(
        get(&get(&error, "rpcError"), "code").as_string().as_deref(),
        Some("denied")
    );
}

#[wasm_bindgen_test(async)]
async fn off_stage_removal_prunes_browser_faces_and_slot_store_scope() {
    let (api, remote) = api();
    let root = root_context();
    let runtime = WasmSessionRuntime::new(root.clone(), api, remote).unwrap();
    JsFuture::from(runtime.refresh()).await.unwrap();
    assert!(!runtime.binding("s1".to_owned()).unwrap().is_undefined());
    let payload = Object::new();
    set(&payload, "type", &JsValue::from_str("host/session-removed"));
    set(&payload, "sessionId", &JsValue::from_str("s1"));
    let envelope = Object::new();
    set(&envelope, "rpcId", &JsValue::from_str("remove"));
    set(&envelope, "payload", &payload);
    runtime.handle_host_envelope(envelope.into()).unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert!(runtime.binding("s1".to_owned()).unwrap().is_undefined());
    let pruned = get(&root, "pruned").dyn_into::<Array>().unwrap();
    assert_eq!(pruned.length(), 1);
    assert_eq!(pruned.get(0).as_string().as_deref(), Some("s1"));
}
