//! Live JavaScript coverage for the compiled clipboard boundary.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_primitives::{
    ANCHORED_MARGIN_PX, COPIED_FEEDBACK_MS, POINTER_GRACE_MS, configure_client_ui_primitive_hooks,
    copied_feedback_ms, pointer_grace_ms, use_anchored_max_height, use_copy_feedback,
    use_pointer_grace, write_clipboard,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let state

export function installClipboard(mode) {
  const children = []
  state = { calls: [], children, selected: undefined }
  const clipboard = mode === 'async-ok' ? {
    writeText(text) { state.calls.push(['writeText', text]); return Promise.resolve() },
  } : mode === 'async-reject' ? {
    writeText(text) { state.calls.push(['writeText', text]); return Promise.reject(new Error('denied')) },
  } : mode === 'async-missing' ? {} : undefined
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { clipboard } })
  const execCommand = mode === 'exec-ok' ? function(command) {
    state.calls.push(['execCommand', command])
    state.selected = children[0]?.value
    return true
  } : mode === 'exec-false' ? function(command) {
    state.calls.push(['execCommand', command]); return false
  } : mode === 'exec-throw' ? function(command) {
    state.calls.push(['execCommand', command]); throw new Error('denied')
  } : undefined
  globalThis.document = {
    execCommand,
    body: { appendChild(node) { children.push(node) } },
    createElement(kind) {
      return {
        kind, value: '', attributes: {}, style: {},
        setAttribute(key, value) { this.attributes[key] = value },
        select() { state.calls.push(['select']) },
        remove() { const at = children.indexOf(this); if (at >= 0) children.splice(at, 1) },
      }
    },
  }
  return state
}

export function clipboardCalls() { return state.calls }
export function clipboardChildren() { return state.children.length }
export function clipboardSelected() { return state.selected }

function sameDeps(left, right) {
  return left !== undefined && left.length === right.length && left.every((value, index) => Object.is(value, right[index]))
}

export function makeHookBench() {
  const slots = []
  let cursor = 0
  let now = 0
  let nextTimer = 1
  const timers = new Map()
  const listeners = new Map()
  const original = {
    window: globalThis.window,
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
    addEventListener: globalThis.addEventListener,
    removeEventListener: globalThis.removeEventListener,
  }
  function installEffect(index, effect, deps) {
    const previous = slots[index]
    if (previous !== undefined && sameDeps(previous.deps, deps)) return
    if (typeof previous?.cleanup === 'function') previous.cleanup()
    slots[index] = { deps: [...deps], cleanup: effect() }
  }
  const React = {
    useState(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = { kind: 'state', value: initial }
      return [slots[index].value, value => {
        slots[index].value = typeof value === 'function' ? value(slots[index].value) : value
      }]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = { current: initial }
      return slots[index]
    },
    useCallback(callback, deps) {
      const index = cursor++
      const previous = slots[index]
      if (previous === undefined || !sameDeps(previous.deps, deps)) slots[index] = { callback, deps: [...deps] }
      return slots[index].callback
    },
    useEffect(effect, deps) { installEffect(cursor++, effect, deps) },
    useLayoutEffect(effect, deps) { installEffect(cursor++, effect, deps) },
  }
  globalThis.window = globalThis
  globalThis.setTimeout = (callback, delay) => {
    const id = nextTimer++
    timers.set(id, { callback, due: now + Number(delay) })
    return id
  }
  globalThis.clearTimeout = id => { timers.delete(id) }
  globalThis.addEventListener = (name, listener, capture = false) => {
    const key = `${name}:${Boolean(capture)}`
    let bucket = listeners.get(key)
    if (bucket === undefined) listeners.set(key, bucket = new Set())
    bucket.add(listener)
  }
  globalThis.removeEventListener = (name, listener, capture = false) => {
    listeners.get(`${name}:${Boolean(capture)}`)?.delete(listener)
  }
  const ref = { current: { bottom: 200, getBoundingClientRect() { return { bottom: this.bottom } } } }
  const counts = { first: 0, second: 0 }
  return {
    React, ref, counts,
    first() { counts.first += 1 },
    second() { counts.second += 1 },
    reset() { cursor = 0 },
    advance(delta) {
      now += delta
      let ran
      do {
        ran = false
        for (const [id, timer] of [...timers]) {
          if (timer.due <= now) { timers.delete(id); timer.callback(); ran = true }
        }
      } while (ran)
    },
    emit(name, capture = false) {
      for (const listener of listeners.get(`${name}:${Boolean(capture)}`) ?? []) listener()
    },
    listenerCount(name, capture = false) {
      return listeners.get(`${name}:${Boolean(capture)}`)?.size ?? 0
    },
    unmount() {
      for (const slot of slots) if (typeof slot?.cleanup === 'function') slot.cleanup()
      slots.length = 0
      globalThis.window = original.window
      globalThis.setTimeout = original.setTimeout
      globalThis.clearTimeout = original.clearTimeout
      globalThis.addEventListener = original.addEventListener
      globalThis.removeEventListener = original.removeEventListener
    },
  }
}

