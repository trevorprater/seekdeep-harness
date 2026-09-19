//! Live JavaScript Conversation registry shape and caller-effect ownership parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Object, Reflect};
use seekdeep_client_runtime::{WasmConversationEventRegistry, WasmConversationViewRegistry};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn event(kind: &str, target: bool, builder: bool) -> JsValue {
    let value = Object::new();
    set(&value, "kind", &JsValue::from_str(kind));
    if target {
        set(&value, "target", &JsValue::from_str("chat"));
    }
    if builder {
        set(
            &value,
            "buildViewNode",
            &Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>).into_js_value(),
        );
    }
    value.into()
}

fn view(target: &str) -> JsValue {
    let value = Object::new();
    set(&value, "target", &JsValue::from_str(target));
    value.into()
}

fn caller() -> JsValue {
    let value = Object::new();
    let effect = Function::new_with_args("installer, _label", "return installer()");
    set(&value, "effect", &effect);
    value.into()
}

#[wasm_bindgen_test]
fn event_and_view_classes_preserve_identity_validation_fallback_and_disposal() {
    let events = WasmConversationEventRegistry::new();
    let changed = std::rc::Rc::new(std::cell::Cell::new(0));
    let observed = changed.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    events.subscribe(listener.into_js_value().unchecked_into());
    let definition = event("message", true, true);
    let dispose = events.register(definition.clone()).unwrap();
    let first = events.entries();
    assert!(Object::is(&first, &events.entries()));
    assert!(Object::is(&first.get(0), &definition));
    assert!(events.register(event("message", true, true)).is_err());
    assert!(events.register(event("target-only", true, false)).is_err());
    assert!(events.register(event("builder-only", false, true)).is_err());
    let fallback = event("unknown", true, true);
    let dispose_fallback = events.register_fallback(fallback.clone()).unwrap();
    assert!(Object::is(&events.fallback_entry(), &fallback));
    assert!(
        events
            .register_fallback(event("other", true, true))
            .is_err()
    );
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    dispose_fallback.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(events.entries().length(), 0);
    assert!(events.fallback_entry().is_undefined());
    assert_eq!(changed.get(), 4);

    let views = WasmConversationViewRegistry::new();
    let definition = view("chat");
    let dispose = views.register(definition.clone()).unwrap();
    assert!(Object::is(&views.entries().get(0), &definition));
    assert!(views.register(view("chat")).is_err());
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(views.entries().length(), 0);
}

#[wasm_bindgen_test]
fn caller_bound_faces_route_registrations_through_effect_lifetime() {
    let events = WasmConversationEventRegistry::new();
    let event_face = events.face_for(caller()).unwrap();
    let register = get(&event_face, "register").dyn_into::<Function>().unwrap();
    let dispose = register
        .call1(&event_face, &event("message", true, true))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(events.entries().length(), 1);
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(events.entries().length(), 0);

    let views = WasmConversationViewRegistry::new();
    let view_face = views.face_for(caller()).unwrap();
    let register = get(&view_face, "register").dyn_into::<Function>().unwrap();
    let dispose = register
        .call1(&view_face, &view("chat"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(views.entries().length(), 1);
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(views.entries().length(), 0);
}
