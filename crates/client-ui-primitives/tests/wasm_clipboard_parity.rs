//! Live JavaScript coverage for the compiled clipboard boundary.

#![cfg(target_arch = "wasm32")]

use js_sys::Array;
use seekdeep_client_ui_primitives::write_clipboard;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
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
"#)]
extern "C" {
    fn installClipboard(mode: &str) -> JsValue;
    fn clipboardCalls() -> Array;
    fn clipboardChildren() -> u32;
    fn clipboardSelected() -> JsValue;
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
