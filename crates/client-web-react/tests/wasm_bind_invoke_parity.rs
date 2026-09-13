//! Live WASM selector binding and concurrent invoke-hook parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_web_react::{
    bind_snapshot_selector, configure_client_web_react, create_selector_shim,
    maybe_observable_hook, observable_hook, projection_hook, use_invoke,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function webReactBench() {
  const selectorCalls = []
  const hookRows = []
  let cursor = 0
  class Component {}
  const React = {
    Component,
    Fragment: 'Fragment',
    createContext(initial) {
      const context = { value: initial }
      context.Provider = props => { context.value = props.value; return props.children }
      return context
    },
    useContext(context) { return context.value },
    createElement(kind, props, ...children) {
      props ||= {}
      if (typeof kind === 'function') return kind({ ...props, children: children[0] })
      return { kind, props, children }
    },
    useRef(initial) {
      const seat = cursor++
      if (!(seat in hookRows)) hookRows[seat] = { current: initial }
      return hookRows[seat]
    },
    useSyncExternalStore(subscribe, getSnapshot) {
      const seat = cursor++
      if (!(seat in hookRows)) {
        const listener = () => {}
        hookRows[seat] = { subscribe, cleanup: subscribe(listener) }
      } else if (hookRows[seat].subscribe !== subscribe) {
        hookRows[seat].cleanup?.()
        const listener = () => {}
        hookRows[seat] = { subscribe, cleanup: subscribe(listener) }
      }
      return getSnapshot()
    },
    render(body) { cursor = 0; return body() },
    unmount() { for (const row of hookRows) row?.cleanup?.(); hookRows.length = 0 },
  }
  const useSelector = (subscribe, getSnapshot, getServerSnapshot, selector, equal) => {
    selectorCalls.push({ subscribe, getSnapshot, getServerSnapshot, selector, equal })
    return selector(getSnapshot())
  }
  return { React, useSelector, selectorCalls, errors: [] }
}