export function hookReset(bench) { bench.reset() }
export function hookAdvance(bench, delta) { bench.advance(delta) }
export function hookEmit(bench, name, capture) { bench.emit(name, capture) }
export function hookUnmount(bench) { bench.unmount() }
export function hookSetBottom(bench, bottom) { bench.ref.current.bottom = bottom }
export function hookSetNullRef(bench) { bench.ref.current = null }
export function hookListenerCount(bench, name, capture) { return bench.listenerCount(name, capture) }
export function hookTick() { return Promise.resolve().then(() => Promise.resolve()) }
"#)]
extern "C" {
    fn installClipboard(mode: &str) -> JsValue;
    fn clipboardCalls() -> Array;
    fn clipboardChildren() -> u32;
    fn clipboardSelected() -> JsValue;
    fn makeHookBench() -> JsValue;
    fn hookReset(bench: &JsValue);
    fn hookAdvance(bench: &JsValue, delta: u32);
    fn hookEmit(bench: &JsValue, name: &str, capture: bool);
    fn hookUnmount(bench: &JsValue);
    fn hookSetBottom(bench: &JsValue, bottom: f64);
    fn hookSetNullRef(bench: &JsValue);
    fn hookListenerCount(bench: &JsValue, name: &str, capture: bool) -> u32;
    fn hookTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).expect("property")
}

async fn accepted(text: &str) -> bool {
    JsFuture::from(write_clipboard(text.to_owned()))
        .await
        .expect("clipboard helper settles")
        .as_bool()
        .expect("boolean result")
}

#[wasm_bindgen_test(async)]
async fn async_clipboard_acceptance_and_refusal_are_exact() {
    installClipboard("async-ok");
    assert!(accepted("payload").await);
    let call = Array::from(&clipboardCalls().get(0));
    assert_eq!(call.get(0).as_string().as_deref(), Some("writeText"));
    assert_eq!(call.get(1).as_string().as_deref(), Some("payload"));

    installClipboard("async-reject");
    assert!(!accepted("payload").await);
    assert_eq!(clipboardCalls().length(), 1);
}

#[wasm_bindgen_test(async)]
async fn exec_fallback_selects_exact_text_reports_host_result_and_always_removes() {
    for (mode, expected) in [
        ("exec-ok", true),
        ("exec-false", false),
        ("exec-throw", false),
    ] {
        installClipboard(mode);
        assert_eq!(accepted("payload").await, expected, "{mode}");
        assert_eq!(clipboardChildren(), 0, "{mode}");
        assert_eq!(
            Array::from(&clipboardCalls().get(0))
                .get(0)
                .as_string()
                .as_deref(),
            Some("select"),
            "{mode}"
        );
        if mode == "exec-ok" {
            assert_eq!(clipboardSelected().as_string().as_deref(), Some("payload"));
            let exec = Array::from(&clipboardCalls().get(1));
            assert_eq!(exec.get(0).as_string().as_deref(), Some("execCommand"));
            assert_eq!(exec.get(1).as_string().as_deref(), Some("copy"));
        }
    }
}

#[wasm_bindgen_test(async)]
async fn missing_clipboard_paths_settle_false_without_creating_a_textarea() {
    for mode in ["none", "async-missing"] {
        installClipboard(mode);
        assert!(!accepted("payload").await, "{mode}");
        assert_eq!(clipboardCalls().length(), 0, "{mode}");
        assert_eq!(clipboardChildren(), 0, "{mode}");
    }
}

