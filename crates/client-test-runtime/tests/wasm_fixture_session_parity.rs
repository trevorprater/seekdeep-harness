//! Live `FixtureSession` snapshot, projection, stub, and override parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_test_runtime::{conversation_snapshot_js, create_fixture_session};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_test::wasm_bindgen_test;

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn method(value: &JsValue, key: &str) -> Function {
    property(value, key).dyn_into().unwrap()
}

#[wasm_bindgen_test]
fn fail_loud_verbs_and_overrides_match_the_fixture_face() {
    let snapshot = conversation_snapshot_js("s1".to_owned()).unwrap();
    let bare =
        create_fixture_session("s1".to_owned(), snapshot.clone(), Object::new().into()).unwrap();
    assert!(Object::is(
        &method(&bare, "getSnapshot")
            .call0(&JsValue::UNDEFINED)
            .unwrap(),
        &snapshot
    ));
    for name in [
        "prompt",
        "readAttachment",
        "updateQueue",
        "cancel",
        "command",
        "loadOlder",
        "rename",
    ] {
        let error = method(&bare, name).call0(&JsValue::UNDEFINED).unwrap_err();
        let message = property(&error, "message").as_string().unwrap();
        assert!(message.contains(name), "{message}");
        assert!(message.contains("is not stubbed"), "{message}");
    }

    let overrides = Object::new();
    let prompt = Function::new_no_args("return 'stubbed'");
    Reflect::set(&overrides, &JsValue::from_str("prompt"), &prompt).unwrap();
    let overridden = create_fixture_session("s2".to_owned(), snapshot, overrides.into()).unwrap();
    assert_eq!(
        method(&overridden, "prompt")
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .as_string()
            .as_deref(),
        Some("stubbed")
    );
}

#[wasm_bindgen_test]
fn projection_faces_are_stable_notify_and_unsubscribe_by_identity() {
    let session = create_fixture_session(
        "s1".to_owned(),
        conversation_snapshot_js("s1".to_owned()).unwrap(),
        Object::new().into(),
    )
    .unwrap();
    let projections = property(&session, "projections");
    let face = method(&projections, "faceOf")
        .call1(&projections, &JsValue::from_str("todos"))
        .unwrap();
    let again = method(&projections, "faceOf")
        .call1(&projections, &JsValue::from_str("todos"))
        .unwrap();
    assert!(Object::is(&face, &again));
    assert!(
        method(&face, "getSnapshot")
            .call0(&face)
            .unwrap()
            .is_undefined()
    );
    let seen = Array::new();
    let captured = seen.clone();
    let observed_face = face.clone();
    let listener = Closure::wrap(Box::new(move || {
        captured.push(
            &method(&observed_face, "getSnapshot")
                .call0(&observed_face)
                .unwrap(),
        );
    }) as Box<dyn FnMut()>);
    let listener = listener.into_js_value().dyn_into::<Function>().unwrap();
    let off = method(&face, "subscribe")
        .call1(&face, &listener)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    method(&face, "subscribe").call1(&face, &listener).unwrap();
    let first = Array::of2(&JsValue::from_f64(1.0), &JsValue::from_f64(2.0));
    method(&projections, "set")
        .call2(&projections, &JsValue::from_str("todos"), &first)
        .unwrap();
    assert_eq!(seen.length(), 1);
    assert!(Object::is(&seen.get(0), &first));
    off.call0(&JsValue::UNDEFINED).unwrap();
    off.call0(&JsValue::UNDEFINED).unwrap();
    method(&projections, "set")
        .call2(
            &projections,
            &JsValue::from_str("todos"),
            &Array::of1(&JsValue::from_f64(3.0)),
        )
        .unwrap();
    assert_eq!(seen.length(), 1);
    method(&projections, "set")
        .call2(
            &projections,
            &JsValue::from_str("untouched"),
            &JsValue::from_f64(1.0),
        )
        .unwrap();
}
