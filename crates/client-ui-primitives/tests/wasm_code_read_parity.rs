//! Live WASM coverage for compiled `CodeBlock` and `ReadBlock` consumers.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Promise, Reflect};
use seekdeep_client_ui_primitives::{
    code_block_component, configure_client_ui_primitive_code_block,
    configure_client_ui_primitive_highlight, configure_client_ui_primitive_read_block,
    default_read_max_lines, read_block_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let dirty = false
let refs = []
let timers = []
let now = 0
let styles = []
let clipboardMode = 'resolve'
let clipboardCalls = []
let execCalls = []
let htmlCalls = 0
let tokenCalls = 0

function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}

function clearRefs() { for (const ref of refs.splice(0)) ref.current = null }
function attachRefs(node) {
  if (node === null || node === undefined || typeof node !== 'object') return
  if (node.props?.ref) { node.props.ref.current = node; refs.push(node.props.ref) }
  for (const child of node.children ?? []) attachRefs(child)
}
function text(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (node.props?.dangerouslySetInnerHTML?.__html) return node.props.dangerouslySetInnerHTML.__html.replace(/<[^>]+>/g, '')
  return (node.children ?? []).map(text).join('')
}
function all(node, predicate, output = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output
  if (predicate(node)) output.push(node)
  for (const child of node.children ?? []) all(child, predicate, output)
  return output
}
function createNode(kind, props, children) {
  return {
    kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false),
    querySelector(selector) { return selector === 'pre' ? all(this, node => node.kind === 'pre')[0] ?? null : null },
    get textContent() { return text(this) },
  }
}

