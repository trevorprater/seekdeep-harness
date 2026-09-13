//! Live WASM coverage for compiled `JsonBlock` and literal `MessageText`.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_markdown_atoms, json_block_component, message_text_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let styles = []
let stringifyCount = 0

function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}
function text(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(text).join('')
}
function all(node, predicate, output = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output
  if (predicate(node)) output.push(node)
  for (const child of node.children ?? []) all(child, predicate, output)
  return output
}

export function installMarkdownAtomsBench() {
  hooks = []
  cursor = 0
  styles = []
  stringifyCount = 0
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(name, value) { this.attributes[name] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const React = {
    createElement(kind, props, ...children) {
      return { kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false) }
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => {
        hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update
      }]
    },
    useMemo(factory, dependencies) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) {
        hooks[index] = { value: factory(), dependencies: [...dependencies] }
      }
      return hooks[index].value
    },
  }
  return { React }
}
export function atomRender(component, props) { cursor = 0; return component(props) }
export function atomUnmount() { hooks = [] }
export function atomObject(entries) { return Object.fromEntries(entries) }
export function atomFindKind(tree, kind) { return all(tree, node => node.kind === kind)[0] }
export function atomFindButton(tree) { return all(tree, node => node.kind === 'button')[0] }
export function atomText(tree) { return text(tree) }
export function atomClick(node) { node.props.onClick() }
export function atomStyles() { return styles }
export function atomCircular() { const value = {}; value.self = value; return value }
export function atomUndefined() { return undefined }
export function atomCountingPayload() { return { toJSON() { stringifyCount += 1; return { counted: true } } } }
export function atomStringifyCount() { return stringifyCount }
export function atomBig() { return 'x'.repeat(30_000) }
export function atomSurrogatePayload() {
  const value = function () {}
  value.toString = () => 'x'.repeat(19_999) + '😀'
  return value
}
export function atomCustomFooter() { return total => 'TOTAL:' + total }
export function atomTextDetails(tree) {
  const value = text(tree)
  return { splitUnit: value.charCodeAt(19_999), value }
}
"#)]
extern "C" {
    fn installMarkdownAtomsBench() -> JsValue;
    fn atomRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn atomUnmount();
    fn atomObject(entries: &Array) -> JsValue;
    fn atomFindKind(tree: &JsValue, kind: &str) -> JsValue;
    fn atomFindButton(tree: &JsValue) -> JsValue;
    fn atomText(tree: &JsValue) -> String;
    fn atomClick(node: &JsValue);
    fn atomStyles() -> Array;
    fn atomCircular() -> JsValue;
    fn atomUndefined() -> JsValue;
    fn atomCountingPayload() -> JsValue;
    fn atomStringifyCount() -> u32;
    fn atomBig() -> JsValue;
    fn atomSurrogatePayload() -> JsValue;
    fn atomCustomFooter() -> Function;
    fn atomTextDetails(tree: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    atomObject(&values).unchecked_into()
}

fn setup() -> (JsValue, JsValue) {
    let bench = installMarkdownAtomsBench();
    let react = property(&bench, "React");
    configure_client_ui_primitive_markdown_atoms(react.clone()).unwrap();
    configure_client_ui_primitive_markdown_atoms(react).unwrap();
    (
        json_block_component().unwrap(),
        message_text_component().unwrap(),
    )
}

fn render(component: &JsValue, props: &Object) -> JsValue {
    atomRender(component, props.as_ref())
}

#[wasm_bindgen_test]
fn message_text_is_literal_and_styles_are_deduplicated() {
    let (_json, message) = setup();
    assert_eq!(atomStyles().length(), 2);
    let tree = render(
        &message,
        &props(&[("text", JsValue::from_str("# line1\n`line2`"))]),
    );
    assert_eq!(atomText(&tree), "# line1\n`line2`");
    assert_eq!(property(&tree, "kind").as_string().as_deref(), Some("div"));
    assert!(atomFindKind(&tree, "h1").is_undefined());
    atomUnmount();
}

#[wasm_bindgen_test]
fn json_block_collapses_toggles_defaults_open_and_memoizes_stringify() {
    let (json, _message) = setup();
    let payload = atomCountingPayload();
    let closed = props(&[
        ("label", JsValue::from_str("args")),
        ("payload", payload.clone()),
    ]);
    let tree = render(&json, &closed);
    assert_eq!(atomStringifyCount(), 0);
    assert!(atomFindKind(&tree, "pre").is_undefined());
    assert!(atomText(&tree).starts_with("▸ args"));
    atomClick(&atomFindButton(&tree));
    let tree = render(&json, &closed);
    assert_eq!(atomStringifyCount(), 1);
    assert!(atomText(&atomFindKind(&tree, "pre")).contains("\"counted\": true"));
    let _ = render(&json, &closed);
    assert_eq!(atomStringifyCount(), 1);
    atomClick(&atomFindButton(&tree));
    assert!(atomFindKind(&render(&json, &closed), "pre").is_undefined());
    atomUnmount();

    let (json, _message) = setup();
    let opened = props(&[
        ("label", JsValue::from_str("args")),
        (
            "payload",
            Array::of2(&JsValue::from_f64(1.0), &JsValue::from_f64(2.0)).into(),
        ),
        ("defaultOpen", JsValue::TRUE),
    ]);
    assert!(atomText(&atomFindKind(&render(&json, &opened), "pre")).contains('1'));
    atomUnmount();
}

#[wasm_bindgen_test]
fn json_fallback_truncation_and_utf16_slice_match_javascript() {
    let (json, _message) = setup();
    let undefined = props(&[
        ("label", JsValue::from_str("x")),
        ("payload", atomUndefined()),
        ("defaultOpen", JsValue::TRUE),
    ]);
    assert_eq!(
        atomText(&atomFindKind(&render(&json, &undefined), "pre")),
        "undefined"
    );
    atomUnmount();

    let (json, _message) = setup();
    let circular = props(&[
        ("label", JsValue::from_str("x")),
        ("payload", atomCircular()),
        ("defaultOpen", JsValue::TRUE),
    ]);
    assert_eq!(
        atomText(&atomFindKind(&render(&json, &circular), "pre")),
        "[object Object]"
    );
    atomUnmount();

    let (json, _message) = setup();
    let big = props(&[
        ("label", JsValue::from_str("x")),
        ("payload", atomBig()),
        ("defaultOpen", JsValue::TRUE),
    ]);
    let body = atomText(&atomFindKind(&render(&json, &big), "pre"));
    assert!(body.contains("… 已截断，共 30002 字符"));
    atomUnmount();

    let (json, _message) = setup();
    let split = props(&[
        ("label", JsValue::from_str("x")),
        ("payload", atomSurrogatePayload()),
        ("defaultOpen", JsValue::TRUE),
        ("truncatedLabel", atomCustomFooter().into()),
    ]);
    let body = atomFindKind(&render(&json, &split), "pre");
    let details = atomTextDetails(&body);
    assert_eq!(
        property(&details, "splitUnit").as_f64(),
        Some(f64::from(0xD83D_u16))
    );
    assert!(
        property(&details, "value")
            .as_string()
            .unwrap()
            .ends_with("\nTOTAL:20001")
    );
    atomUnmount();
}
