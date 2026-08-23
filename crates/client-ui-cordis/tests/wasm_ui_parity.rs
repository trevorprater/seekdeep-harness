//! Live browser execution for Rust-owned Cordis component and plugin assembly.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Function, Object, Promise, Reflect};
use seekdeep_client_ui_cordis::{client_ui_cordis_plugin, cordis_ui_components};
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisActiveRun, DynamicCordisInventoryPackage, DynamicCordisInventoryRow,
};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

#[wasm_bindgen_test]
fn action_component_executes_in_react_and_preserves_visible_attributes() {
    let react = Object::new();
    let create = Closure::wrap(Box::new(|kind: JsValue, props: JsValue| -> JsValue {
        let element = Object::new();
        Reflect::set(&element, &JsValue::from_str("kind"), &kind).unwrap();
        Reflect::set(&element, &JsValue::from_str("props"), &props).unwrap();
        element.into()
    }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
    set(&react, "createElement", &create.into_js_value());

    let primitives = Object::new();
    for name in [
        "StateDot",
        "IconTrashOutline16",
        "IconStopFill16",
        "IconInspectOutline12",
    ] {
        set(&primitives, name, &JsValue::from_str(name));
    }
    let components = cordis_ui_components(react.into(), primitives.into()).unwrap();
    let action = get(&components, "CordisActionRow")
        .dyn_into::<Function>()
        .unwrap();
    let props = Object::new();
    set(&props, "callId", &JsValue::from_str("call-1"));
    set(&props, "toolName", &JsValue::from_str("cordis_stop"));
    set(
        &props,
        "block",
        &serde_wasm_bindgen::to_value(&serde_json::json!({
            "kind": "tool-result",
            "seq": 2,
            "call": {"name": "cordis_stop", "argsRaw": "{\"pluginId\":\"clock-1\"}"},
            "content": [{"type": "text", "text": "Stopped clock-1."}],
            "isError": false
        }))
        .unwrap(),
    );
    let t = Closure::wrap(
        Box::new(|key: String| -> String { format!("translated:{key}") })
            as Box<dyn FnMut(String) -> String>,
    );
    set(&props, "t", &t.into_js_value());
    let rendered = action.call1(&JsValue::UNDEFINED, &props).unwrap();
    let root_props = get(&rendered, "props");
    assert_eq!(
        get(&root_props, "data-tool").as_string().unwrap(),
        "cordis_stop"
    );
    assert_eq!(get(&root_props, "data-state").as_string().unwrap(), "ok");
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn define_run_and_panel_components_execute_against_framework_hook_shapes() {
    let react = fake_react();
    let primitives = fake_primitives();
    let components = cordis_ui_components(react, primitives).unwrap();
    let inventory_row = DynamicCordisInventoryRow {
        plugin_id: CordisDynamicPluginId::new("clock-1"),
        agent_id: SessionId::new("session-1"),
        packages: vec![DynamicCordisInventoryPackage {
            package_id: CordisDynamicPackageId::new("pkg-1"),
            name: "Clock".to_owned(),
            purpose: "show time".to_owned(),
            has_host_half: true,
            has_client_half: false,
        }],
        current_package_id: Some(CordisDynamicPackageId::new("pkg-1")),
        next_package_id: None,
        active_run: Some(DynamicCordisActiveRun {
            plugin_run_id: CordisDynamicPluginRunId::new("run-1"),
            package_id: CordisDynamicPackageId::new("pkg-1"),
        }),
        latest_run: None,
    };
    let inventory = Object::new();
    set(
        &inventory,
        "rows",
        &serde_wasm_bindgen::to_value(&vec![inventory_row]).unwrap(),
    );
    set(
        &inventory,
        "removed",
        &js_sys::Set::new(&JsValue::UNDEFINED),
    );
    set(&inventory, "read", &JsValue::TRUE);
    let loaded: JsValue = js_sys::Array::new().into();
    let empty_map: JsValue = js_sys::Map::new().into();

    let define_props = common_tool_props("call-define", "cordis_define");
    set(
        &define_props,
        "block",
        &serde_wasm_bindgen::to_value(&serde_json::json!({
            "callId": "call-define",
            "name": "cordis_define",
            "argsRaw": "{\"name\":\"Clock\",\"code\":{\"host\":\"HOST\"}}"
        }))
        .unwrap(),
    );
    set(
        &define_props,
        "useInventory",
        &observable_hook(inventory.clone().into()),
    );
    set(&define_props, "useLoaded", &observable_hook(loaded.clone()));
    let define = get(&components, "CordisDefineRow")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &define_props)
        .unwrap();
    assert_eq!(
        get(&get(&define, "props"), "data-tool")
            .as_string()
            .unwrap(),
        "cordis_define"
    );

    let run_props = common_tool_props("call-run", "cordis_run");
    set(
        &run_props,
        "block",
        &serde_wasm_bindgen::to_value(&serde_json::json!({
            "kind": "tool-result",
            "seq": 9,
            "call": {"name": "cordis_run", "argsRaw": "{\"pluginId\":\"clock-1\",\"packageId\":\"pkg-1\",\"mode\":\"run\"}"},
            "content": [{"type": "text", "text": "running"}],
            "isError": false,
            "meta": {"pluginId": "clock-1", "packageId": "pkg-1", "pluginRunId": "run-1"}
        }))
        .unwrap(),
    );
    set(
        &run_props,
        "useInventory",
        &observable_hook(inventory.clone().into()),
    );
    set(&run_props, "useLoaded", &observable_hook(loaded.clone()));
    set(
        &run_props,
        "useRunCards",
        &observable_hook(empty_map.clone()),
    );
    set(
        &run_props,
        "useActiveRuns",
        &observable_hook(empty_map.clone()),
    );
    let observe = Closure::wrap(Box::new(|_pointer: JsValue| {}) as Box<dyn FnMut(JsValue)>);
    set(&run_props, "onObserveRunCard", &observe.into_js_value());
    let render_slot = Closure::wrap(Box::new(
        |_name: JsValue, _owner: JsValue, _options: JsValue| -> JsValue {
            JsValue::from_str("business")
        },
    )
        as Box<dyn FnMut(JsValue, JsValue, JsValue) -> JsValue>);
    set(&run_props, "renderSlot", &render_slot.into_js_value());
    let run = get(&components, "CordisRunRow")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &run_props)
        .unwrap();
    assert_eq!(
        get(&get(&run, "props"), "data-cordis-status")
            .as_string()
            .unwrap(),
        "running"
    );

    let panel_props = Object::new();
    set(
        &panel_props,
        "useInventory",
        &observable_hook(inventory.into()),
    );
    for hook in ["useActiveRuns", "useRunErrors", "useRenderFailures"] {
        set(&panel_props, hook, &observable_hook(empty_map.clone()));
    }
    set(&panel_props, "useLoaded", &observable_hook(loaded));
    let use_sessions = Closure::wrap(Box::new(move |selector: Function| -> JsValue {
        let state = Object::new();
        set(&state, "current", &JsValue::from_str("session-1"));
        selector.call1(&JsValue::UNDEFINED, &state).unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&panel_props, "useSessions", &use_sessions.into_js_value());
    for callback in [
        "onApprove",
        "onDecline",
        "onRun",
        "onStop",
        "onRemove",
        "onRefresh",
    ] {
        set(&panel_props, callback, &noop_function());
    }
    set(&panel_props, "t", &translation_function());
    let panel = get(&components, "CordisPanel")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &panel_props)
        .unwrap();
    assert_eq!(
        get(&get(&panel, "props"), "className").as_string().unwrap(),
        "seekdeep-cordis-panel-layer seekdeep-cordis-panel-rail"
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn plugin_apply_registers_complete_browser_assembly_and_reads_inventory() {
    let injected = Rc::new(RefCell::new(Vec::<String>::new()));
    let registered_entries = Rc::new(RefCell::new(0_usize));
    let registered_sources = Rc::new(RefCell::new(0_usize));
    let remote_events = Rc::new(RefCell::new(Vec::<String>::new()));
    let inventory_reads = Rc::new(RefCell::new(0_usize));
    let reconciles = Rc::new(RefCell::new(0_usize));
    let inserted_styles = Rc::new(RefCell::new(0_usize));

    let document = Object::new();
    let create_element = Closure::wrap(Box::new(move |_tag: String| -> JsValue {
        let style = Object::new();
        let set_attribute = Closure::wrap(
            Box::new(|_name: String, _value: String| {}) as Box<dyn FnMut(String, String)>
        );
        set(&style, "setAttribute", &set_attribute.into_js_value());
        let remove = Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>);
        set(&style, "remove", &remove.into_js_value());
        style.into()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&document, "createElement", &create_element.into_js_value());
    let head = Object::new();
    let style_count = inserted_styles.clone();
    let append = Closure::wrap(Box::new(move |_style: JsValue| -> JsValue {
        *style_count.borrow_mut() += 1;
        JsValue::UNDEFINED
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(&head, "appendChild", &append.into_js_value());
    set(&document, "head", &head);
    let global = js_sys::global();
    set(&global, "document", &document);

    let slots = Object::new();
    let entry_count = registered_entries.clone();
    let register_entry = Closure::wrap(Box::new(
        move |_options: JsValue, _component: JsValue| -> Function {
            *entry_count.borrow_mut() += 1;
            disposer()
        },
    ) as Box<dyn FnMut(JsValue, JsValue) -> Function>);
    set(&slots, "register", &register_entry.into_js_value());
    let observed = injected.clone();
    let inject = Closure::wrap(Box::new(
        move |name: String, factory: Function| -> Result<Function, JsValue> {
            let index = observed.borrow().len();
            observed.borrow_mut().push(name);
            if index == 2 {
                factory.call1(&JsValue::UNDEFINED, &JsValue::from_str("session-1"))?;
            } else {
                factory.call0(&JsValue::UNDEFINED)?;
            }
            Ok(disposer())
        },
    )
        as Box<dyn FnMut(String, Function) -> Result<Function, JsValue>>);
    set(&slots, "inject", &inject.into_js_value());

    let locale = Object::new();
    let register = Closure::wrap(Box::new(
        |_namespace: String, _dictionaries: JsValue| -> Function { disposer() },
    ) as Box<dyn FnMut(String, JsValue) -> Function>);
    set(&locale, "register", &register.into_js_value());

    let input_triggers = Object::new();
    let source_count = registered_sources.clone();
    let register_source = Closure::wrap(Box::new(move |_source: JsValue| -> Function {
        *source_count.borrow_mut() += 1;
        disposer()
    }) as Box<dyn FnMut(JsValue) -> Function>);
    set(
        &input_triggers,
        "registerSource",
        &register_source.into_js_value(),
    );

    let remote = Object::new();
    let observed_events = remote_events.clone();
    let on_remote = Closure::wrap(
        Box::new(move |event: String, _listener: Function| -> Function {
            observed_events.borrow_mut().push(event);
            disposer()
        }) as Box<dyn FnMut(String, Function) -> Function>,
    );
    set(&remote, "$on", &on_remote.into_js_value());

    let namespace = Object::new();
    let reads = inventory_reads.clone();
    let inventory = Closure::wrap(Box::new(move || -> Promise {
        *reads.borrow_mut() += 1;
        let answer = Object::new();
        set(&answer, "ok", &JsValue::TRUE);
        set(&answer, "value", &js_sys::Array::new());
        let answer: JsValue = answer.into();
        Promise::resolve(&answer)
    }) as Box<dyn FnMut() -> Promise>);
    set(&namespace, "inventory", &inventory.into_js_value());

    let runner = Object::new();
    let get_snapshot = Closure::wrap(
        Box::new(|| -> JsValue { js_sys::Array::new().into() }) as Box<dyn FnMut() -> JsValue>
    );
    set(&runner, "getSnapshot", &get_snapshot.into_js_value());
    let subscribe_runner =
        Closure::wrap(Box::new(|_listener: Function| -> Function { disposer() })
            as Box<dyn FnMut(Function) -> Function>);
    set(&runner, "subscribe", &subscribe_runner.into_js_value());
    let empty_observable = Object::new();
    let empty_get = Closure::wrap(
        Box::new(|| -> JsValue { js_sys::Map::new().into() }) as Box<dyn FnMut() -> JsValue>
    );
    set(&empty_observable, "getSnapshot", &empty_get.into_js_value());
    let empty_subscribe = Closure::wrap(Box::new(|_listener: Function| -> Function { disposer() })
        as Box<dyn FnMut(Function) -> Function>);
    set(
        &empty_observable,
        "subscribe",
        &empty_subscribe.into_js_value(),
    );
    for name in ["activeRuns", "lastRunError", "renderFailures"] {
        set(&runner, name, &empty_observable);
    }
    for name in ["approve", "decline", "startUserRun"] {
        set(&runner, name, &noop_function());
    }
    let reconcile_count = reconciles.clone();
    let reconcile = Closure::wrap(Box::new(move |_rows: JsValue| {
        *reconcile_count.borrow_mut() += 1;
    }) as Box<dyn FnMut(JsValue)>);
    set(&runner, "reconcileApprovals", &reconcile.into_js_value());

    let services = Object::new();
    set(&services, "slots", &slots);
    set(&services, "locale", &locale);
    set(&services, "inputTriggers", &input_triggers);
    set(&services, "remote", &remote);
    set(&services, "remote.dynamicCordisRunner", &namespace);
    set(&services, "dynamicCordisRunner", &runner);

    let ctx = Object::new();
    let get_services = services.clone();
    let get_service = Closure::wrap(Box::new(move |name: String| -> JsValue {
        Reflect::get(&get_services, &JsValue::from_str(&name)).unwrap()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&ctx, "get", &get_service.into_js_value());
    let effect = Closure::wrap(Box::new(
        move |installer: Function, _label: String| -> Result<JsValue, JsValue> {
            installer.call0(&JsValue::UNDEFINED)
        },
    )
        as Box<dyn FnMut(Function, String) -> Result<JsValue, JsValue>>);
    set(&ctx, "effect", &effect.into_js_value());
    let on =
        Closure::wrap(
            Box::new(|_event: String, _listener: Function| -> Function { disposer() })
                as Box<dyn FnMut(String, Function) -> Function>,
        );
    set(&ctx, "on", &on.into_js_value());

    let plugin = client_ui_cordis_plugin(Object::new().into(), Object::new().into()).unwrap();
    let apply = get(&plugin, "apply").dyn_into::<Function>().unwrap();
    apply.call1(&JsValue::UNDEFINED, &ctx).unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();

    assert_eq!(
        injected.borrow().as_slice(),
        [
            "sidebar.footer.action",
            "tool.call.toolview",
            "tool.call.toolview",
            "tool.call.toolview"
        ]
    );
    assert_eq!(*registered_sources.borrow(), 1);
    assert_eq!(*registered_entries.borrow(), 5);
    assert_eq!(*inventory_reads.borrow(), 1);
    assert_eq!(*reconciles.borrow(), 1);
    assert_eq!(remote_events.borrow().len(), 4);
    assert_eq!(*inserted_styles.borrow(), 1);
    Reflect::delete_property(&global, &JsValue::from_str("document")).unwrap();
}

fn disposer() -> Function {
    Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
}

fn fake_react() -> JsValue {
    let react = Object::new();
    let create = Closure::wrap(Box::new(|kind: JsValue, props: JsValue| -> JsValue {
        let element = Object::new();
        set(&element, "kind", &kind);
        set(&element, "props", &props);
        element.into()
    }) as Box<dyn FnMut(JsValue, JsValue) -> JsValue>);
    set(&react, "createElement", &create.into_js_value());
    set(&react, "Fragment", &JsValue::from_str("Fragment"));
    let use_state = Closure::wrap(Box::new(|initial: JsValue| -> js_sys::Array {
        let state = js_sys::Array::new();
        state.push(&initial);
        state.push(&noop_function());
        state
    }) as Box<dyn FnMut(JsValue) -> js_sys::Array>);
    set(&react, "useState", &use_state.into_js_value());
    let use_id = Closure::wrap(
        Box::new(|| -> String { "cordis-id".to_owned() }) as Box<dyn FnMut() -> String>
    );
    set(&react, "useId", &use_id.into_js_value());
    let use_effect = Closure::wrap(
        Box::new(|effect: Function, _dependencies: JsValue| -> JsValue {
            effect
                .call0(&JsValue::UNDEFINED)
                .unwrap_or(JsValue::UNDEFINED)
        }) as Box<dyn FnMut(Function, JsValue) -> JsValue>,
    );
    set(&react, "useEffect", &use_effect.into_js_value());
    let use_ref = Closure::wrap(Box::new(|initial: JsValue| -> JsValue {
        let reference = Object::new();
        set(&reference, "current", &initial);
        reference.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    set(&react, "useRef", &use_ref.into_js_value());
    react.into()
}

fn fake_primitives() -> JsValue {
    let primitives = Object::new();
    for name in [
        "CodeBlock",
        "DisclosureRow",
        "IconCheckOutline16",
        "IconCloseOutline16",
        "IconCodeOutline16",
        "IconCordisPluginOutline14",
        "IconInspectOutline12",
        "IconPlayOutline16",
        "IconStopFill16",
        "IconTrashOutline16",
        "StateDot",
        "Tooltip",
    ] {
        set(&primitives, name, &JsValue::from_str(name));
    }
    primitives.into()
}

fn common_tool_props(call_id: &str, tool_name: &str) -> Object {
    let props = Object::new();
    set(&props, "callId", &JsValue::from_str(call_id));
    set(&props, "toolName", &JsValue::from_str(tool_name));
    set(&props, "t", &translation_function());
    props
}

fn translation_function() -> Function {
    Closure::wrap(Box::new(|key: String, _parameters: JsValue| -> String {
        format!("translated:{key}")
    }) as Box<dyn FnMut(String, JsValue) -> String>)
    .into_js_value()
    .unchecked_into()
}

fn observable_hook(value: JsValue) -> Function {
    Closure::wrap(
        Box::new(move |_selector: Function| -> JsValue { value.clone() })
            as Box<dyn FnMut(Function) -> JsValue>,
    )
    .into_js_value()
    .unchecked_into()
}

fn noop_function() -> Function {
    Closure::wrap(Box::new(|| -> JsValue { JsValue::UNDEFINED }) as Box<dyn FnMut() -> JsValue>)
        .into_js_value()
        .unchecked_into()
}

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}
