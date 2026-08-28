//! Live WASM coverage for Rust-owned highlighting aliases, lazy loading, and token shaping.

#![cfg(target_arch = "wasm32")]

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_highlight, grammar_load_count, highlight_aliases,
    highlight_lines, highlight_to_html, lazy_grammar_ids, subscribe_grammar_loaded,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let state
let timers

export function installHighlightBench() {
  state = { warm: 0, warmed: false, failWarm: false, loads: [], html: [], tokens: [], malformed: false }
  timers = []
  globalThis.setTimeout = (callback, delay) => { timers.push({ callback, delay }); return timers.length }
  const escape = value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  const backend = {
    warm() { if (state.failWarm) throw new Error('warm failed'); if (!state.warmed) { state.warmed = true; state.warm += 1 } },
    loadGrammar(id) { state.loads.push(id); return Promise.resolve(id) },
    codeToHtml(code, id) {
      state.html.push([code, id])
      return '<pre class="shiki css-variables"><code><span style="color:var(--shiki-token-keyword)">' + escape(code) + '</span></code></pre>'
    },
    codeToTokens(code, id) {
      state.tokens.push([code, id])
      const tokens = String(code).split('\n').map(line => line === '' ? [] : [{ content: line, ...(state.malformed ? {} : { color: 'var(--shiki-token-keyword)' }) }])
      return { tokens }
    },
  }
  return { backend, state }
}
export function highlightFireWarmup() { for (const timer of timers.splice(0)) timer.callback() }
export function highlightFireFailingWarmup() {
  state.failWarm = true
  try { for (const timer of timers.splice(0)) timer.callback() } catch (error) { return String(error) }
  return undefined
}
export function highlightTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function highlightSetMalformed(value) { state.malformed = value }
export function highlightLoads() { return state.loads }
export function highlightWarmCount() { return state.warm }
export function highlightCalls(kind) { return state[kind] }
"#)]
extern "C" {
    fn installHighlightBench() -> JsValue;
    fn highlightFireWarmup();
    fn highlightFireFailingWarmup() -> JsValue;
    fn highlightTick() -> js_sys::Promise;
    fn highlightSetMalformed(value: bool);
    fn highlightLoads() -> Array;
    fn highlightWarmCount() -> u32;
    fn highlightCalls(kind: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn setup() {
    let bench = installHighlightBench();
    configure_client_ui_primitive_highlight(property(&bench, "backend")).unwrap();
}

async fn tick() {
    JsFuture::from(highlightTick()).await.unwrap();
}

#[wasm_bindgen_test]
fn boot_aliases_unknown_values_and_deferred_warmup_match_source_policy() {
    setup();
    assert_eq!(highlightWarmCount(), 0);
    highlightFireWarmup();
    assert_eq!(highlightWarmCount(), 1);

    for alias in [
        "typescript",
        "ts",
        "tsx",
        "javascript",
        "js",
        "jsx",
        "shellscript",
        "bash",
        "sh",
        "shell",
        "zsh",
        "json",
        "jsonc",
    ] {
        let html = highlight_to_html("const x = 1".to_owned(), Some(alias.to_owned())).unwrap();
        assert!(
            html.as_string()
                .unwrap()
                .contains("pre class=\"shiki css-variables\"")
        );
    }
    for unknown in [None, Some("cobol"), Some("constructor"), Some("__proto__")] {
        assert!(
            highlight_to_html("x".to_owned(), unknown.map(str::to_owned))
                .unwrap()
                .is_undefined()
        );
    }
    assert_eq!(highlightCalls("html").length(), 13);
    assert_eq!(grammar_load_count().unwrap(), 0);

    let aliases = highlight_aliases();
    assert_eq!(
        property(aliases.as_ref(), "rs").as_string().as_deref(),
        Some("rust")
    );
    assert_eq!(lazy_grammar_ids().length(), 23);
}

#[wasm_bindgen_test]
fn deferred_warmup_surfaces_backend_failures() {
    setup();
    assert!(
        highlightFireFailingWarmup()
            .as_string()
            .unwrap()
            .contains("warm failed")
    );
}

#[wasm_bindgen_test]
fn line_tokens_preserve_text_styles_and_trailing_terminator_contract() {
    setup();
    let lines = Array::from(
        &highlight_lines("const a = 1\n// c".to_owned(), Some("ts".to_owned())).unwrap(),
    );
    assert_eq!(lines.length(), 2);
    let first = Array::from(&lines.get(0));
    assert_eq!(
        property(&first.get(0), "text").as_string().as_deref(),
        Some("const a = 1")
    );
    assert!(
        property(&property(&first.get(0), "style"), "color")
            .as_string()
            .unwrap()
            .contains("var(--shiki-")
    );

    let terminated =
        Array::from(&highlight_lines("a\n".to_owned(), Some("ts".to_owned())).unwrap());
    assert_eq!(terminated.length(), 1);
    let blank = Array::from(&highlight_lines("a\n\n".to_owned(), Some("ts".to_owned())).unwrap());
    assert_eq!(blank.length(), 2);
    assert_eq!(Array::from(&blank.get(1)).length(), 0);

    highlightSetMalformed(true);
    let error = highlight_lines("x".to_owned(), Some("ts".to_owned())).unwrap_err();
    assert!(format!("{error:?}").contains("color"));
}

#[wasm_bindgen_test(async)]
async fn lazy_grammars_request_once_publish_load_count_and_notify_subscribers() {
    setup();
    let notifications = Rc::new(Cell::new(0_u32));
    let observed = notifications.clone();
    let listener = Closure::wrap(Box::new(move || {
        assert!(grammar_load_count().unwrap() > 0);
        observed.set(observed.get() + 1);
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .dyn_into::<Function>()
    .unwrap();
    let dispose = subscribe_grammar_loaded(listener.clone()).unwrap();
    let duplicate_dispose = subscribe_grammar_loaded(listener).unwrap();
    let aliases = [
        "py", "rb", "go", "rs", "java", "c", "cpp", "cs", "kotlin", "swift", "php", "yaml", "toml",
        "ini", "md", "mdx", "html", "css", "scss", "less", "sql", "xml", "lua",
    ];
    for alias in aliases {
        assert!(
            highlight_to_html("x".to_owned(), Some(alias.to_owned()))
                .unwrap()
                .is_undefined()
        );
        assert!(
            highlight_to_html("x".to_owned(), Some(alias.to_owned()))
                .unwrap()
                .is_undefined()
        );
    }
    assert_eq!(highlightWarmCount(), 1);
    assert_eq!(highlightLoads().length(), 23);
    tick().await;
    assert_eq!(grammar_load_count().unwrap(), 23);
    assert_eq!(notifications.get(), 23);
    for alias in aliases {
        assert!(
            highlight_to_html("x".to_owned(), Some(alias.to_owned()))
                .unwrap()
                .as_string()
                .unwrap()
                .contains("shiki")
        );
    }
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    duplicate_dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(highlightLoads().length(), 23);
}

#[wasm_bindgen_test(async)]
async fn subscriber_iteration_matches_live_javascript_set_semantics() {
    setup();
    let calls = Rc::new(RefCell::new(Vec::new()));

    let third_calls = calls.clone();
    let third = Closure::wrap(Box::new(move || {
        third_calls.borrow_mut().push("third");
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .dyn_into::<Function>()
    .unwrap();

    let second_calls = calls.clone();
    let second = Closure::wrap(Box::new(move || {
        second_calls.borrow_mut().push("second");
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .dyn_into::<Function>()
    .unwrap();

    let second_disposer = Rc::new(RefCell::new(None::<Function>));
    let first_disposer_slot = second_disposer.clone();
    let first_calls = calls.clone();
    let first_third = third.clone();
    let first = Closure::wrap(Box::new(move || {
        first_calls.borrow_mut().push("first");
        if let Some(dispose) = first_disposer_slot.borrow_mut().take() {
            dispose.call0(&JsValue::UNDEFINED).unwrap();
        }
        let _ = subscribe_grammar_loaded(first_third.clone()).unwrap();
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .dyn_into::<Function>()
    .unwrap();

    let dispose_first = subscribe_grammar_loaded(first).unwrap();
    *second_disposer.borrow_mut() = Some(subscribe_grammar_loaded(second).unwrap());
    assert!(
        highlight_to_html("x".to_owned(), Some("py".to_owned()))
            .unwrap()
            .is_undefined()
    );
    tick().await;
    assert_eq!(&*calls.borrow(), &["first", "third"]);

    dispose_first.call0(&JsValue::UNDEFINED).unwrap();
    subscribe_grammar_loaded(third)
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
}
