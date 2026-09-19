//! Registrations reached through Cordis service tracing belong to the calling plugin.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_runtime::{
    WasmClientSlotRegistry, WasmConversationEventRegistry, WasmConversationViewRegistry,
};
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function ownerContextWrapper() {
  return core => new Proxy(core, {
    get(target, key, receiver) {
      if (key === 'get') return name => target.traceService(target.get(name), receiver)
      if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
      if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
      if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
      if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
      if (key === 'waterfall') return (...args) => target.eventArgs('waterfall', args)
      if (Reflect.has(target, key)) {
        const value = Reflect.get(target, key, receiver)
        return typeof value === 'function' ? value.bind(target) : value
      }
      const value = target.metaGet(key)
      if (value !== undefined) return value
      return typeof key === 'string' ? target.traceService(target.get(key), receiver) : undefined
    },
  })
}
export function registrationOwnerPlugin() {
  return {
    name: 'registration-owner',
    inject: ['slots', 'conversationEvents', 'conversationViews'],
    apply(ctx) {
      ctx.get('slots').register({name: 'owned-seat'}, 'child')
      ctx.get('conversationEvents').register({kind: 'owned-message'})
      ctx.get('conversationEvents').registerFallback({kind: 'owned-fallback', target: 'owned-view', buildViewNode() {}})
      ctx.get('conversationViews').register({target: 'owned-view'})
    },
  }
}
export function registerOwnerRoot(slots) {
  slots.register({name: 'root', children: {'owned-seat': {kind: 'single', scope: 'root'}}}, 'root')
}
"#)]
extern "C" {
    fn ownerContextWrapper() -> JsValue;
    fn registrationOwnerPlugin() -> JsValue;
    fn registerOwnerRoot(slots: &JsValue);
}

fn call(value: &JsValue, key: &str, args: &[JsValue]) -> JsValue {
    let method = Reflect::get(value, &key.into())
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let arguments = args.iter().cloned().collect::<Array>();
    method.apply(value, &arguments).unwrap()
}

#[wasm_bindgen_test(async)]
async fn plugin_disposal_releases_traced_registrations_and_allows_replacement() {
    configure_context_wrapper(ownerContextWrapper()).unwrap();
    let root = create_context().unwrap();
    let slots = WasmClientSlotRegistry::new(None);
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    let slots_face = slots.face_for(root.clone()).unwrap();
    for (name, face) in [
        ("slots", slots_face.clone()),
        ("conversationEvents", events.face_for(root.clone()).unwrap()),
        ("conversationViews", views.face_for(root.clone()).unwrap()),
    ] {
        call(&root, "provide", &[name.into(), face]);
    }
    registerOwnerRoot(&slots_face);
    let plugin = registrationOwnerPlugin();
    for _ in 0..2 {
        let fiber = call(&root, "plugin", std::slice::from_ref(&plugin));
        JsFuture::from(Promise::resolve(&call(&fiber, "await", &[])))
            .await
            .unwrap();
        assert_eq!(slots.entries("owned-seat".into()).length(), 1);
        assert_eq!(events.entries().length(), 1);
        assert!(!events.fallback_entry().is_undefined());
        assert_eq!(views.entries().length(), 1);
        JsFuture::from(Promise::resolve(&call(&fiber, "dispose", &[])))
            .await
            .unwrap();
        assert_eq!(slots.entries("root".into()).length(), 1);
        assert_eq!(slots.entries("owned-seat".into()).length(), 0);
        assert_eq!(events.entries().length(), 0);
        assert!(events.fallback_entry().is_undefined());
        assert_eq!(views.entries().length(), 0);
        assert!(call(&root, "get", &["slots".into()]).is_object());
    }
    let fiber = Reflect::get(&root, &"fiber".into()).unwrap();
    JsFuture::from(Promise::resolve(&call(&fiber, "dispose", &[])))
        .await
        .unwrap();
}