export function installCodeReadBench() {
  hooks = []
  cursor = 0
  dirty = false
  refs = []
  timers = []
  now = 0
  styles = []
  clipboardMode = 'resolve'
  clipboardCalls = []
  execCalls = []
  htmlCalls = 0
  tokenCalls = 0
  globalThis.window = globalThis
  globalThis.setTimeout = (callback, delay) => { timers.push({ callback, at: now + Number(delay), active: true }); return timers.length }
  globalThis.clearTimeout = id => { if (timers[id - 1]) timers[id - 1].active = false }
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { clipboard: { writeText(value) {
      clipboardCalls.push(value)
      return clipboardMode === 'reject' ? Promise.reject(new Error('denied')) : Promise.resolve()
    } } },
  })
  globalThis.document = {
    body: { appendChild() {} },
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, value: '', style: {}, setAttribute(name, value) { this.attributes[name] = value }, select() {}, remove() {} } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
    execCommand(command) { execCalls.push(command); if (clipboardMode === 'exec-throw') throw new Error('denied'); return clipboardMode === 'exec' },
  }
  const backend = {
    warm() {},
    loadGrammar() { return Promise.resolve() },
    codeToHtml(code) { htmlCalls += 1; return '<pre class="shiki css-variables"><code><span style="color:var(--shiki-token-keyword)">' + String(code) + '</span></code></pre>' },
    codeToTokens(code) {
      tokenCalls += 1
      return { tokens: String(code).split('\n').map(line => {
        if (line === '') return []
        if (line.startsWith('const ')) return [
          { content: 'const', color: 'var(--shiki-token-keyword)' },
          { content: line.slice(5), color: 'var(--shiki-foreground)' },
        ]
        return [{ content: line, color: 'var(--shiki-foreground)' }]
      }) }
    },
  }
  const React = {
    createElement(kind, props, ...children) { return createNode(kind, props, children) },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => {
        const value = typeof update === 'function' ? update(hooks[index].value) : update
        if (!Object.is(value, hooks[index].value)) { hooks[index].value = value; dirty = true }
      }]
    },
    useRef(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { current: initial }; return hooks[index] },
    useMemo(factory, dependencies) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) {
        hooks[index] = { value: factory(), dependencies: [...dependencies] }
      }
      return hooks[index].value
    },
    useCallback(callback, dependencies) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) {
        hooks[index] = { value: callback, dependencies: [...dependencies] }
      }
      return hooks[index].value
    },
    useSyncExternalStore(subscribe, snapshot) {
      const index = cursor++
      if (!(index in hooks)) {
        const listener = () => { dirty = true }
        hooks[index] = { cleanup: subscribe(listener) }
      }
      return snapshot()
    },
  }
  return { React, backend, styles }
}
export function blockRender(component, props) {
  clearRefs(); cursor = 0; dirty = false
  const tree = component(props)
  attachRefs(tree)
  return tree
}
export function blockUnmount() { clearRefs(); for (const hook of hooks) hook?.cleanup?.(); hooks = [] }
export function blockObject(entries) { return Object.fromEntries(entries) }
export function blockLines(count, first = 1) { return Array.from({ length: count }, (_value, index) => ({ number: first + index, text: 'line ' + (first + index) })) }
export function blockLine(number, text) { return { number, text } }
export function blockFindKind(tree, kind) { return all(tree, node => node.kind === kind)[0] }
export function blockFindKinds(tree, kind) { return all(tree, node => node.kind === kind) }
export function blockDescendantKinds(tree, kind) { return (tree.children ?? []).flatMap(child => all(child, node => node.kind === kind)) }
export function blockFindClass(tree, className) { return all(tree, node => String(node.props?.className ?? '').split(/\s+/).includes(className))[0] }
export function blockFindButton(tree, label) { return all(tree, node => node.kind === 'button' && text(node) === label)[0] }
export function blockText(tree) { return text(tree) }
export function blockClick(node) { node.props?.onClick?.() }
export function blockHtml(tree) { return all(tree, node => node.props?.dangerouslySetInnerHTML)[0]?.props?.dangerouslySetInnerHTML?.__html }
export function blockSetClipboardMode(mode) {
  clipboardMode = mode
  if (mode === 'missing' || mode.startsWith('exec')) navigator.clipboard = undefined
  if (mode === 'exec-absent') document.execCommand = undefined
}
export function blockClipboardCalls() { return clipboardCalls }
export function blockExecCalls() { return execCalls }
export function blockAdvance(ms) { now += ms; for (const timer of timers) if (timer.active && timer.at <= now) { timer.active = false; timer.callback() } }
export function blockTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function blockStyles() { return styles }
export function blockHtmlCalls() { return htmlCalls }
export function blockTokenCalls() { return tokenCalls }
export function blockDirty() { return dirty }
"#)]
extern "C" {
    fn installCodeReadBench() -> JsValue;
    fn blockRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn blockUnmount();
    fn blockObject(entries: &Array) -> JsValue;
    fn blockLines(count: u32, first: u32) -> Array;
    fn blockLine(number: u32, text: &str) -> JsValue;
    fn blockFindKind(tree: &JsValue, kind: &str) -> JsValue;
    fn blockFindKinds(tree: &JsValue, kind: &str) -> Array;
    fn blockDescendantKinds(tree: &JsValue, kind: &str) -> Array;
    fn blockFindClass(tree: &JsValue, class_name: &str) -> JsValue;
    fn blockFindButton(tree: &JsValue, label: &str) -> JsValue;
    fn blockText(tree: &JsValue) -> String;
    fn blockClick(node: &JsValue);
    fn blockHtml(tree: &JsValue) -> JsValue;
    fn blockSetClipboardMode(mode: &str);
    fn blockClipboardCalls() -> Array;
    fn blockExecCalls() -> Array;
    fn blockAdvance(milliseconds: f64);
    fn blockTick() -> Promise;
    fn blockStyles() -> Array;
    fn blockHtmlCalls() -> u32;
    fn blockTokenCalls() -> u32;
    fn blockDirty() -> bool;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    blockObject(&array).unchecked_into()
}

fn setup() -> (JsValue, JsValue) {
    let bench = installCodeReadBench();
    configure_client_ui_primitive_highlight(property(&bench, "backend")).unwrap();
    configure_client_ui_primitive_code_block(property(&bench, "React")).unwrap();
    configure_client_ui_primitive_read_block(property(&bench, "React")).unwrap();
    (
        code_block_component().unwrap(),
        read_block_component().unwrap(),
    )
}

fn render(component: &JsValue, props: &Object) -> JsValue {
    blockRender(component, props.as_ref())
}