#[wasm_bindgen_test]
fn pointer_grace_rearms_cancels_refreshes_close_and_cleans_up() {
    let bench = makeHookBench();
    configure_client_ui_primitive_hooks(property(&bench, "React"));
    hookReset(&bench);
    let first = property(&bench, "first").dyn_into::<Function>().unwrap();
    let grace = use_pointer_grace(first).unwrap();
    let arm = property(&grace, "arm").dyn_into::<Function>().unwrap();
    let cancel = property(&grace, "cancel").dyn_into::<Function>().unwrap();
    arm.call0(&JsValue::UNDEFINED).unwrap();
    hookAdvance(&bench, POINTER_GRACE_MS - 1);
    assert_eq!(
        property(&property(&bench, "counts"), "first").as_f64(),
        Some(0.0)
    );
    hookAdvance(&bench, 1);
    assert_eq!(
        property(&property(&bench, "counts"), "first").as_f64(),
        Some(1.0)
    );

    arm.call0(&JsValue::UNDEFINED).unwrap();
    hookAdvance(&bench, 150);
    arm.call0(&JsValue::UNDEFINED).unwrap();
    hookAdvance(&bench, 50);
    assert_eq!(
        property(&property(&bench, "counts"), "first").as_f64(),
        Some(1.0)
    );
    cancel.call0(&JsValue::UNDEFINED).unwrap();
    hookAdvance(&bench, POINTER_GRACE_MS * 2);
    assert_eq!(
        property(&property(&bench, "counts"), "first").as_f64(),
        Some(1.0)
    );

    hookReset(&bench);
    let second = property(&bench, "second").dyn_into::<Function>().unwrap();
    let refreshed = use_pointer_grace(second).unwrap();
    assert!(Object::is(
        &property(&grace, "arm"),
        &property(&refreshed, "arm")
    ));
    arm.call0(&JsValue::UNDEFINED).unwrap();
    hookAdvance(&bench, POINTER_GRACE_MS);
    assert_eq!(
        property(&property(&bench, "counts"), "second").as_f64(),
        Some(1.0)
    );
    arm.call0(&JsValue::UNDEFINED).unwrap();
    hookUnmount(&bench);
    hookAdvance(&bench, POINTER_GRACE_MS);
    assert_eq!(
        property(&property(&bench, "counts"), "second").as_f64(),
        Some(1.0)
    );
    assert_eq!(pointer_grace_ms(), 200);
}

#[wasm_bindgen_test(async)]
async fn copy_feedback_only_announces_accepted_writes_for_one_second() {
    installClipboard("async-ok");
    let bench = makeHookBench();
    configure_client_ui_primitive_hooks(property(&bench, "React"));
    hookReset(&bench);
    let first = use_copy_feedback("exact".to_owned()).unwrap();
    assert_eq!(property(&first, "copied").as_bool(), Some(false));
    property(&first, "onCopy")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    JsFuture::from(hookTick()).await.unwrap();
    hookReset(&bench);
    let copied = use_copy_feedback("exact".to_owned()).unwrap();
    assert_eq!(property(&copied, "copied").as_bool(), Some(true));
    property(&copied, "onCopy")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(clipboardCalls().length(), 1);
    hookAdvance(&bench, COPIED_FEEDBACK_MS);
    hookReset(&bench);
    assert_eq!(
        property(&use_copy_feedback("exact".to_owned()).unwrap(), "copied").as_bool(),
        Some(false)
    );
    hookUnmount(&bench);

    installClipboard("async-reject");
    let refused = makeHookBench();
    configure_client_ui_primitive_hooks(property(&refused, "React"));
    hookReset(&refused);
    let state = use_copy_feedback("exact".to_owned()).unwrap();
    property(&state, "onCopy")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    JsFuture::from(hookTick()).await.unwrap();
    hookReset(&refused);
    assert_eq!(
        property(&use_copy_feedback("exact".to_owned()).unwrap(), "copied").as_bool(),
        Some(false)
    );
    hookUnmount(&refused);
    assert_eq!(copied_feedback_ms(), 1_000);
}

#[wasm_bindgen_test]
#[allow(clippy::float_cmp)]
fn anchored_height_measures_clamps_reacts_to_events_and_disposes() {
    let bench = makeHookBench();
    configure_client_ui_primitive_hooks(property(&bench, "React"));
    hookReset(&bench);
    assert_eq!(
        use_anchored_max_height(property(&bench, "ref"), 300.0, JsValue::from_str("a")).unwrap(),
        300.0
    );
    hookReset(&bench);
    assert_eq!(
        use_anchored_max_height(property(&bench, "ref"), 300.0, JsValue::from_str("a")).unwrap(),
        200.0 - ANCHORED_MARGIN_PX
    );
    assert_eq!(hookListenerCount(&bench, "resize", false), 1);
    assert_eq!(hookListenerCount(&bench, "scroll", true), 1);
    hookSetBottom(&bench, 5.0);
    hookEmit(&bench, "scroll", true);
    hookReset(&bench);
    assert_eq!(
        use_anchored_max_height(property(&bench, "ref"), 300.0, JsValue::from_str("a")).unwrap(),
        0.0
    );
    hookSetBottom(&bench, 900.0);
    hookEmit(&bench, "resize", false);
    hookReset(&bench);
    assert_eq!(
        use_anchored_max_height(property(&bench, "ref"), 300.0, JsValue::from_str("a")).unwrap(),
        300.0
    );
    hookUnmount(&bench);
    assert_eq!(hookListenerCount(&bench, "resize", false), 0);
    assert_eq!(hookListenerCount(&bench, "scroll", true), 0);

    let closed = makeHookBench();
    configure_client_ui_primitive_hooks(property(&closed, "React"));
    hookSetNullRef(&closed);
    hookReset(&closed);
    assert_eq!(
        use_anchored_max_height(property(&closed, "ref"), 240.0, JsValue::NULL).unwrap(),
        240.0
    );
    assert_eq!(hookListenerCount(&closed, "resize", false), 0);
    hookUnmount(&closed);
}
