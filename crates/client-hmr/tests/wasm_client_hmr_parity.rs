//! Browser-executed swap ordering, failure window, and `EventSource` ownership.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_hmr::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn bench() -> JsValue {
    Function::new_no_args(
        r#"
const order = [];
let failPrefetch = false;
const oldFiber = { runtime: { callback: {} }, inertia: undefined };
const entry = {
  options: { name: 'a' },
  fiber: oldFiber,
  ctx: {
    registry: {
      delete(callback) {
        order.push('delete-runtime');
        oldFiber.inertia = Promise.resolve().then(() => {
          order.push('drained');
          oldFiber.inertia = undefined;
        });
      },
    },
  },
  async refresh() {
    order.push('refresh');
    entry.fiber = {
      runtime: { callback: {} },
      inertia: undefined,
      async await() { order.push('await-new'); },
    };
  },
};
const loader = { entries() { return [entry]; } };
const modules = {
  invalidate(id) { order.push(`invalidate:${id}`); },
  async prefetch(id) {
    order.push(`prefetch:${id}`);
    if (failPrefetch) throw new Error('prefetch failed');
  },
};
return {
  loader,
  modules,
  entry,
  oldFiber,
  order,
  failPrefetch() { failPrefetch = true; },
};
"#,
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap()
}

fn field<T: JsCast>(value: &JsValue, name: &str) -> T {
    Reflect::get(value, &JsValue::from_str(name))
        .unwrap()
        .dyn_into::<T>()
        .unwrap_or_else(|_| panic!("{name} has the wrong JavaScript type"))
}

fn style(plugin: &str) -> web_sys::Element {
    let element = web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .create_element("style")
        .unwrap();
    element.set_attribute("data-plugin", plugin).unwrap();
    web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .head()
        .unwrap()
        .append_child(&element)
        .unwrap();
    element
}

#[wasm_bindgen_test]
async fn wasm_swap_orders_registry_teardown_styles_refresh_and_failure_window() {
    let state = bench();
    let platform = WasmClientHmrPlatform::new(
        field::<Object>(&state, "loader").into(),
        field::<Object>(&state, "modules").into(),
    );
    let owned = style("a");
    let foreign = style("other");
    platform.reload("a".to_owned()).await.unwrap();
    let order: Array = field(&state, "order");
    assert_eq!(
        order
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        [
            "invalidate:a",
            "prefetch:a",
            "delete-runtime",
            "drained",
            "refresh",
            "await-new",
        ]
    );
    assert!(!owned.is_connected());
    assert!(foreign.is_connected());
    let entry: Object = field(&state, "entry");
    let current = Reflect::get(&entry, &JsValue::from_str("fiber")).unwrap();
    let old = Reflect::get(&state, &JsValue::from_str("oldFiber")).unwrap();
    assert!(!Object::is(&current, &old));

    let failed = bench();
    let fail_prefetch: Function = field(&failed, "failPrefetch");
    fail_prefetch.call0(&failed).unwrap();
    let platform = WasmClientHmrPlatform::new(
        field::<Object>(&failed, "loader").into(),
        field::<Object>(&failed, "modules").into(),
    );
    let retained_style = style("a");
    assert!(platform.reload("a".to_owned()).await.is_err());
    let order: Array = field(&failed, "order");
    assert_eq!(
        order
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["invalidate:a", "prefetch:a"]
    );
    let entry: Object = field(&failed, "entry");
    assert!(Object::is(
        &Reflect::get(&entry, &JsValue::from_str("fiber")).unwrap(),
        &Reflect::get(&failed, &JsValue::from_str("oldFiber")).unwrap()
    ));
    assert!(retained_style.is_connected());
    foreign.remove();
    retained_style.remove();
}

#[wasm_bindgen_test]
async fn plugin_owns_event_source_parses_frames_and_closes_on_context_dispose() {
    let state = bench();
    let global = js_sys::global();
    let original = Reflect::get(&global, &JsValue::from_str("EventSource")).unwrap();
    let fake = Function::new_no_args(
        r#"
return class FakeEventSource {
  constructor(url) { this.url = url; this.closed = false; globalThis.__fakeSource = this; }
  emit(data) { this.onmessage?.({ data }); }
  close() { this.closed = true; }
};
"#,
    )
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    Reflect::set(&global, &JsValue::from_str("EventSource"), &fake).unwrap();
    let ctx = Function::new_with_args(
        "loader,modules",
        r#"
return {
  effects: [],
  get(name) { return name === 'loader' ? loader : name === 'modules' ? modules : undefined; },
  effect(installer, label) {
    this.label = label;
    const dispose = installer();
    this.effects.push(dispose);
    return dispose;
  },
};
"#,
    )
    .call2(
        &JsValue::UNDEFINED,
        &field::<Object>(&state, "loader"),
        &field::<Object>(&state, "modules"),
    )
    .unwrap();
    let plugin = client_hmr_plugin().unwrap();
    let inject: Array = field(&plugin, "inject");
    assert_eq!(
        inject
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["loader", "modules"]
    );
    let apply: Function = field(&plugin, "apply");
    apply.call1(&plugin, &ctx).unwrap();
    assert_eq!(
        Reflect::get(&ctx, &JsValue::from_str("label"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("client-hmr: event source")
    );
    let source = Reflect::get(&global, &JsValue::from_str("__fakeSource")).unwrap();
    assert_eq!(
        Reflect::get(&source, &JsValue::from_str("url"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("/plugins/events")
    );
    let emit: Function = field(&source, "emit");
    emit.call1(&source, &JsValue::from_str("not-json")).unwrap();
    emit.call1(
        &source,
        &JsValue::from_str("{\"type\":\"graph\",\"graph\":{}}"),
    )
    .unwrap();
    emit.call1(
        &source,
        &JsValue::from_str("{\"type\":\"rebuilt\",\"id\":\"a\",\"rev\":\"2\"}"),
    )
    .unwrap();
    JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(
        Function::new_no_args("return new Promise(resolve => setTimeout(resolve, 0));")
            .call0(&JsValue::UNDEFINED)
            .unwrap()
            .dyn_into::<js_sys::Promise>()
            .unwrap(),
    )
    .await
    .unwrap();
    let order: Array = field(&state, "order");
    assert!(
        order
            .iter()
            .filter_map(|value| value.as_string())
            .any(|value| value == "await-new")
    );
    let effects: Array = field(&ctx, "effects");
    effects
        .get(0)
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(
        Reflect::get(&source, &JsValue::from_str("closed"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    Reflect::set(&global, &JsValue::from_str("EventSource"), &original).unwrap();
}
