//! Live WASM controller, Binder lifecycle, and Client plugin assembly parity.

#![cfg(target_arch = "wasm32")]

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_settings::{
    ClientSettingsDescribeValue, ClientSettingsNamespaceView, WasmSettingsScopeController,
    apply_client_ui_settings, bind_settings_scope, configure_client_ui_settings, settings_inject,
};
use seekdeep_schemastery::Schema;
use serde::Serialize;
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test(async)]
async fn controller_preserves_snapshot_identity_freezing_and_revision_fenced_set_unset() {
    let describe_calls = Rc::new(Cell::new(0_usize));
    let describe_count = describe_calls.clone();
    let describe = Closure::wrap(Box::new(move |_request: JsValue| -> Promise {
        describe_count.set(describe_count.get() + 1);
        Promise::resolve(&success(&ClientSettingsDescribeValue {
            writable: true,
            namespaces: vec![view(json!({"preference": "dark"}), 1.0)],
        }))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let requests = Rc::new(RefCell::new(Vec::<JsValue>::new()));
    let request_capture = requests.clone();
    let mutation = Rc::new(Cell::new(0_usize));
    let mutation_count = mutation.clone();
    let mutate = Closure::wrap(Box::new(move |request: JsValue| -> Promise {
        request_capture.borrow_mut().push(request);
        let index = mutation_count.get();
        mutation_count.set(index + 1);
        let response = if index == 0 {
            view(json!({"preference": "light"}), 2.0)
        } else {
            view(json!({"preference": "system"}), 3.0)
        };
        Promise::resolve(&success(&response))
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let api = api(describe, mutate);
    let scope = WasmSettingsScopeController::new(api, spec(), None).unwrap();
    let first = scope.get_snapshot().unwrap();
    let second = scope.get_snapshot().unwrap();
    assert!(Object::is(&first, &second));
    assert!(Object::is_frozen(&Object::from(first.clone())));
    assert_eq!(string(&first, "status"), "loading");
    assert_eq!(string(&first, "mode"), "host");

    let notifications = Rc::new(Cell::new(0_usize));
    let notification_count = notifications.clone();
    let listener = Closure::wrap(Box::new(move || {
        notification_count.set(notification_count.get() + 1);
    }) as Box<dyn FnMut()>);
    let stop = scope.subscribe(listener.into_js_value().unchecked_into());
    JsFuture::from(scope.load()).await.unwrap();
    assert_eq!(describe_calls.get(), 1);
    let loaded = scope.get_snapshot().unwrap();
    assert!(!Object::is(&first, &loaded));
    assert_eq!(string(&loaded, "status"), "ready");
    assert_eq!(integer(&loaded, "revision"), 1);
    assert_eq!(string(&get(&loaded, "value"), "preference"), "dark");

    JsFuture::from(
        scope
            .set("preference".to_owned(), JsValue::from_str("light"))
            .unwrap(),
    )
    .await
    .unwrap();
    JsFuture::from(scope.unset("preference".to_owned()))
        .await
        .unwrap();
    assert_eq!(notifications.get(), 3);
    assert_eq!(requests.borrow().len(), 2);
    assert_eq!(integer(&requests.borrow()[0], "expectedRevision"), 1);
    assert_eq!(integer(&requests.borrow()[1], "expectedRevision"), 2);
    let first_ops = Array::from(&get(&requests.borrow()[0], "ops"));
    assert_eq!(string(&first_ops.get(0), "op"), "set");
    assert_eq!(string(&first_ops.get(0), "value"), "light");
    let second_ops = Array::from(&get(&requests.borrow()[1], "ops"));
    assert_eq!(string(&second_ops.get(0), "op"), "unset");
    let final_snapshot = scope.get_snapshot().unwrap();
    assert_eq!(integer(&final_snapshot, "revision"), 3);
    assert_eq!(
        string(&get(&final_snapshot, "value"), "preference"),
        "system"
    );
    stop.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(scope.dispose()).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn binder_subscribes_before_initial_read_converges_and_retires_with_caller_effect() {
    let first_resolve = Rc::new(RefCell::new(None::<Function>));
    let resolve_capture = first_resolve.clone();
    let first = Promise::new(&mut move |resolve, _reject| {
        *resolve_capture.borrow_mut() = Some(resolve);
    });
    let describe_calls = Rc::new(Cell::new(0_usize));
    let describe_count = describe_calls.clone();
    let first_response = first.clone();
    let describe = Closure::wrap(Box::new(move |_request: JsValue| -> Promise {
        let index = describe_count.get();
        describe_count.set(index + 1);
        match index {
            0 => first_response.clone(),
            1 => Promise::resolve(&success(&ClientSettingsDescribeValue {
                writable: true,
                namespaces: vec![view(json!({"preference": "light"}), 2.0)],
            })),
            _ => Promise::resolve(&success(&ClientSettingsDescribeValue {
                writable: true,
                namespaces: vec![view(json!({"preference": "system"}), 3.0)],
            })),
        }
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let mutate = Closure::wrap(Box::new(move |_request: JsValue| -> Promise {
        Promise::resolve(&rejected())
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let api_face = api(describe, mutate);
    let harness = CallerHarness::new(&api_face, true);
    let scope = bind_settings_scope(harness.ctx.clone().into(), spec()).unwrap();
    wait_until(|| describe_calls.get() == 1).await;
    harness.dispatch_remote("unrelated");
    harness.dispatch_remote("ui-test");
    harness.dispatch_reset();
    first_resolve
        .borrow()
        .as_ref()
        .unwrap()
        .call1(
            &JsValue::UNDEFINED,
            &success(&ClientSettingsDescribeValue {
                writable: true,
                namespaces: vec![view(json!({"preference": "dark"}), 1.0)],
            }),
        )
        .unwrap();
    wait_until(|| describe_calls.get() == 3).await;
    wait_until(|| {
        get(&call(&scope, "getSnapshot", &[]), "revision")
            .as_f64()
            .is_some_and(|revision| (revision - 3.0).abs() < f64::EPSILON)
    })
    .await;
    let snapshot = call(&scope, "getSnapshot", &[]);
    assert_eq!(string(&get(&snapshot, "value"), "preference"), "system");
    assert_eq!(
        harness.effect_label.borrow().as_deref(),
        Some("ui-settings: ui-test settings scope")
    );
    let dispose = harness.effect_disposer.borrow().clone().unwrap();
    let disposed = dispose.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(&disposed)).await.unwrap();
    assert!(!harness.remote_active.get());
    assert!(!harness.reset_active.get());
    harness.dispatch_remote("ui-test");
    yield_microtask().await;
    assert_eq!(describe_calls.get(), 3);

    let remote_describe_calls = Rc::new(Cell::new(0_usize));
    let remote_count = remote_describe_calls.clone();
    let describe = Closure::wrap(Box::new(move |_request: JsValue| -> Promise {
        remote_count.set(remote_count.get() + 1);
        Promise::resolve(&rejected())
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let mutate = Closure::wrap(Box::new(move |_request: JsValue| -> Promise {
        Promise::resolve(&rejected())
    }) as Box<dyn FnMut(JsValue) -> Promise>);
    let remote_api = api(describe, mutate);
    let remote = CallerHarness::new(&remote_api, false);
    let memory_scope = bind_settings_scope(remote.ctx.clone().into(), spec()).unwrap();
    yield_microtask().await;
    let memory = call(&memory_scope, "getSnapshot", &[]);
    assert_eq!(string(&memory, "status"), "unavailable");
    assert_eq!(string(&memory, "mode"), "memory");
    assert!(!get(&memory, "writable").as_bool().unwrap());
    assert_eq!(remote_describe_calls.get(), 0);
    let dispose = remote.effect_disposer.borrow().clone().unwrap();
    let disposed = dispose.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(&disposed)).await.unwrap();
}

#[wasm_bindgen_test]
fn apply_constructs_the_configured_binder_and_declares_no_inject_dependencies() {
    let constructor = Function::new_with_args(
        "ctx",
        "this.marker = 'binder'; ctx.provide('settingsScope', this);",
    );
    configure_client_ui_settings(constructor);
    let provided = Rc::new(RefCell::new(None::<JsValue>));
    let provided_capture = provided.clone();
    let ctx = Object::new();
    let provide = Closure::wrap(Box::new(move |name: String, value: JsValue| {
        if name == "settingsScope" {
            *provided_capture.borrow_mut() = Some(value);
        }
    }) as Box<dyn FnMut(String, JsValue)>);
    set(&ctx, "provide", &provide.into_js_value());
    apply_client_ui_settings(ctx.into()).unwrap();
    assert_eq!(
        string(&provided.borrow().clone().unwrap(), "marker"),
        "binder"
    );
    assert_eq!(settings_inject().length(), 0);
}

struct CallerHarness {
    ctx: Object,
    remote_listener: Rc<RefCell<Option<Function>>>,
    reset_listener: Rc<RefCell<Option<Function>>>,
    remote_active: Rc<Cell<bool>>,
    reset_active: Rc<Cell<bool>>,
    effect_disposer: Rc<RefCell<Option<Function>>>,
    effect_label: Rc<RefCell<Option<String>>>,
}

impl CallerHarness {
    fn new(api: &JsValue, loopback: bool) -> Self {
        let remote_listener = Rc::new(RefCell::new(None::<Function>));
        let reset_listener = Rc::new(RefCell::new(None::<Function>));
        let remote_active = Rc::new(Cell::new(true));
        let reset_active = Rc::new(Cell::new(true));
        let effect_disposer = Rc::new(RefCell::new(None::<Function>));
        let effect_label = Rc::new(RefCell::new(None::<String>));

        let remote = Object::new();
        let remote_listener_capture = remote_listener.clone();
        let remote_active_disposer = remote_active.clone();
        let on_remote = Closure::wrap(Box::new(
            move |event: String, listener: Function| -> Function {
                assert_eq!(event, "settings/document-updated");
                *remote_listener_capture.borrow_mut() = Some(listener);
                let active = remote_active_disposer.clone();
                Closure::wrap(Box::new(move || active.set(false)) as Box<dyn FnMut()>)
                    .into_js_value()
                    .unchecked_into()
            },
        ) as Box<dyn FnMut(String, Function) -> Function>);
        set(&remote, "$on", &on_remote.into_js_value());

        let connection = Object::new();
        set(&connection, "api", api);
        set(&connection, "isLoopback", &JsValue::from_bool(loopback));
        let services = Object::new();
        set(&services, "connection", &connection);
        set(&services, "remote", &remote);

        let ctx = Object::new();
        let service_table = services;
        let get_service = Closure::wrap(Box::new(move |name: String| -> JsValue {
            Reflect::get(&service_table, &JsValue::from_str(&name)).unwrap()
        }) as Box<dyn FnMut(String) -> JsValue>);
        set(&ctx, "get", &get_service.into_js_value());

        let reset_listener_capture = reset_listener.clone();
        let reset_active_disposer = reset_active.clone();
        let on = Closure::wrap(
            Box::new(move |event: String, listener: Function| -> Function {
                assert_eq!(event, "connection/reset");
                *reset_listener_capture.borrow_mut() = Some(listener);
                let active = reset_active_disposer.clone();
                Closure::wrap(Box::new(move || active.set(false)) as Box<dyn FnMut()>)
                    .into_js_value()
                    .unchecked_into()
            }) as Box<dyn FnMut(String, Function) -> Function>,
        );
        set(&ctx, "on", &on.into_js_value());

        let disposer_capture = effect_disposer.clone();
        let label_capture = effect_label.clone();
        let effect = Closure::wrap(Box::new(move |installer: Function, label: String| {
            *label_capture.borrow_mut() = Some(label);
            *disposer_capture.borrow_mut() = installer
                .call0(&JsValue::UNDEFINED)
                .unwrap()
                .dyn_into::<Function>()
                .ok();
        }) as Box<dyn FnMut(Function, String)>);
        set(&ctx, "effect", &effect.into_js_value());

        Self {
            ctx,
            remote_listener,
            reset_listener,
            remote_active,
            reset_active,
            effect_disposer,
            effect_label,
        }
    }

    fn dispatch_remote(&self, namespace: &str) {
        if self.remote_active.get() {
            self.remote_listener
                .borrow()
                .as_ref()
                .unwrap()
                .call2(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(namespace),
                    &JsValue::from_f64(0.0),
                )
                .unwrap();
        }
    }

    fn dispatch_reset(&self) {
        if self.reset_active.get() {
            self.reset_listener
                .borrow()
                .as_ref()
                .unwrap()
                .call0(&JsValue::UNDEFINED)
                .unwrap();
        }
    }
}

fn api(
    describe: Closure<dyn FnMut(JsValue) -> Promise>,
    mutate: Closure<dyn FnMut(JsValue) -> Promise>,
) -> JsValue {
    let settings = Object::new();
    set(&settings, "describe", &describe.into_js_value());
    set(&settings, "mutate", &mutate.into_js_value());
    let api = Object::new();
    set(&api, "settings", &settings);
    api.into()
}

fn envelope() -> Value {
    Schema::object([(
        "preference",
        Schema::union([
            Schema::constant("light"),
            Schema::constant("dark"),
            Schema::constant("system"),
        ])
        .with_default("system"),
    )])
    .to_json()
}

fn view(value: Value, revision: f64) -> ClientSettingsNamespaceView {
    ClientSettingsNamespaceView {
        ns: "ui-test".to_owned(),
        schema: envelope(),
        value,
        base: None,
        user: None,
        revision,
    }
}

fn spec() -> JsValue {
    let spec = Object::new();
    set(&spec, "namespace", &JsValue::from_str("ui-test"));
    spec.into()
}

fn success(value: &impl Serialize) -> JsValue {
    let result = Object::new();
    set(&result, "ok", &JsValue::TRUE);
    set(
        &result,
        "value",
        &value
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .unwrap(),
    );
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("scope-1"));
    set(&response, "result", &result);
    response.into()
}

fn rejected() -> JsValue {
    let result = Object::new();
    set(&result, "ok", &JsValue::FALSE);
    let response = Object::new();
    set(&response, "rpcId", &JsValue::from_str("scope-rejected"));
    set(&response, "result", &result);
    response.into()
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        yield_microtask().await;
    }
    assert!(predicate(), "condition did not become true");
}

async fn yield_microtask() {
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
}

fn call(value: &JsValue, method: &str, arguments: &[JsValue]) -> JsValue {
    let function = get(value, method).dyn_into::<Function>().unwrap();
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args).unwrap()
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn set(value: &Object, key: &str, item: &JsValue) {
    Reflect::set(value, &JsValue::from_str(key), item).unwrap();
}

fn string(value: &JsValue, key: &str) -> String {
    let actual = get(value, key);
    actual
        .as_string()
        .unwrap_or_else(|| panic!("property {key:?} is not a string: {actual:?}"))
}

fn integer(value: &JsValue, key: &str) -> u64 {
    let actual = get(value, key);
    actual
        .as_f64()
        .unwrap_or_else(|| panic!("property {key:?} is not a number: {actual:?}"))
        .to_string()
        .parse()
        .unwrap_or_else(|_| panic!("property {key:?} is not an unsigned integer: {actual:?}"))
}