fn read_rows(tree: &JsValue) -> Vec<JsValue> {
    blockFindKinds(tree, "div")
        .iter()
        .filter(|node| {
            property(&property(node, "props"), "className")
                .as_string()
                .as_deref()
                == Some("seekdeep-primitive-read-block-line")
        })
        .collect()
}

async fn tick() {
    JsFuture::from(blockTick()).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn code_block_highlight_plain_copy_labels_and_refusal_match_source() {
    let (code, _read) = setup();
    assert_eq!(blockStyles().length(), 2);
    let highlighted = props(&[
        ("code", JsValue::from_str("const a = 1\n")),
        ("lang", JsValue::from_str("ts")),
        ("className", JsValue::from_str("caller")),
        ("copyLabel", JsValue::from_str("Copy")),
        ("copiedLabel", JsValue::from_str("Copied")),
    ]);
    let tree = render(&code, &highlighted);
    assert_eq!(blockHtmlCalls(), 1);
    assert!(
        blockHtml(&tree)
            .as_string()
            .unwrap()
            .contains("pre class=\"shiki css-variables\"")
    );
    assert!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );
    assert!(blockText(&tree).contains("ts"));
    blockClick(&blockFindButton(&tree, "Copy"));
    assert_eq!(
        blockClipboardCalls().get(0).as_string().as_deref(),
        Some("const a = 1")
    );
    tick().await;
    let tree = render(&code, &highlighted);
    assert_eq!(blockHtmlCalls(), 1);
    assert!(!blockFindButton(&tree, "Copied").is_undefined());
    blockClick(&blockFindButton(&tree, "Copied"));
    assert_eq!(blockClipboardCalls().length(), 1);
    blockAdvance(1_000.0);
    assert!(!blockFindButton(&render(&code, &highlighted), "Copy").is_undefined());

    blockUnmount();
    let (code, _read) = setup();
    let plain = props(&[
        ("code", JsValue::from_str("IDENTIFICATION DIVISION.")),
        ("lang", JsValue::from_str("cobol")),
        ("className", JsValue::from_str("")),
    ]);
    let tree = render(&code, &plain);
    assert!(blockHtml(&tree).is_undefined());
    assert_eq!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .as_deref(),
        Some("seekdeep-primitive-code-block-block md-code-block")
    );
    assert_eq!(
        blockText(&blockFindKind(&tree, "pre")),
        "IDENTIFICATION DIVISION."
    );
    blockSetClipboardMode("reject");
    blockClick(&blockFindButton(&tree, "复制"));
    tick().await;
    assert!(!blockFindButton(&render(&code, &plain), "复制").is_undefined());
    blockUnmount();

    let (code, _read) = setup();
    blockSetClipboardMode("exec-absent");
    let tree = render(&code, &plain);
    blockClick(&blockFindButton(&tree, "复制"));
    tick().await;
    assert!(!blockFindButton(&render(&code, &plain), "复制").is_undefined());
    assert!(blockFindButton(&render(&code, &plain), "复制成功").is_undefined());
    blockUnmount();
}

#[wasm_bindgen_test(async)]
async fn code_block_exec_fallback_matches_shared_clipboard_behavior() {
    let (code, _read) = setup();
    let plain = props(&[("code", JsValue::from_str("plain body"))]);
    blockSetClipboardMode("exec");
    let tree = render(&code, &plain);
    blockClick(&blockFindButton(&tree, "复制"));
    assert_eq!(blockExecCalls().get(0).as_string().as_deref(), Some("copy"));
    tick().await;
    assert!(!blockFindButton(&render(&code, &plain), "复制成功").is_undefined());
    blockUnmount();

    let (code, _read) = setup();
    blockSetClipboardMode("exec-throw");
    let tree = render(&code, &plain);
    blockClick(&blockFindButton(&tree, "复制"));
    tick().await;
    assert!(!blockFindButton(&render(&code, &plain), "复制").is_undefined());
    blockUnmount();
}