export function methodSource() {
  return {
    state: { a: 7, b: 1 }, listeners: new Set(), subscribeCalls: 0,
    getSnapshot() { if (this.state === undefined) throw new Error('lost this'); return this.state },
    subscribe(listener) {
      if (this.listeners === undefined) throw new Error('lost this')
      this.subscribeCalls += 1
      this.listeners.add(listener)
      return () => this.listeners.delete(listener)
    },
  }
}
export function selectorCalls(bench) { return bench.selectorCalls }
export function renderSelector(bench, hook, selector, equal) {
  return bench.React.render(() => hook(selector, equal))
}
export function setMethodSource(source, a, b) {
  source.state = { a, b }
  for (const listener of [...source.listeners]) listener()
}
export function observable(value) {
  return { value, getSnapshot() { return this.value }, subscribe() { return () => {} } }
}
export function projectionInfo() {
  const rows = new Map([['title', observable({ text: 'hello' })]])
  return {
    sessionId: 's-one', hooks: {}, props: {},
    projections: { faceOf(key) { return rows.get(key) } },
  }
}
export function renderInvoke(bench, fn) { return bench.React.render(() => wasmUseInvoke(fn)) }
let wasmUseInvoke
export function installUseInvoke(fn) { wasmUseInvoke = fn }
export function unmountInvoke(bench) { bench.React.unmount() }
export function deferred() {
  let resolve, reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
export function actionQueue(rows, calls) { return () => { calls.push('call'); return rows.shift().promise } }
export function actionResolved(calls, label) { return () => { calls.push(label); return Promise.resolve() } }
export function invokeCalls() { return [] }
export function invokeArrayGet(value, index) { return value[index] }
export function deferredResolve(row) { row.resolve() }
export function deferredReject(row) { row.reject(new Error('boom')) }
export function flushMicrotasks() { return Promise.resolve().then(() => Promise.resolve()) }
export function captureConsole(bench) {
  const previous = console.error
  console.error = (...args) => { bench.errors.push(args) }
  return () => { console.error = previous }
}
export function consoleErrors(bench) { return bench.errors }
"#)]
extern "C" {
    fn webReactBench() -> JsValue;
    fn methodSource() -> JsValue;
    fn renderSelector(
        bench: &JsValue,
        hook: &Function,
        selector: &Function,
        equal: &JsValue,
    ) -> JsValue;
    fn setMethodSource(source: &JsValue, a: f64, b: f64);
    fn observable(value: JsValue) -> JsValue;
    fn projectionInfo() -> JsValue;
    fn installUseInvoke(function: Function);
    fn renderInvoke(bench: &JsValue, action: &Function) -> Array;
    fn unmountInvoke(bench: &JsValue);
    fn deferred() -> JsValue;
    fn actionQueue(rows: &Array, calls: &Array) -> Function;
    fn actionResolved(calls: &Array, label: &str) -> Function;
    fn invokeCalls() -> Array;
    fn invokeArrayGet(value: &Array, index: u32) -> JsValue;
    fn deferredResolve(row: &JsValue);
    fn deferredReject(row: &JsValue);
    fn flushMicrotasks() -> Promise;
    fn captureConsole(bench: &JsValue) -> Function;
    fn consoleErrors(bench: &JsValue) -> Array;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn configure(bench: &JsValue) {
    configure_client_web_react(property(bench, "React"), property(bench, "useSelector")).unwrap();
    let use_invoke =
        wasm_bindgen::closure::Closure::wrap(Box::new(|action: JsValue| use_invoke(action))
            as Box<dyn FnMut(JsValue) -> Result<Array, JsValue>>);
    installUseInvoke(use_invoke.into_js_value().unchecked_into());
}

fn configure_internal_selector(bench: &JsValue) {
    let react = property(bench, "React");
    let selector = create_selector_shim(react.clone()).unwrap();
    configure_client_web_react(react, selector.into()).unwrap();
}

async fn flush() {
    JsFuture::from(flushMicrotasks()).await.unwrap();
}

#[wasm_bindgen_test]
fn selector_binding_captures_stable_this_safe_source_methods_and_equality() {
    let bench = webReactBench();
    configure(&bench);
    let source = methodSource();
    let hook = bind_snapshot_selector(source.clone()).unwrap();
    let selector = Function::new_with_args("snapshot", "return snapshot.a");
    let result = renderSelector(&bench, &hook, &selector, &JsValue::UNDEFINED);
    assert_eq!(result.as_f64(), Some(7.0));
    let equality = Function::new_with_args("left,right", "return left === right");
    renderSelector(&bench, &hook, &selector, &equality);
    unmountInvoke(&bench);
}

#[wasm_bindgen_test]
fn rust_selector_shim_uses_custom_equality_and_stable_subscription() {
    let bench = webReactBench();
    configure_internal_selector(&bench);
    let source = methodSource();
    let hook = bind_snapshot_selector(source.clone()).unwrap();
    let selector = Function::new_with_args("snapshot", "return { a: snapshot.a }");
    let equality = Function::new_with_args("left,right", "return left.a === right.a");
    let first = renderSelector(&bench, &hook, &selector, equality.as_ref());
    setMethodSource(&source, 7.0, 2.0);
    let equal = renderSelector(&bench, &hook, &selector, equality.as_ref());
    assert!(Object::is(&first, &equal));
    setMethodSource(&source, 8.0, 2.0);
    let changed = renderSelector(&bench, &hook, &selector, equality.as_ref());
    assert!(!Object::is(&equal, &changed));
    assert_eq!(property(&source, "subscribeCalls").as_f64(), Some(1.0));
    unmountInvoke(&bench);
}

#[wasm_bindgen_test]
fn observable_optional_and_projection_hooks_are_identity_stable_and_absence_safe() {
    let bench = webReactBench();
    configure(&bench);
    let source = observable(JsValue::from_str("snapshot"));
    let first = observable_hook(source.clone()).unwrap();
    let second = observable_hook(source).unwrap();
    assert!(Object::is(&first, &second));

    let absent = maybe_observable_hook(JsValue::UNDEFINED).unwrap();
    let selector = Function::new_with_args("_value", "throw new Error('must not run')");
    assert!(
        absent
            .call2(&JsValue::UNDEFINED, &selector, &JsValue::UNDEFINED)
            .unwrap()
            .is_undefined()
    );

    let info = projectionInfo();
    let projection = projection_hook(info.clone()).unwrap();
    assert!(Object::is(&projection, &projection_hook(info).unwrap()));
    let select_text = Function::new_with_args("value", "return value?.text");
    assert_eq!(
        projection
            .call3(
                &JsValue::UNDEFINED,
                &JsValue::from_str("title"),
                &select_text,
                &JsValue::UNDEFINED,
            )
            .unwrap()
            .as_string()
            .as_deref(),
        Some("hello")
    );
    assert!(
        projection
            .call3(
                &JsValue::UNDEFINED,
                &JsValue::from_str("missing"),
                &select_text,
                &JsValue::UNDEFINED,
            )
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test(async)]
async fn invoke_is_stable_uses_latest_action_and_counts_concurrent_settlement() {
    let bench = webReactBench();
    configure(&bench);
    let calls = invokeCalls();
    let first = deferred();
    let second = deferred();
    let queue = Array::of2(&first, &second);
    let action = actionQueue(&queue, &calls);
    let initial = renderInvoke(&bench, &action);
    let invoke = invokeArrayGet(&initial, 0).dyn_into::<Function>().unwrap();
    assert_eq!(invokeArrayGet(&initial, 1).as_bool(), Some(false));
    invoke.call0(&JsValue::UNDEFINED).unwrap();
    invoke.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(
        invokeArrayGet(&renderInvoke(&bench, &action), 1).as_bool(),
        Some(true)
    );
    deferredResolve(&first);
    flush().await;
    assert_eq!(
        invokeArrayGet(&renderInvoke(&bench, &action), 1).as_bool(),
        Some(true)
    );
    deferredResolve(&second);
    flush().await;
    assert_eq!(
        invokeArrayGet(&renderInvoke(&bench, &action), 1).as_bool(),
        Some(false)
    );
    assert_eq!(calls.length(), 2);

    let latest = actionResolved(&calls, "latest");
    let rerendered = renderInvoke(&bench, &latest);
    assert!(Object::is(&invokeArrayGet(&rerendered, 0), &invoke));
    invoke.call0(&JsValue::UNDEFINED).unwrap();
    flush().await;
    assert_eq!(calls.get(2).as_string().as_deref(), Some("latest"));
    unmountInvoke(&bench);
}

#[wasm_bindgen_test(async)]
async fn rejected_action_logs_and_always_clears_pending() {
    let bench = webReactBench();
    configure(&bench);
    let restore = captureConsole(&bench);
    let calls = invokeCalls();
    let row = deferred();
    let queue = Array::of1(&row);
    let action = actionQueue(&queue, &calls);
    let rendered = renderInvoke(&bench, &action);
    invokeArrayGet(&rendered, 0)
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    deferredReject(&row);
    flush().await;
    assert_eq!(
        invokeArrayGet(&renderInvoke(&bench, &action), 1).as_bool(),
        Some(false)
    );
    assert_eq!(consoleErrors(&bench).length(), 1);
    assert_eq!(
        Array::from(&consoleErrors(&bench).get(0))
            .get(0)
            .as_string()
            .as_deref(),
        Some("useInvoke action failed:")
    );
    restore.call0(&JsValue::UNDEFINED).unwrap();
}
