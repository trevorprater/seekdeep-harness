//! Live browser Slot Service ownership and Host-face parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_runtime::WasmClientSlotRegistry;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = get(value, method).dyn_into::<Function>()?;
    let values = Array::new();
    for arg in args {
        values.push(arg);
    }
    method.apply(value, &values)
}

fn caller(name: &str) -> JsValue {
    let caller = Object::new();
    let fiber = Object::new();
    set(&fiber, "name", &JsValue::from_str(name));
    set(&caller, "fiber", &fiber);
    let effect = Function::new_with_args("installer, _label", "return installer()");
    set(&caller, "effect", &effect);
    caller.into()
}

fn declaration(kind: &str, scope: &str) -> Object {
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str(kind));
    set(&value, "scope", &JsValue::from_str(scope));
    value
}

fn root_options(children: &[(&str, &str, &str)]) -> Object {
    let options = Object::new();
    set(&options, "name", &JsValue::from_str("root"));
    let table = Object::new();
    for (name, kind, scope) in children {
        set(&table, name, &declaration(kind, scope));
    }
    if !children.is_empty() {
        set(&options, "children", &table);
    }
    options
}

fn options(name: &str) -> Object {
    let value = Object::new();
    set(&value, "name", &JsValue::from_str(name));
    value
}

