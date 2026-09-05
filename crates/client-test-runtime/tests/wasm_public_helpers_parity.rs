//! Browser-compatible public helper construction and delegation parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_test_runtime::{
    create_fixture_session_from_store, create_stub_settings_scope, create_test_root,
    make_translate_js,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function helperSpyFactory() {
  return implementation => {
    const spy = (...args) => { spy.calls.push(args); return implementation(...args) }
    spy.calls = []
    return spy
  }
}
export function helperPrototypeParams() {
  const params = Object.create({ inherited: 'yes' })
  params.name = ['A', null, 'B']
  params.object = { value: true }
  return params
}
export function helperStore() {
  let snapshot = { marker: 1 }
  const listeners = new Set()
  return {
    getSnapshot() { return snapshot },
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener) },
    publish(next) { snapshot = next; for (const listener of [...listeners]) listener() },
  }
}
export function helperRootBench() {
  const bench = { registrations: 0, disposals: 0, stabilizations: 0 }
  bench.slots = {
    register(options, component) {
      if (options.name !== 'root' || options.children.panel.kind !== 'single' || component !== 'frame') {
        throw new Error('TestRoot registration drifted')
      }
      bench.registrations += 1
      return () => { bench.disposals += 1 }
    },
  }
  bench.stabilize = async callback => { bench.stabilizations += 1; await callback() }
  return bench
}
export function helperRootChildren() { return { panel: { kind: 'single', scope: 'root' } } }
"#)]
extern "C" {
    fn helperSpyFactory() -> JsValue;
    fn helperPrototypeParams() -> JsValue;
    fn helperStore() -> JsValue;
    fn helperRootBench() -> JsValue;
    fn helperRootChildren() -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, key: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = property(value, key).dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().cloned().collect();
    function.apply(value, &arguments)
}

#[wasm_bindgen_test]
fn translation_uses_order_fallback_word_keys_prototypes_and_javascript_stringification() {
    let first = Object::new();
    Reflect::set(
        &first,
        &JsValue::from_str("message"),
        &JsValue::from_str("first {name} {missing} {inherited} {object}"),
    )
    .unwrap();
    let second = Object::new();
    Reflect::set(
        &second,
        &JsValue::from_str("message"),
        &JsValue::from_str("second"),
    )
    .unwrap();
    let translator = make_translate_js(Array::of2(&first, &second)).unwrap();
    assert_eq!(
        translator
            .call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str("message"),
                &helperPrototypeParams(),
            )
            .unwrap()
            .as_string()
            .as_deref(),
        Some("first A,,B {missing} yes [object Object]")
    );
    assert_eq!(
        translator
            .call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str("unknown"),
                &JsValue::UNDEFINED,
            )
            .unwrap()
            .as_string()
            .as_deref(),
        Some("unknown")
    );
}

#[wasm_bindgen_test(async)]
async fn settings_scope_spies_observable_identity_and_partial_publication_match_source() {
    let handle = create_stub_settings_scope(helperSpyFactory()).unwrap();
    let scope = property(&handle, "scope");
    let initial = call(&scope, "getSnapshot", &[]).unwrap();
    assert_eq!(
        property(&initial, "status").as_string().as_deref(),
        Some("loading")
    );
    assert!(property(&initial, "base").is_undefined());
    assert_eq!(property(&initial, "writable").as_bool(), Some(false));

    let notifications = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let observed = notifications.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    let listener = listener.into_js_value().dyn_into::<Function>().unwrap();
    let unsubscribe = call(&scope, "subscribe", std::slice::from_ref(&listener))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    call(&scope, "subscribe", std::slice::from_ref(&listener)).unwrap();
    assert_eq!(
        call(&handle, "listenerCount", &[]).unwrap().as_f64(),
        Some(1.0)
    );
    let set_result = call(
        &scope,
        "set",
        &[JsValue::from_str("preference"), JsValue::from_str("dark")],
    )
    .unwrap();
    JsFuture::from(Promise::resolve(&set_result)).await.unwrap();
    let unset_result = call(&scope, "unset", &[JsValue::from_str("preference")]).unwrap();
    JsFuture::from(Promise::resolve(&unset_result))
        .await
        .unwrap();
    let patch = Object::new();
    Reflect::set(
        &patch,
        &JsValue::from_str("status"),
        &JsValue::from_str("ready"),
    )
    .unwrap();
    Reflect::set(
        &patch,
        &JsValue::from_str("revision"),
        &JsValue::from_f64(2.0),
    )
    .unwrap();
    call(&handle, "publish", &[patch.into()]).unwrap();
    assert_eq!(notifications.get(), 1);
    let accepted = call(&scope, "getSnapshot", &[]).unwrap();
    assert_eq!(
        property(&accepted, "status").as_string().as_deref(),
        Some("ready")
    );
    assert_eq!(property(&accepted, "revision").as_f64(), Some(2.0));
    assert_eq!(
        Array::from(&property(&property(&handle, "set"), "calls")).length(),
        1
    );
    assert_eq!(
        Array::from(&property(&property(&handle, "unset"), "calls")).length(),
        1
    );
    unsubscribe.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(
        call(&handle, "listenerCount", &[]).unwrap().as_f64(),
        Some(0.0)
    );
}

#[wasm_bindgen_test(async)]
async fn public_fixture_and_test_root_delegate_to_the_supplied_store_and_slot_service() {
    let store = helperStore();
    let session =
        create_fixture_session_from_store("s1".to_owned(), store.clone(), Object::new().into())
            .unwrap();
    assert_eq!(
        property(&call(&session, "getSnapshot", &[]).unwrap(), "marker").as_f64(),
        Some(1.0)
    );
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let observed = notifications.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    let off = call(&session, "subscribe", &[listener.into_js_value()])
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let next = Object::new();
    Reflect::set(&next, &JsValue::from_str("marker"), &JsValue::from_f64(2.0)).unwrap();
    call(&store, "publish", &[next.into()]).unwrap();
    assert_eq!(notifications.get(), 1);
    assert_eq!(
        property(&call(&session, "getSnapshot", &[]).unwrap(), "marker").as_f64(),
        Some(2.0)
    );
    off.call0(&JsValue::UNDEFINED).unwrap();

    let bench = helperRootBench();
    let root = create_test_root(property(&bench, "slots"), property(&bench, "stabilize")).unwrap();
    let declaration = call(
        &root,
        "declare",
        &[helperRootChildren(), JsValue::from_str("frame")],
    )
    .unwrap();
    JsFuture::from(Promise::resolve(&declaration))
        .await
        .unwrap();
    assert_eq!(property(&bench, "registrations").as_f64(), Some(1.0));
    assert_eq!(property(&bench, "stabilizations").as_f64(), Some(1.0));
    call(&root, "release", &[]).unwrap();
    call(&root, "release", &[]).unwrap();
    assert_eq!(property(&bench, "disposals").as_f64(), Some(1.0));
}
