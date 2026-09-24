//! Live WASM session contexts, providers, optional hooks, and projection binding parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Object, Reflect};
use seekdeep_client_web_react::{
    configure_client_web_react, create_selector_shim, maybe_observable_hook, projection_hook,
    session_maybe_provider, session_provider, use_host, use_session_maybe_provide_info,
    use_session_provide_info,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function sessionBench() {
  const contexts = []
  const refs = []
  let cursor = 0
  class Component {}
  const React = {
    Component,
    Fragment: 'Fragment',
    createContext(value) {
      const context = { current: value }
      context.Provider = 'Provider'
      contexts.push(context)
      return context
    },
    useContext(context) { return context.current },
    useRef(initial) {
      const seat = cursor++
      if (!(seat in refs)) refs[seat] = { current: initial }
      return refs[seat]
    },
    useSyncExternalStore(subscribe, getSnapshot) { return getSnapshot() },
    createElement(kind, props, child) { return { kind, props: props ?? {}, child } },
  }
  const useSelector = (subscribe, getSnapshot, server, selector, equal) => selector(getSnapshot())
  return { React, useSelector, contexts }
}
export function setHost(bench, host) { bench.contexts[0].current = host }
export function setBinding(bench, info) { bench.contexts[1].current = info }
export function observable(value) {
  return { value, getSnapshot() { return this.value }, subscribe() { return () => {} } }
}
export function hostWithInfo(info) { return { sessions: { provideInfo: observable(info) } } }
export function providerValue(node) { return node.props.value }
export function providerKey(node) { return node.props.key }
export function providerChild(node) { return node.child }
export function callSessionProvider(props) { return wasmSessionProvider(props) }
export function callMaybeProvider(props) { return wasmMaybeProvider(props) }
let wasmSessionProvider, wasmMaybeProvider
export function installProviders(session, maybe) { wasmSessionProvider = session; wasmMaybeProvider = maybe }
export function projectionInfo(value) {
  const faces = new Map([['known', observable(value)]])
  return { projections: { faceOf(key) { return faces.get(key) } } }
}
"#)]
extern "C" {
    fn sessionBench() -> JsValue;
    fn setHost(bench: &JsValue, host: &JsValue);
    fn setBinding(bench: &JsValue, info: &JsValue);
    fn observable(value: JsValue) -> JsValue;
    fn hostWithInfo(info: &JsValue) -> JsValue;
    fn providerValue(node: &JsValue) -> JsValue;
    fn providerKey(node: &JsValue) -> JsValue;
    fn providerChild(node: &JsValue) -> JsValue;
    fn callSessionProvider(props: &JsValue) -> JsValue;
    fn callMaybeProvider(props: &JsValue) -> JsValue;
    fn installProviders(session: Function, maybe: Function);
    fn projectionInfo(value: JsValue) -> JsValue;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn configure(bench: &JsValue) {
    let react = property(bench, "React");
    let selector = create_selector_shim(react.clone()).unwrap();
    configure_client_web_react(react, selector.into()).unwrap();
    let session =
        wasm_bindgen::closure::Closure::wrap(Box::new(|props: JsValue| session_provider(props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let maybe = wasm_bindgen::closure::Closure::wrap(Box::new(|props: JsValue| {
        session_maybe_provider(props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    installProviders(
        session.into_js_value().unchecked_into(),
        maybe.into_js_value().unchecked_into(),
    );
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let output = js_sys::Object::new();
    for (key, value) in entries {
        Reflect::set(&output, &JsValue::from_str(key), value).unwrap();
    }
    output.into()
}

#[wasm_bindgen_test]
fn providers_follow_current_info_key_the_strict_body_and_preserve_empty_shape() {
    let bench = sessionBench();
    configure(&bench);
    let absent = object(&[("sessionId", JsValue::UNDEFINED)]);
    let host = hostWithInfo(&absent);
    setHost(&bench, &host);
    let empty = Function::new_no_args("return 'empty'");
    let body = Function::new_with_args("id", "return 'body:' + id");
    let node = callSessionProvider(&object(&[
        ("empty", empty.into()),
        ("children", body.clone().into()),
    ]));
    assert_eq!(providerChild(&node).as_string().as_deref(), Some("empty"));

    let selected = object(&[("sessionId", JsValue::from_str("s1"))]);
    Reflect::set(
        &property(&property(&host, "sessions"), "provideInfo"),
        &JsValue::from_str("value"),
        &selected,
    )
    .unwrap();
    let node = callSessionProvider(&object(&[("children", body.into())]));
    assert_eq!(providerKey(&node).as_string().as_deref(), Some("s1"));
    assert!(Object::is(&providerValue(&node), &selected));
    assert_eq!(providerChild(&node).as_string().as_deref(), Some("body:s1"));

    let maybe = callMaybeProvider(&object(&[("children", JsValue::from_str("child"))]));
    assert!(Object::is(&providerValue(&maybe), &selected));
    assert_eq!(providerChild(&maybe).as_string().as_deref(), Some("child"));
}

#[wasm_bindgen_test]
fn context_readers_fail_loud_and_strict_reader_rejects_the_absent_bundle() {
    let bench = sessionBench();
    configure(&bench);
    let error = use_host().unwrap_err();
    assert_eq!(
        property(&error, "name").as_string().as_deref(),
        Some("SlotAssemblyError")
    );
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("outside the installed renderer tree")
    );
    let host = hostWithInfo(&object(&[("sessionId", JsValue::UNDEFINED)]));
    setHost(&bench, &host);
    assert!(use_host().is_ok());
    assert!(use_session_maybe_provide_info().is_err());
    let absent = object(&[("sessionId", JsValue::UNDEFINED)]);
    setBinding(&bench, &absent);
    assert!(Object::is(
        &use_session_maybe_provide_info().unwrap(),
        &absent
    ));
    let error = use_session_provide_info().unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("without a session")
    );
}

#[wasm_bindgen_test]
fn optional_and_projection_hooks_keep_absence_and_keyed_values_hook_stable() {
    let bench = sessionBench();
    configure(&bench);
    let absent = maybe_observable_hook(JsValue::UNDEFINED).unwrap();
    let selector =
        Function::new_with_args("value", "return value === undefined ? 'absent' : value");
    assert!(
        absent
            .call2(&JsValue::UNDEFINED, &selector, &JsValue::UNDEFINED)
            .unwrap()
            .is_undefined()
    );
    assert!(Object::is(
        &absent,
        &maybe_observable_hook(JsValue::UNDEFINED).unwrap()
    ));

    let info = projectionInfo(JsValue::from_str("value"));
    let hook = projection_hook(info.clone()).unwrap();
    assert!(Object::is(&hook, &projection_hook(info).unwrap()));
    assert_eq!(
        hook.call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("known"),
            &Function::new_with_args("value", "return 'seen:' + value"),
            &JsValue::UNDEFINED,
        )
        .unwrap()
        .as_string()
        .as_deref(),
        Some("seen:value")
    );
    assert!(
        hook.call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("missing"),
            &JsValue::UNDEFINED,
            &JsValue::UNDEFINED
        )
        .unwrap()
        .is_undefined()
    );
    let present = maybe_observable_hook(observable(JsValue::from_f64(7.0))).unwrap();
    assert_eq!(
        present
            .call2(
                &JsValue::UNDEFINED,
                &Function::new_with_args("value", "return value"),
                &JsValue::UNDEFINED
            )
            .unwrap()
            .as_f64(),
        Some(7.0)
    );
}