#[wasm_bindgen_test]
fn caller_owned_registration_stamps_registrant_bridges_events_and_cascades() {
    let changed = Rc::new(RefCell::new(Vec::<String>::new()));
    let observed = changed.clone();
    let on_changed = Closure::wrap(Box::new(move |key: String| {
        observed.borrow_mut().push(key);
    }) as Box<dyn FnMut(String)>);
    let registry = WasmClientSlotRegistry::new(Some(on_changed.into_js_value().unchecked_into()));
    let face = registry.face_for(caller("plugin-a")).unwrap();
    let dispose_root = call(
        &face,
        "register",
        &[
            root_options(&[("t.host", "single", "root")]).into(),
            JsValue::from_str("frame"),
        ],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    let dispose_entry = call(
        &face,
        "register",
        &[options("t.host").into(), JsValue::from_str("entry")],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    let entries = registry.entries("t.host".to_owned());
    assert_eq!(entries.length(), 1);
    assert_eq!(
        get(&entries.get(0), "registrant").as_string().as_deref(),
        Some("plugin-a")
    );
    assert_eq!(changed.borrow().as_slice(), ["root", "t.host", "t.host"]);
    dispose_entry.call0(&JsValue::UNDEFINED).unwrap();
    dispose_entry.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(registry.entries("t.host".to_owned()).length(), 0);
    dispose_root.call0(&JsValue::UNDEFINED).unwrap();
    assert!(registry.spec("t.host".to_owned()).unwrap().is_undefined());
}

#[wasm_bindgen_test]
fn declaration_injection_waits_cleans_and_reactivates_through_caller_effects() {
    let registry = WasmClientSlotRegistry::new(None);
    let face = registry.face_for(caller("plugin-b")).unwrap();
    let setups = Rc::new(RefCell::new(0_usize));
    let cleanups = Rc::new(RefCell::new(0_usize));
    let setup_count = setups.clone();
    let cleanup_count = cleanups.clone();
    let callback = Closure::wrap(Box::new(move || -> Function {
        *setup_count.borrow_mut() += 1;
        let cleanup_count = cleanup_count.clone();
        Closure::wrap(Box::new(move || {
            *cleanup_count.borrow_mut() += 1;
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }) as Box<dyn FnMut() -> Function>);
    let dispose_injection = call(
        &face,
        "inject",
        &[JsValue::from_str("t.host"), callback.into_js_value()],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    assert_eq!(*setups.borrow(), 0);
    let dispose_root = call(
        &face,
        "register",
        &[
            root_options(&[("t.host", "single", "root")]).into(),
            JsValue::from_str("frame"),
        ],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    assert!(!registry.spec("t.host".to_owned()).unwrap().is_undefined());
    assert_eq!(*setups.borrow(), 1);
    dispose_root.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(*cleanups.borrow(), 1);
    call(
        &face,
        "register",
        &[
            root_options(&[("t.host", "single", "root")]).into(),
            JsValue::from_str("frame-2"),
        ],
    )
    .unwrap();
    assert_eq!(*setups.borrow(), 2);
    dispose_injection.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(*cleanups.borrow(), 2);
}

#[wasm_bindgen_test]
fn declaration_callback_can_reenter_the_shared_register_binding() {
    let registry = WasmClientSlotRegistry::new(None);
    let face = registry.face_for(caller("plugin-reentrant")).unwrap();
    let registration_face = face.clone();
    let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call(
            &registration_face,
            "register",
            &[
                options("t.host").into(),
                JsValue::from_str("injected-entry"),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dispose_injection = call(
        &face,
        "inject",
        &[JsValue::from_str("t.host"), callback.into_js_value()],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    let dispose_root = call(
        &face,
        "register",
        &[
            root_options(&[("t.host", "single", "root")]).into(),
            JsValue::from_str("frame"),
        ],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    assert_eq!(registry.entries("t.host".to_owned()).length(), 1);
    dispose_injection.call0(&JsValue::UNDEFINED).unwrap();
    dispose_root.call0(&JsValue::UNDEFINED).unwrap();
}

#[wasm_bindgen_test(async)]
async fn caller_face_exposes_batched_ledger_subscriptions() {
    let registry = WasmClientSlotRegistry::new(None);
    let face = registry.face_for(caller("plugin-subscriber")).unwrap();
    let notifications = Rc::new(RefCell::new(0_usize));
    let observed = notifications.clone();
    let listener = Closure::wrap(Box::new(move || {
        *observed.borrow_mut() += 1;
    }) as Box<dyn FnMut()>);
    let unsubscribe = call(
        &face,
        "subscribe",
        &[JsValue::from_str("t.host"), listener.into_js_value()],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();

    let dispose_root = call(
        &face,
        "register",
        &[
            root_options(&[("t.host", "single", "root")]).into(),
            JsValue::from_str("frame"),
        ],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert_eq!(*notifications.borrow(), 1);

    unsubscribe.call0(&JsValue::UNDEFINED).unwrap();
    let dispose_entry = call(
        &face,
        "register",
        &[options("t.host").into(), JsValue::from_str("entry")],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert_eq!(*notifications.borrow(), 1);
    dispose_entry.call0(&JsValue::UNDEFINED).unwrap();
    dispose_root.call0(&JsValue::UNDEFINED).unwrap();
}

#[wasm_bindgen_test]
fn renderer_host_resolves_session_store_and_prunes_persistence() {
    let registry = WasmClientSlotRegistry::new(None);
    let sessions = Object::new();
    set(&sessions, "list", &Object::new());
    set(&sessions, "provideInfo", &Object::new());
    registry.install_sessions(sessions.into());
    let workspaces = Object::new();
    set(&workspaces, "list", &Object::new());
    registry.install_workspaces(workspaces.into());
    let face = registry.face_for(caller("layout")).unwrap();
    let captured_host = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let captured = captured_host.clone();
    let renderer = Object::new();
    let render_root = Closure::wrap(Box::new(move |host: JsValue, owner: JsValue| -> JsValue {
        *captured.borrow_mut() = host;
        owner
    }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
    set(&renderer, "renderRoot", &render_root.into_js_value());
    call(&face, "install", &[renderer.into()]).unwrap();

    let dispose_root = call(
        &face,
        "register",
        &[
            root_options(&[("t.panel", "single", "session")]).into(),
            JsValue::from_str("frame"),
        ],
    )
    .unwrap()
    .dyn_into::<Function>()
    .unwrap();
    let cleared = Rc::new(RefCell::new(0_usize));
    let created_keys = Rc::new(RefCell::new(Vec::<Option<String>>::new()));
    let store = Object::new();
    let cleared_instances = cleared.clone();
    let observed_keys = created_keys.clone();
    let create = Closure::wrap(Box::new(move |scope: JsValue| -> JsValue {
        observed_keys.borrow_mut().push(scope.as_string());
        let instance = Object::new();
        let clear_count = cleared_instances.clone();
        let clear = Closure::wrap(Box::new(move || {
            *clear_count.borrow_mut() += 1;
        }) as Box<dyn FnMut()>);
        set(&instance, "clearPersisted", &clear.into_js_value());
        let snapshot =
            Closure::wrap(Box::new(|| JsValue::UNDEFINED) as Box<dyn FnMut() -> JsValue>);
        set(&instance, "getSnapshot", &snapshot.into_js_value());
        let subscribe = Closure::wrap(Box::new(|_listener: Function| -> Function {
            Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_into()
        }) as Box<dyn FnMut(Function) -> Function>);
        set(&instance, "subscribe", &subscribe.into_js_value());
        instance.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(&store, "create", &create.into_js_value());
    let panel = options("t.panel");
    set(&panel, "store", &store);
    call(
        &face,
        "register",
        &[panel.into(), JsValue::from_str("panel")],
    )
    .unwrap();

    let owner = Object::new();
    set(&owner, "marker", &JsValue::from_str("owner"));
    let root_result = call(
        &face,
        "renderSlot",
        &[JsValue::from_str("root"), owner.clone().into()],
    )
    .unwrap();
    assert!(Object::is(&root_result, &owner));
    let host = captured_host.borrow().clone();
    let entry = registry.entries("t.panel".to_owned()).get(0);
    let first = call(&host, "storeOf", &[entry.clone(), JsValue::from_str("s1")]).unwrap();
    let second = call(&host, "storeOf", &[entry, JsValue::from_str("s1")]).unwrap();
    assert!(Object::is(&first, &second));
    assert_eq!(created_keys.borrow().as_slice(), [Some("s1".to_owned())]);
    registry.prune_store_scope("s1".to_owned());
    assert_eq!(*cleared.borrow(), 1);
    dispose_root.call0(&JsValue::UNDEFINED).unwrap();
}
