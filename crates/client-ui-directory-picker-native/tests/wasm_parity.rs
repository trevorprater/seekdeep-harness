//! Live renderless flow and transactional Slot registration parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_directory_picker_native::{
    apply_client_ui_directory_picker_native, configure_client_ui_directory_picker_native,
    directory_picker_native_inject, native_directory_flow_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function harness() {
  const slots = []
  let cursor = 0
  let pending = []
  const same = (a, b) => a && a.length === b.length && a.every((v, i) => Object.is(v, b[i]))
  const React = {
    useRef(initial) { const i = cursor++; if (!(i in slots)) slots[i] = { current: initial }; return slots[i] },
    useEffect(setup, deps = []) {
      const i = cursor++
      const previous = slots[i]
      if (previous && same(previous.deps, deps)) return
      pending.push(() => {
        previous?.cleanup?.()
        slots[i] = { deps: deps.slice(), cleanup: setup() }
      })
    },
  }
  return {
    React,
    render(component, props) { cursor = 0; pending = []; const value = component(props); pending.splice(0).forEach(run => run()); return value },
    unmount() { for (const slot of slots) slot?.cleanup?.() },
  }
}
export function makeDirectoryFlowBench() {
  const react = harness()
  const calls = []
  let resolve, reject
  const promise = new Promise((ok, fail) => { resolve = ok; reject = fail })
  const props = {
    open: true, busy: false,
    pick() { calls.push(['pick']); return promise },
    onPicked(path) { calls.push(['picked', path]) },
    onCancel() { calls.push(['cancel']) },
    onError(error) { calls.push(['error', error]) },
  }
  return { react, React: react.React, props, calls, resolve, reject }
}
export function directoryRender(bench, component) { return bench.react.render(component, bench.props) }
export function directoryUnmount(bench) { bench.react.unmount() }
export function directoryCalls(bench) { return bench.calls }
export function directorySetProps(bench, values) { Object.assign(bench.props, values) }
export function directoryResolve(bench, value) { bench.resolve(value) }
export function directoryTick() { return Promise.resolve().then(() => Promise.resolve()) }

export function makeDirectoryPluginBench() {
  const entries = [], effects = [], picks = []
  const ctx = { workspaces: { pickDirectory() { picks.push('pick'); return Promise.resolve('/tmp/picked') } } }
  ctx.slots = {
    inject(name, install) { const dispose = install(); effects.push(dispose); return dispose },
    register(options, component) {
      if (entries.some(entry => entry.options.name === options.name)) throw new Error(`${options.name} already has a registration`)
      const entry = { options, component }; entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return { ctx, entries, effects, picks }
}
export function directoryPluginEntries(bench) { return bench.entries }
export function directoryPluginInject(entry) { return entry.options.inject() }
export function directoryPluginDispose(bench) { [...bench.effects].reverse().forEach(dispose => dispose()) }
export function directoryPluginPicks(bench) { return bench.picks }
"#)]
extern "C" {
    fn makeDirectoryFlowBench() -> JsValue;
    fn directoryRender(bench: &JsValue, component: &Function) -> JsValue;
    fn directoryUnmount(bench: &JsValue);
    fn directoryCalls(bench: &JsValue) -> Array;
    fn directorySetProps(bench: &JsValue, values: &JsValue);
    fn directoryResolve(bench: &JsValue, value: &JsValue);
    fn directoryTick() -> js_sys::Promise;
    fn makeDirectoryPluginBench() -> JsValue;
    fn directoryPluginEntries(bench: &JsValue) -> Array;
    fn directoryPluginInject(entry: &JsValue) -> JsValue;
    fn directoryPluginDispose(bench: &JsValue);
    fn directoryPluginPicks(bench: &JsValue) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test(async)]
async fn one_pick_runs_per_open_edge_and_uses_latest_live_handlers() {
    let bench = makeDirectoryFlowBench();
    configure_client_ui_directory_picker_native(property(&bench, "React"));
    let component: Function = native_directory_flow_component()
        .unwrap()
        .dyn_into()
        .unwrap();
    assert!(directoryRender(&bench, &component).is_null());
    assert_eq!(directoryCalls(&bench).length(), 1);
    let latest = js_sys::Object::new();
    let calls: Array = property(&bench, "calls").dyn_into().unwrap();
    let on_picked = wasm_bindgen::closure::Closure::wrap(Box::new(move |path: String| {
        calls.push(&Array::of2(
            &JsValue::from_str("latest"),
            &JsValue::from_str(&path),
        ));
    }) as Box<dyn FnMut(String)>);
    Reflect::set(
        &latest,
        &JsValue::from_str("onPicked"),
        &on_picked.into_js_value(),
    )
    .unwrap();
    directorySetProps(&bench, &latest.into());
    directoryRender(&bench, &component);
    assert_eq!(directoryCalls(&bench).length(), 1);
    directoryResolve(&bench, &JsValue::from_str("/tmp/project"));
    JsFuture::from(directoryTick()).await.unwrap();
    assert!(directoryCalls(&bench).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("latest")
            && call.get(1).as_string().as_deref() == Some("/tmp/project")
    }));

    let closed = js_sys::Object::new();
    Reflect::set(&closed, &JsValue::from_str("open"), &JsValue::FALSE).unwrap();
    directorySetProps(&bench, &closed.into());
    directoryRender(&bench, &component);
    let opened = js_sys::Object::new();
    Reflect::set(&opened, &JsValue::from_str("open"), &JsValue::TRUE).unwrap();
    directorySetProps(&bench, &opened.into());
    directoryRender(&bench, &component);
    assert_eq!(directoryCalls(&bench).length(), 3); // first pick, latest result, second pick
    directoryUnmount(&bench);
}

#[wasm_bindgen_test(async)]
async fn plugin_registers_both_holes_transactionally_and_injects_pick() {
    let bench = makeDirectoryPluginBench();
    let flow = makeDirectoryFlowBench();
    configure_client_ui_directory_picker_native(property(&flow, "React"));
    apply_client_ui_directory_picker_native(property(&bench, "ctx")).unwrap();
    assert_eq!(directory_picker_native_inject().length(), 2);
    let entries = directoryPluginEntries(&bench);
    assert_eq!(entries.length(), 2);
    let injected = directoryPluginInject(&entries.get(0));
    let pick = property(&injected, "pick").dyn_into::<Function>().unwrap();
    let returned = pick.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(js_sys::Promise::resolve(&returned))
        .await
        .unwrap();
    assert_eq!(directoryPluginPicks(&bench).length(), 1);
    directoryPluginDispose(&bench);
    assert_eq!(directoryPluginEntries(&bench).length(), 0);
}