#[wasm_bindgen_test]
fn read_rows_banner_plain_highlight_and_height_cap_match_source() {
    let (_code, read) = setup();
    assert_eq!(default_read_max_lines(), 16);
    let basic = props(&[
        ("label", JsValue::from_str("src/a.ts")),
        ("lines", blockLines(3, 41).into()),
        ("totalLines", JsValue::from_f64(180.0)),
        ("lang", JsValue::from_str("ts")),
        ("className", JsValue::from_str("caller")),
    ]);
    let tree = render(&read, &basic);
    assert_eq!(blockTokenCalls(), 1);
    let rows = read_rows(&tree);
    assert_eq!(rows.len(), 3);
    assert_eq!(blockText(&rows[0]), "41line 41");
    assert!(blockText(&tree).contains("显示 3 / 180 行"));
    assert!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );
    let content = blockFindClass(&tree, "seekdeep-primitive-read-block-content");
    let spans = blockDescendantKinds(&content, "span");
    assert_eq!(spans.length(), 1);
    assert_eq!(
        property(&property(&spans.get(0), "props"), "key").as_f64(),
        Some(0.0)
    );
    let known = props(&[
        ("lines", Array::of1(&blockLine(1, "const a = 1")).into()),
        ("totalLines", JsValue::from_f64(1.0)),
        ("lang", JsValue::from_str("ts")),
    ]);
    let known_tree = render(&read, &known);
    let known_content = blockFindClass(&known_tree, "seekdeep-primitive-read-block-content");
    let spans = blockDescendantKinds(&known_content, "span");
    assert_eq!(spans.length(), 2);
    assert_eq!(
        property(&property(&spans.get(0), "props"), "key").as_f64(),
        Some(0.0)
    );
    assert_eq!(
        property(&property(&spans.get(1), "props"), "key").as_f64(),
        Some(1.0)
    );
    assert!(blockFindClass(&tree, "seekdeep-primitive-read-block-expand").is_undefined());

    let unknown = props(&[
        ("lines", blockLines(1, 1).into()),
        ("totalLines", JsValue::from_f64(1.0)),
        ("lang", JsValue::from_str("cobol")),
    ]);
    let tree = render(&read, &unknown);
    assert!(!blockText(&tree).contains("显示"));
    assert_eq!(
        blockDescendantKinds(
            &blockFindClass(&tree, "seekdeep-primitive-read-block-content"),
            "span"
        )
        .length(),
        0
    );
    blockUnmount();
}

