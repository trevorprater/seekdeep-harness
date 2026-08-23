//! Live JavaScript `WorkspaceRuntime` list, Promise, action, and Host-frame parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::{WasmSessionRuntime, WasmWorkspaceRuntime};
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
    let error = Object::new();
    set(&error, "code", &JsValue::from_str(code));
    set(&error, "message", &JsValue::from_str(message));
    set(&error, "details", &Object::new());
    let result = Object::new();
    set(&result, "ok", &JsValue::FALSE);
    set(&result, "error", &error);
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("fake"));
    set(&response, "result", &result);
    response.into()
}

fn workspace(id: &str, session_ids: &[&str], created_at: &str) -> JsValue {
    let value = Object::new();
    set(&value, "workspaceId", &JsValue::from_str(id));
    set(&value, "path", &JsValue::from_str(&format!("/w/{id}")));
    set(&value, "title", &JsValue::from_str(id));
    let sessions = Array::new();
    for session_id in session_ids {
        sessions.push(&JsValue::from_str(session_id));
    }
    set(&value, "sessionIds", &sessions);
    set(&value, "createdAt", &JsValue::from_str(created_at));
    set(&value, "updatedAt", &JsValue::from_str(created_at));
    value.into()
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

struct FakeApi {
    api: JsValue,
    remote: JsValue,
    seen_signal: Rc<RefCell<Option<JsValue>>>,
}

#[allow(clippy::too_many_lines)]
fn api() -> FakeApi {
    let api = Object::new();
    let sessions = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let item = Object::new();
        set(&item, "sessionId", &JsValue::from_str("s1"));
        set(&item, "updatedAt", &JsValue::from_f64(1_800_000_000_000.0));
        set(&item, "running", &JsValue::FALSE);
        set(&item, "blank", &JsValue::TRUE);
        set(&item, "cwd", &JsValue::from_str("/w/alpha"));
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
        if get(&payload, "workspaceId").as_string().as_deref() == Some("fail") {
            Promise::resolve(&failure_response("internal", "create failed"))
        } else {
            let value = Object::new();
            set(&value, "sessionId", &JsValue::from_str("fresh"));
            Promise::resolve(&response(value.into()))
        }
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

    let workspace_api = Object::new();
    let list = Closure::wrap(Box::new(move |_payload: JsValue| {
        let items = Array::new();
        items.push(&workspace("alpha", &["s1"], "2026-01-01T00:00:00.000Z"));
        items.push(&workspace("beta", &[], "2026-01-02T00:00:00.000Z"));
        let value = Object::new();
        set(&value, "items", &items);
        set(&value, "archivedSessionIds", &Array::new());
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&workspace_api, "list", &list.into_js_value());
    let create = Closure::wrap(Box::new(move |input: JsValue| {
        if get(&input, "path").as_string().as_deref() == Some("/bad") {
            Promise::resolve(&failure_response("workspace-invalid-path", "missing"))
        } else {
            let value = Object::new();
            set(
                &value,
                "workspace",
                &workspace("created", &[], "2026-01-03T00:00:00.000Z"),
            );
            set(&value, "created", &JsValue::TRUE);
            Promise::resolve(&response(value.into()))
        }
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&workspace_api, "create", &create.into_js_value());
    let archive = Closure::wrap(Box::new(move |payload: JsValue| {
        let archived = Array::new();
        archived.push(&get(&payload, "sessionId"));
        let value = Object::new();
        set(&value, "archivedSessionIds", &archived);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&workspace_api, "archiveSession", &archive.into_js_value());
    set(&api, "workspace", &workspace_api);

    let seen_signal = Rc::new(RefCell::new(None));
    let host = Object::new();
    let seen = seen_signal.clone();
    let list_directory = Closure::wrap(Box::new(move |payload: JsValue, signal: JsValue| {
        *seen.borrow_mut() = Some(signal);
        if get(&payload, "path").as_string().as_deref() == Some("/bad") {
            Promise::resolve(&failure_response("directory-unreadable", "denied"))
        } else {
            let value = Object::new();
            set(&value, "path", &JsValue::from_str("/"));
            set(&value, "home", &JsValue::from_str("/"));
            set(&value, "crumbs", &Array::new());
            set(&value, "entries", &Array::new());
            set(&value, "truncated", &JsValue::FALSE);
            Promise::resolve(&response(value.into()))
        }
    }) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
    set(&host, "listDirectory", &list_directory.into_js_value());
    let picker = Closure::wrap(Box::new(move |_payload: JsValue| {
        let value = Object::new();
        set(&value, "path", &JsValue::NULL);
        Promise::resolve(&response(value.into()))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    set(&host, "pickDirectory", &picker.into_js_value());
    set(&api, "host", &host);

    let remote = Object::new();
    let commands = Object::new();
    set(
        &commands,
        "execute",
        &Function::new_with_args("sessionId, line", "return Promise.resolve({ ok: true })"),
    );
    set(&remote, "commands", &commands);
    FakeApi {
        api: api.into(),
        remote: remote.into(),
        seen_signal,
    }
}

#[wasm_bindgen_test(async)]
async fn list_projection_keeps_workspace_item_identity_and_shares_refresh_promise() {
    let fake = api();
    let root = root_context();
    let sessions = WasmSessionRuntime::new(root.clone(), fake.api.clone(), fake.remote).unwrap();
    let workspaces = WasmWorkspaceRuntime::new(root, fake.api, &sessions).unwrap();
    let first = workspaces.refresh();
    let second = workspaces.refresh();
    assert!(Object::is(&first, &second));
    JsFuture::from(first).await.unwrap();
    JsFuture::from(sessions.refresh()).await.unwrap();
    let list = workspaces.list();
    let get_snapshot = get(&list, "getSnapshot").dyn_into::<Function>().unwrap();
    let snapshot = get_snapshot.call0(&list).unwrap();
    assert_eq!(get(&snapshot, "baselinesReady").as_bool(), Some(true));
    assert_eq!(
        get(&snapshot, "recentWorkspaceId").as_string().as_deref(),
        Some("alpha")
    );
    let items = get(&snapshot, "items");
    sessions.note_agent_preset("s1".to_owned(), "minimal".to_owned());
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    let projected = get_snapshot.call0(&list).unwrap();
    assert!(Object::is(&items, &get(&projected, "items")));
}

#[wasm_bindgen_test(async)]
async fn connect_workspace_shares_promise_and_create_errors_keep_source_class() {
    let fake = api();
    let root = root_context();
    let sessions = WasmSessionRuntime::new(root.clone(), fake.api.clone(), fake.remote).unwrap();
    let workspaces = WasmWorkspaceRuntime::new(root, fake.api, &sessions).unwrap();
    JsFuture::from(workspaces.refresh()).await.unwrap();
    JsFuture::from(sessions.refresh()).await.unwrap();
    let reused_first = workspaces.connect_workspace("alpha".to_owned());
    let reused_second = workspaces.connect_workspace("alpha".to_owned());
    assert!(!Object::is(&reused_first, &reused_second));
    assert_eq!(
        JsFuture::from(reused_first)
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("s1")
    );
    assert_eq!(
        JsFuture::from(reused_second)
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("s1")
    );
    let first = workspaces.connect_workspace("beta".to_owned());
    let second = workspaces.connect_workspace("beta".to_owned());
    assert!(Object::is(&first, &second));
    assert_eq!(
        JsFuture::from(first).await.unwrap().as_string().as_deref(),
        Some("fresh")
    );
    let input = Object::new();
    set(&input, "path", &JsValue::from_str("/bad"));
    let error = JsFuture::from(workspaces.create(input.into()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(
        get(&error, "name").as_string().as_deref(),
        Some("WorkspaceCreateError")
    );
    assert_eq!(
        get(&get(&error, "rpcError"), "code").as_string().as_deref(),
        Some("workspace-invalid-path")
    );
    let input = Object::new();
    set(&input, "path", &JsValue::from_str("/good"));
    let created = JsFuture::from(workspaces.create(input.into()).unwrap())
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    let list = workspaces.list();
    let snapshot = get(&list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&list)
        .unwrap();
    let first = get(&snapshot, "items").dyn_into::<Array>().unwrap().get(0);
    assert!(Object::is(&created, &first));
}

#[wasm_bindgen_test(async)]
async fn directory_browse_forwards_exact_signal_and_preserves_business_error() {
    let fake = api();
    let root = root_context();
    let sessions = WasmSessionRuntime::new(root.clone(), fake.api.clone(), fake.remote).unwrap();
    let workspaces = WasmWorkspaceRuntime::new(root, fake.api, &sessions).unwrap();
    let signal: JsValue = Object::new().into();
    JsFuture::from(workspaces.list_directory(Some("/".to_owned()), signal.clone()))
        .await
        .unwrap();
    assert!(Object::is(
        fake.seen_signal.borrow().as_ref().unwrap(),
        &signal
    ));
    let error = JsFuture::from(workspaces.list_directory(Some("/bad".to_owned()), signal))
        .await
        .unwrap_err();
    assert_eq!(
        get(&error, "name").as_string().as_deref(),
        Some("DirectoryBrowseError")
    );
    assert_eq!(
        get(&get(&error, "rpcError"), "code").as_string().as_deref(),
        Some("directory-unreadable")
    );
}

#[wasm_bindgen_test(async)]
async fn archived_host_frame_clears_current_session_through_shared_rust_core() {
    let fake = api();
    let root = root_context();
    let sessions = WasmSessionRuntime::new(root.clone(), fake.api.clone(), fake.remote).unwrap();
    let workspaces = WasmWorkspaceRuntime::new(root, fake.api, &sessions).unwrap();
    JsFuture::from(workspaces.refresh()).await.unwrap();
    JsFuture::from(sessions.refresh()).await.unwrap();
    sessions.open("s1".to_owned()).unwrap();
    let payload = Object::new();
    set(
        &payload,
        "type",
        &JsValue::from_str("host/archived-sessions-changed"),
    );
    let archived = Array::new();
    archived.push(&JsValue::from_str("s1"));
    set(&payload, "archivedSessionIds", &archived);
    let envelope = Object::new();
    set(&envelope, "rpcId", &JsValue::from_str("archive"));
    set(&envelope, "payload", &payload);
    workspaces.handle_host_envelope(envelope.into()).unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    let session_list = sessions.list();
    let snapshot = get(&session_list, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&session_list)
        .unwrap();
    assert!(get(&snapshot, "current").is_undefined());
}
