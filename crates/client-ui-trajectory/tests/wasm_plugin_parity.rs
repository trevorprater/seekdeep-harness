//! Live browser plugin registration through caller-bound Rust registries.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_runtime::{
    WasmConversationEventRegistry, WasmConversationNodeAssembler, WasmConversationViewRegistry,
};
use seekdeep_client_ui_trajectory::{
    apply_client_ui_trajectory, configure_client_ui_trajectory_modules,
    configure_client_ui_trajectory_runtime, trajectory_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function makeTrajectoryPluginBench() {
  const effects = []
  const slotEntries = []
  const localeCalls = []
  const duration = {
    value: false,
    getSnapshot() { return this.value },
    subscribe() { return () => {} },
    set(value) { this.value = value },
  }
  let trajectorySnapshot = { generation: 1 }
  const session = {
    getSnapshot() { return { views: new Map([['trajectory', trajectorySnapshot]]) } },
    loadOlder() { trajectorySnapshot = { generation: trajectorySnapshot.generation + 1 }; return Promise.resolve() },
  }
  const ctx = {
    effect(setup, label) {
      const dispose = setup()
      effects.push({ label, dispose: typeof dispose === 'function' ? dispose : () => {} })
      return dispose
    },
  }
  ctx.locale = {
    register(namespace, dictionaries) {
      localeCalls.push([namespace, dictionaries])
      return () => localeCalls.push(['disposed', namespace])
    },
    bind() { return key => key === 'view.trajectory' ? 'Trajectory' : key },
  }
  ctx.slots = {
    inject(name, install) {
      const dispose = install()
      effects.push({ label: `slot:${name}`, dispose })
      return dispose
    },
    register(options, component) {
      const entry = { options, component }
      slotEntries.push(entry)
      return () => {
        const at = slotEntries.indexOf(entry)
        if (at >= 0) slotEntries.splice(at, 1)
      }
    },
  }
  ctx.sessions = { binding(id) { return id === 'session-1' ? { session } : undefined } }
  const runtime = {
    createSnapshotStore(initial, options) {
      duration.value = initial
      duration.options = options
      return duration
    },
  }
  return {
    ctx,
    runtime,
    React: { createElement(kind, props, ...children) { return { kind, props, children } } },
    primitives: {},
    effects,
    slotEntries,
    localeCalls,
    duration,
  }
}

export function setTrajectoryPluginService(bench, name, value) { bench.ctx[name] = value }
export function trajectorySlotEntries(bench) { return bench.slotEntries }
export function trajectoryLocaleCalls(bench) { return bench.localeCalls }
export function trajectoryDisposeAll(bench) {
  for (const effect of [...bench.effects].reverse()) effect.dispose()
}
export function trajectorySlotInject(bench, sessionId) {
  return bench.slotEntries[0].options.inject(sessionId)
}
export function trajectorySlotInjectError(bench, sessionId) {
  try { bench.slotEntries[0].options.inject(sessionId); return '' }
  catch (error) { return String(error?.message ?? error) }
}
export function trajectoryCall(object, name, ...args) { return object[name](...args) }
export function trajectoryProperty(object, name) { return object?.[name] }
"#)]
extern "C" {
    fn makeTrajectoryPluginBench() -> JsValue;
    fn setTrajectoryPluginService(bench: &JsValue, name: &str, value: &JsValue);
    fn trajectorySlotEntries(bench: &JsValue) -> Array;
    fn trajectoryLocaleCalls(bench: &JsValue) -> Array;
    fn trajectoryDisposeAll(bench: &JsValue);
    fn trajectorySlotInject(bench: &JsValue, session_id: &str) -> JsValue;
    fn trajectorySlotInjectError(bench: &JsValue, session_id: &str) -> String;
    fn trajectoryCall(object: &JsValue, name: &str, argument: &JsValue) -> JsValue;
    fn trajectoryProperty(object: &JsValue, name: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn apply_registers_native_definitions_view_slot_locale_store_and_disposes() {
    let bench = makeTrajectoryPluginBench();
    let ctx = property(&bench, "ctx");
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    let event_face = events.face_for(ctx.clone()).unwrap();
    let view_face = views.face_for(ctx.clone()).unwrap();
    setTrajectoryPluginService(&bench, "conversationEvents", &event_face);
    setTrajectoryPluginService(&bench, "conversationViews", &view_face);
    configure_client_ui_trajectory_modules(
        property(&bench, "React"),
        property(&bench, "primitives"),
    )
    .unwrap();
    configure_client_ui_trajectory_runtime(property(&bench, "runtime")).unwrap();

    apply_client_ui_trajectory(ctx).unwrap();
    assert_eq!(trajectory_inject().length(), 5);
    assert_eq!(events.entries().length(), 8);
    assert_eq!(views.entries().length(), 1);
    assert_eq!(trajectorySlotEntries(&bench).length(), 1);
    assert_eq!(trajectoryLocaleCalls(&bench).length(), 1);
    let entry = trajectorySlotEntries(&bench).get(0);
    let options = property(&entry, "options");
    assert_eq!(
        property(&options, "id").as_string().as_deref(),
        Some("trajectory")
    );
    assert_eq!(property(&options, "order").as_f64(), Some(10.0));
    let label = property(&options, "label").dyn_into::<Function>().unwrap();
    assert_eq!(
        label
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .as_string()
            .as_deref(),
        Some("Trajectory")
    );
    let duration = property(&bench, "duration");
    let options = property(&duration, "options");
    let persist = property(&options, "persist");
    assert_eq!(
        property(&persist, "name").as_string().as_deref(),
        Some("dsh.trajectory.duration")
    );

    let request_header = events
        .entries()
        .iter()
        .find(|definition| {
            property(definition, "kind").as_string().as_deref() == Some("trajectory-request-header")
        })
        .unwrap();
    let event = js_sys::Object::new();
    for (key, value) in [
        ("seq", JsValue::from_f64(1.0)),
        ("time", JsValue::from_f64(1_000.0)),
        ("type", JsValue::from_str("request/header")),
        ("data", js_sys::Object::new().into()),
    ] {
        Reflect::set(&event, &JsValue::from_str(key), &value).unwrap();
    }
    let matched = trajectoryCall(&request_header, "match", &event.into());
    assert_eq!(property(&matched, "id").as_string().as_deref(), Some("1"));
    assert_eq!(
        property(&matched, "role").as_string().as_deref(),
        Some("start")
    );

    let mut assembler = WasmConversationNodeAssembler::new(&events, &views);
    let wire = js_sys::JSON::parse(
        r#"{"seq":1,"time":1000,"type":"user/message","data":{"id":"m1","source":{"kind":"user"},"content":[{"type":"text","text":"hello"}]}}"#,
    )
    .unwrap();
    let input = js_sys::Object::new();
    Reflect::set(&input, &JsValue::from_str("event"), &wire).unwrap();
    assembler.replace_window(Array::of1(&input), false).unwrap();
    assert!(assembler.flush().unwrap());
    let snapshot = assembler.get("trajectory").unwrap();
    let nodes = Array::from(&property(&snapshot, "eventNodes"));
    assert_eq!(nodes.length(), 1);
    assert_eq!(
        property(&nodes.get(0), "kind").as_string().as_deref(),
        Some("user")
    );

    trajectoryDisposeAll(&bench);
    assert_eq!(events.entries().length(), 0);
    assert_eq!(views.entries().length(), 0);
    assert_eq!(trajectorySlotEntries(&bench).length(), 0);
}

#[wasm_bindgen_test(async)]
async fn slot_injection_shares_duration_pages_and_fails_loud_for_missing_sessions() {
    let bench = makeTrajectoryPluginBench();
    let ctx = property(&bench, "ctx");
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    setTrajectoryPluginService(
        &bench,
        "conversationEvents",
        &events.face_for(ctx.clone()).unwrap(),
    );
    setTrajectoryPluginService(
        &bench,
        "conversationViews",
        &views.face_for(ctx.clone()).unwrap(),
    );
    configure_client_ui_trajectory_modules(
        property(&bench, "React"),
        property(&bench, "primitives"),
    )
    .unwrap();
    configure_client_ui_trajectory_runtime(property(&bench, "runtime")).unwrap();
    apply_client_ui_trajectory(ctx).unwrap();

    let first = trajectorySlotInject(&bench, "session-1");
    let second = trajectorySlotInject(&bench, "session-1");
    assert!(js_sys::Object::is(
        &property(&property(&first, "hooks"), "duration"),
        &property(&property(&second, "hooks"), "duration")
    ));
    let setter = property(&first, "setActualDuration")
        .dyn_into::<Function>()
        .unwrap();
    setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE).unwrap();
    assert_eq!(
        property(&property(&bench, "duration"), "value").as_bool(),
        Some(true)
    );
    let load = property(&first, "loadOlder")
        .dyn_into::<Function>()
        .unwrap();
    let changed = load.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(
        JsFuture::from(Promise::resolve(&changed))
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );

    assert_eq!(
        trajectorySlotInjectError(&bench, "missing"),
        "ui-trajectory: session \"missing\" is unavailable"
    );
}
