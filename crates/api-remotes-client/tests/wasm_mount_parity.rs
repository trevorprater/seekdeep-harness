//! Live WASM contribution ordering, rollback, and disposal parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Promise, Reflect};
use seekdeep_api_remotes_client::{api_remotes_inject, apply_api_remotes, configure_api_remotes};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function apiRemotesBench(failAt) {
  const log = []
  const remote = {
    async $mount(contribution) {
      log.push('mount:' + contribution)
      if (contribution === failAt) throw new Error('mount failed: ' + contribution)
      return async () => { log.push('dispose:' + contribution) }
    },
  }
  const ctx = { get(name) { return name === 'remote' ? remote : undefined } }
  return { ctx, log }
}
export function apiRemotesLog(bench) { return bench.log }
"#)]
extern "C" {
    fn apiRemotesBench(fail_at: &str) -> JsValue;
    fn apiRemotesLog(bench: &JsValue) -> Array;
}

fn contributions() -> Array {
    ["commands", "goals", "dynamic", "inventory", "feedback"]
        .into_iter()
        .map(JsValue::from_str)
        .collect()
}

fn log(bench: &JsValue) -> Vec<String> {
    apiRemotesLog(bench)
        .iter()
        .filter_map(|value| value.as_string())
        .collect()
}

#[wasm_bindgen_test(async)]
async fn mounts_in_declaration_order_and_disposes_in_reverse() {
    assert!(configure_api_remotes(JsValue::from_str("abcde")).is_err());
    assert!(
        configure_api_remotes(
            Array::of2(&JsValue::from_str("one"), &JsValue::from_str("two")).into()
        )
        .is_err()
    );
    configure_api_remotes(contributions().into()).unwrap();
    assert_eq!(
        api_remotes_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["remote"]
    );
    let bench = apiRemotesBench("");
    let disposer = JsFuture::from(apply_api_remotes(
        Reflect::get(&bench, &JsValue::from_str("ctx")).unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(
        log(&bench),
        [
            "mount:commands",
            "mount:goals",
            "mount:dynamic",
            "mount:inventory",
            "mount:feedback",
        ]
    );
    let result = disposer
        .dyn_into::<js_sys::Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    JsFuture::from(Promise::resolve(&result)).await.unwrap();
    assert_eq!(
        &log(&bench)[5..],
        [
            "dispose:feedback",
            "dispose:inventory",
            "dispose:dynamic",
            "dispose:goals",
            "dispose:commands",
        ]
    );
}

#[wasm_bindgen_test(async)]
async fn a_mount_failure_rolls_back_only_committed_predecessors() {
    configure_api_remotes(contributions().into()).unwrap();
    let bench = apiRemotesBench("dynamic");
    assert!(
        JsFuture::from(apply_api_remotes(
            Reflect::get(&bench, &JsValue::from_str("ctx")).unwrap(),
        ))
        .await
        .is_err()
    );
    assert_eq!(
        log(&bench),
        [
            "mount:commands",
            "mount:goals",
            "mount:dynamic",
            "dispose:goals",
            "dispose:commands",
        ]
    );
}