#[wasm_bindgen_test]
fn read_height_cap_and_expand_accessibility_match_source() {
    let (_code, read) = setup();
    let capped = props(&[
        ("lines", blockLines(10, 1).into()),
        ("totalLines", JsValue::from_f64(10.0)),
        ("maxLines", JsValue::from_f64(4.0)),
    ]);
    let tree = render(&read, &capped);
    let rows = read_rows(&tree);
    assert_eq!(rows.len(), 4);
    assert_eq!(blockText(&rows[0]), "1line 1");
    assert_eq!(blockText(&rows[1]), "2line 2");
    assert_eq!(blockText(&rows[2]), "9line 9");
    assert_eq!(blockText(&rows[3]), "10line 10");
    assert_eq!(
        blockText(&blockFindClass(
            &tree,
            "seekdeep-primitive-read-block-label"
        )),
        ""
    );
    assert_eq!(
        blockText(&blockFindClass(&tree, "seekdeep-primitive-read-block-lang")),
        ""
    );
    let toggle = blockFindButton(&tree, "… 其余 6 行");
    assert_eq!(
        property(&property(&toggle, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&toggle, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("展开其余 6 行")
    );
    blockClick(&toggle);
    let token_calls = blockTokenCalls();
    let tree = render(&read, &capped);
    assert_eq!(blockTokenCalls(), token_calls);
    assert_eq!(read_rows(&tree).len(), 10);
    let collapse = blockFindButton(&tree, "收起");
    assert_eq!(
        property(&property(&collapse, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("收起内容")
    );
    assert_eq!(
        property(&property(&collapse, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    blockClick(&collapse);
    let token_calls = blockTokenCalls();
    assert_eq!(read_rows(&render(&read, &capped)).len(), 4);
    assert_eq!(blockTokenCalls(), token_calls);

    let head_only = props(&[
        ("lines", blockLines(5, 1).into()),
        ("totalLines", JsValue::from_f64(5.0)),
        ("maxLines", JsValue::from_f64(1.0)),
    ]);
    let tree = render(&read, &head_only);
    let rows = read_rows(&tree);
    assert_eq!(rows.len(), 1);
    assert_eq!(blockText(&rows[0]), "1line 1");
    assert!(!blockFindButton(&tree, "… 其余 4 行").is_undefined());

    let default_cap = props(&[
        ("lines", blockLines(default_read_max_lines() + 1, 1).into()),
        (
            "totalLines",
            JsValue::from_f64(f64::from(default_read_max_lines() + 1)),
        ),
    ]);
    let tree = render(&read, &default_cap);
    assert_eq!(
        read_rows(&tree).len(),
        usize::try_from(default_read_max_lines()).unwrap()
    );
    assert!(!blockFindButton(&tree, "… 其余 1 行").is_undefined());

    let fractional_cap = props(&[
        ("lines", blockLines(10, 1).into()),
        ("totalLines", JsValue::from_f64(10.0)),
        ("maxLines", JsValue::from_f64(4.5)),
    ]);
    let tree = render(&read, &fractional_cap);
    let rows = read_rows(&tree);
    assert_eq!(rows.len(), 5);
    assert_eq!(blockText(&rows[2]), "3line 3");
    assert_eq!(blockText(&rows[3]), "9line 9");
    assert!(!blockFindButton(&tree, "… 其余 5.5 行").is_undefined());

    let negative_cap = props(&[
        ("lines", blockLines(10, 1).into()),
        ("totalLines", JsValue::from_f64(10.0)),
        ("maxLines", JsValue::from_f64(-4.0)),
    ]);
    let tree = render(&read, &negative_cap);
    let rows = read_rows(&tree);
    assert_eq!(rows.len(), 8);
    assert_eq!(blockText(&rows[7]), "8line 8");
    assert!(!blockFindButton(&tree, "… 其余 14 行").is_undefined());
    blockUnmount();
}

#[wasm_bindgen_test(async)]
async fn read_copy_full_window_refusal_empty_guard_and_lazy_rehighlight_match_source() {
    let (_code, read) = setup();
    let capped = props(&[
        ("lines", blockLines(10, 1).into()),
        ("totalLines", JsValue::from_f64(10.0)),
        ("maxLines", JsValue::from_f64(4.0)),
    ]);
    let tree = render(&read, &capped);
    blockClick(&blockFindButton(&tree, "复制"));
    let expected = (1..=10)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        blockClipboardCalls().get(0).as_string().as_deref(),
        Some(expected.as_str())
    );
    tick().await;
    let tree = render(&read, &capped);
    let copied = blockFindButton(&tree, "复制成功");
    assert!(!copied.is_undefined());
    blockClick(&copied);
    assert_eq!(blockClipboardCalls().length(), 1);
    blockAdvance(1_000.0);
    assert!(!blockFindButton(&render(&read, &capped), "复制").is_undefined());
    blockUnmount();

    let (_code, read) = setup();
    blockSetClipboardMode("reject");
    let one = props(&[
        ("lines", blockLines(1, 1).into()),
        ("totalLines", JsValue::from_f64(1.0)),
    ]);
    let tree = render(&read, &one);
    blockClick(&blockFindButton(&tree, "复制"));
    tick().await;
    assert!(!blockFindButton(&render(&read, &one), "复制").is_undefined());

    let empty = props(&[
        ("lines", Array::new().into()),
        ("totalLines", JsValue::from_f64(0.0)),
    ]);
    assert!(blockFindButton(&render(&read, &empty), "复制").is_undefined());
    blockUnmount();

    let (_code, read) = setup();
    let lazy = props(&[
        ("lines", Array::of1(&blockLine(1, "def f(): pass")).into()),
        ("totalLines", JsValue::from_f64(1.0)),
        ("lang", JsValue::from_str("py")),
    ]);
    let first = render(&read, &lazy);
    assert!(!blockDirty());
    assert_eq!(
        blockDescendantKinds(
            &blockFindClass(&first, "seekdeep-primitive-read-block-content"),
            "span"
        )
        .length(),
        0
    );
    tick().await;
    assert!(blockDirty());
    let highlighted = render(&read, &lazy);
    assert!(
        blockDescendantKinds(
            &blockFindClass(&highlighted, "seekdeep-primitive-read-block-content"),
            "span"
        )
        .length()
            > 0
    );
    blockUnmount();
}
