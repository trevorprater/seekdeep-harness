//! Clone-only DOM snapshot normalization parity.

#![cfg(target_arch = "wasm32")]

use seekdeep_client_test_runtime::{normalize_dom_snapshot, snapshot_needs_normalization};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeElement {
  constructor(tagName, attrs = {}, innerHTML = '', children = []) {
    this.tagName = tagName
    this.attrs = { ...attrs }
    this.innerHTML = innerHTML
    this.childNodes = innerHTML === '' ? [] : [{}]
    this.children = children
  }
  getAttribute(name) { return Object.hasOwn(this.attrs, name) ? this.attrs[name] : null }
  setAttribute(name, value) { this.attrs[name] = String(value) }
  replaceChildren() { this.innerHTML = ''; this.childNodes = [] }
  querySelectorAll(selector) {
    const all = []
    const visit = node => {
      for (const child of node.children) {
        if ((selector === '[class]' && child.getAttribute('class') !== null)
            || (selector === 'svg' && child.tagName.toLowerCase() === 'svg')) all.push(child)
        visit(child)
      }
    }
    visit(this)
    return all
  }
  cloneNode() {
    return new FakeElement(
      this.tagName,
      this.attrs,
      this.innerHTML,
      this.children.map(child => child.cloneNode(true)),
    )
  }
}

export function snapshotTree() {
  const filled = new FakeElement('svg', { class: '_icon_123abc' }, '<path d="M0 0"></path>')
  const empty = new FakeElement('svg', { class: 'plain' }, '')
  return new FakeElement('div', { class: '_frame_a1b2c3 foreign' }, '', [filled, empty])
}
export function snapshotSummary(root) {
  const svgs = root.querySelectorAll('svg')
  return {
    rootClass: root.getAttribute('class'),
    filledClass: svgs[0].getAttribute('class'),
    filledFingerprint: svgs[0].getAttribute('data-content'),
    filledChildren: svgs[0].childNodes.length,
    emptyFingerprint: svgs[1].getAttribute('data-content'),
    emptyChildren: svgs[1].childNodes.length,
  }
}
"#)]
extern "C" {
    fn snapshotTree() -> JsValue;
    fn snapshotSummary(root: &JsValue) -> JsValue;
}

fn string(value: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(value, &JsValue::from_str(key))
        .unwrap()
        .as_string()
}

#[wasm_bindgen_test]
fn normalizes_a_clone_and_preserves_the_live_tree_and_childless_svg() {
    let live = snapshotTree();
    assert!(snapshot_needs_normalization(live.clone()).unwrap());
    let clone = normalize_dom_snapshot(live.clone()).unwrap();
    let normalized = snapshotSummary(&clone);
    assert_eq!(
        string(&normalized, "rootClass").as_deref(),
        Some("frame foreign")
    );
    assert_eq!(string(&normalized, "filledClass").as_deref(), Some("icon"));
    assert_eq!(
        string(&normalized, "filledFingerprint").as_deref(),
        Some("66dc961a")
    );
    assert_eq!(
        js_sys::Reflect::get(&normalized, &JsValue::from_str("filledChildren"))
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
    assert!(string(&normalized, "emptyFingerprint").is_none());
    assert_eq!(
        js_sys::Reflect::get(&normalized, &JsValue::from_str("emptyChildren"))
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
    let original = snapshotSummary(&live);
    assert_eq!(
        string(&original, "rootClass").as_deref(),
        Some("_frame_a1b2c3 foreign")
    );
    assert_eq!(
        js_sys::Reflect::get(&original, &JsValue::from_str("filledChildren"))
            .unwrap()
            .as_f64(),
        Some(1.0)
    );
}
