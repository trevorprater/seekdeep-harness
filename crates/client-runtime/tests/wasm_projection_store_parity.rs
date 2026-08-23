//! Live JavaScript projection-store identity and notification parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Function, Object, Promise, Reflect};
use seekdeep_client_runtime::WasmProjectionValueStore;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn baseline(as_of_seq: f64, values: &[(&str, &JsValue)]) -> JsValue {
    let carried = Object::new();
    for (key, value) in values {
        set(&carried, key, value);
    }
    let baseline = Object::new();
    set(&baseline, "asOfSeq", &JsValue::from_f64(as_of_seq));
    set(&baseline, "values", &carried);
    baseline.into()
}

#[wasm_bindgen_test]
fn faces_values_and_whole_projection_values_keep_javascript_identity() {
    let store = WasmProjectionValueStore::new();
    assert!(store.get("test/marks").is_undefined());
    let face = store.face_of("test/marks".to_owned()).unwrap();
    assert!(Object::is(
        &face,
        &store.face_of("test/marks".to_owned()).unwrap()
    ));
    let snapshot = get(&face, "getSnapshot").dyn_into::<Function>().unwrap();
    assert!(snapshot.call0(&face).unwrap().is_undefined());

    let pushed: JsValue = Object::new().into();
    store
        .apply("test/marks".to_owned(), pushed.clone(), 20.0)
        .unwrap();
    assert!(Object::is(&store.get("test/marks"), &pushed));
    assert!(Object::is(&snapshot.call0(&face).unwrap(), &pushed));
    let values = store.values().unwrap();
    assert!(Object::is(&get(&values, "test/marks"), &pushed));
    assert!(Object::is(&values, &store.values().unwrap()));
    assert!(Object::is_frozen(&Object::from(values.clone())));

    let stale: JsValue = Object::new().into();
    store.apply("test/marks".to_owned(), stale, 20.0).unwrap();
    assert!(Object::is(&values, &store.values().unwrap()));
    store.seed(baseline(10.0, &[])).unwrap();
    assert!(Object::is(&store.get("test/marks"), &pushed));
    store.seed(baseline(30.0, &[])).unwrap();
    assert!(store.get("test/marks").is_undefined());

    let phantom: JsValue = Object::new().into();
    store.apply("phantom".to_owned(), phantom, 50.0).unwrap();
    store.truncate(40.0).unwrap();
    assert!(store.get("phantom").is_undefined());
}

#[wasm_bindgen_test(async)]
async fn key_and_any_subscribers_receive_one_microtask_batched_publication() {
    let store = WasmProjectionValueStore::new();
    let face = store.face_of("test/marks".to_owned()).unwrap();
    let key_ticks = Rc::new(Cell::new(0));
    let observed = key_ticks.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    let subscribe = get(&face, "subscribe").dyn_into::<Function>().unwrap();
    let dispose_key = subscribe
        .call1(&face, &listener.into_js_value())
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let any_ticks = Rc::new(Cell::new(0));
    let observed = any_ticks.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    let dispose_any = store.subscribe_any(listener.into_js_value().unchecked_into());

    store
        .apply("test/marks".to_owned(), Object::new().into(), 1.0)
        .unwrap();
    store
        .apply("test/marks".to_owned(), Object::new().into(), 2.0)
        .unwrap();
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
    assert_eq!(key_ticks.get(), 1);
    assert_eq!(any_ticks.get(), 1);

    dispose_key.call0(&JsValue::UNDEFINED).unwrap();
    dispose_any.call0(&JsValue::UNDEFINED).unwrap();
    store
        .apply("test/marks".to_owned(), Object::new().into(), 3.0)
        .unwrap();
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
    let _ = JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await;
    assert_eq!(key_ticks.get(), 1);
    assert_eq!(any_ticks.get(), 1);
}
