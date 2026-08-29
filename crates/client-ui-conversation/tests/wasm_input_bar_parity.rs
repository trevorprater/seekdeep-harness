//! Live WASM coverage for the resident composer bar.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_input_bar, input_bar_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let bench
let hooks = []
let cursor = 0
let pendingEffects = []
let documentListeners = new Map()
let windowListeners = new Map()

function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}
function listen(table, type, fn) {
  const rows = table.get(type) ?? []
  rows.push(fn)
  table.set(type, rows)
}
function unlisten(table, type, fn) {
  const rows = table.get(type) ?? []
  const at = rows.indexOf(fn)
  if (at >= 0) rows.splice(at, 1)
}
function baseInput(over = {}) {
  return { draft: '', imageIds: [], draftRev: 0, phase: 'plain', occurrences: [], queue: [], ...over }
}
function textareaTarget(value = '', start = value.length, end = start) {
  return {
    value, selectionStart: start, selectionEnd: end, selectionDirection: 'none',
    focus(options) { bench.calls.push({ method: 'focus', options, receiver: this }) },
    setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; bench.calls.push({ method: 'selection', a, b }) },
  }
}

export function barSetup() {
  hooks = []; cursor = 0; pendingEffects = []; documentListeners = new Map(); windowListeners = new Map()
  bench = {
    input: baseInput(), notice: null, lexicon: new Map(), menuLauncher: null,
    session: { promptError: null, running: false, subagent: null, removed: false },
    projections: new Map(), calls: [], slots: [], attachments: new Map(), addResult: null,
    variant: 'composer', disabled: false, blocked: undefined, workspacePickerOpen: false,
  }
  const documentElement = {}
  const body = {}
  const document = {
    documentElement, body,
    head: { appendChild() {} },
    createElement() { return { setAttribute() {} } },
    querySelector() { return null },
    createRange() {
      return {
        setStart() {}, setEnd() {}, collapse() {},
        getBoundingClientRect() { return bench.rangeRect ?? { top: 0, bottom: 0 } },
      }
    },
    addEventListener(type, fn) { listen(documentListeners, type, fn) },
    removeEventListener(type, fn) { unlisten(documentListeners, type, fn) },
  }
  const window = {
    innerWidth: 1200, innerHeight: 800,
    addEventListener(type, fn) { listen(windowListeners, type, fn) },
    removeEventListener(type, fn) { unlisten(windowListeners, type, fn) },
  }
  globalThis.document = document
  globalThis.window = window
  globalThis.getComputedStyle = () => ({ lineHeight: '20px' })
  globalThis.requestAnimationFrame = fn => { fn(0); return 1 }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) {
      const node = { kind, props: props ?? {}, children }
      if (props?.ref !== undefined && props.ref !== null) {
        if (kind === 'textarea') {
          if (props.ref.current?.focus !== undefined) {
            const value = props.value ?? ''
            if (props.ref.current.value !== value) {
              props.ref.current.value = value
              props.ref.current.selectionStart = value.length
              props.ref.current.selectionEnd = value.length
            }
          } else props.ref.current = textareaTarget(props.value ?? '')
        } else if (props['data-input-scroll'] !== undefined) {
          if (props.ref.current?.listeners === undefined) {
            const listeners = new Map()
            props.ref.current = {
              scrollTop: 0, clientHeight: 100, scrollHeight: 100,
              closest() { return bench.host },
              addEventListener(type, fn) { listen(listeners, type, fn) },
              removeEventListener(type, fn) { unlisten(listeners, type, fn) },
              getBoundingClientRect() { return { top: 0, bottom: 100 } },
              listeners,
            }
          }
          bench.scroll = props.ref.current
        } else if (props['data-input-mirror'] !== undefined) {
          if (props.ref.current?.firstChild !== undefined) props.ref.current.firstChild.data = children[0] ?? ''
          else props.ref.current = { firstChild: { data: children[0] ?? '' } }
          bench.mirror = props.ref.current
        } else {
          props.ref.current = node
        }
      }
      return node
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: typeof initial === 'function' ? initial() : initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'ref', value: { current: initial } }
      return hooks[index].value
    },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'memo', value: factory(), deps: [...deps] }
      return hooks[index].value
    },
    useCallback(callback, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'callback', value: callback, deps: [...deps] }
      return hooks[index].value
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        pendingEffects.push({ index, effect, deps: [...deps] })
      }
    },
  }
  const uiPrimitives = {
    Tooltip: 'Tooltip', Toast: 'Toast', IconPlusOutline16: 'IconPlusOutline16',
    IconWarningOutline16: 'IconWarningOutline16', Menu: 'Menu', RiskConfirmation: 'RiskConfirmation',
    IconChevronDownOutline14: 'IconChevronDownOutline14',
  }
  const uiAttachment = {
    AttachmentRail: 'AttachmentRail', DropOverlay: 'DropOverlay', ImageLightbox: 'ImageLightbox',
  }
  const keyboard = {
    get snapshot() { return bench.input },
    setDraft(text, range) { bench.calls.push({ method: 'setDraft', text, range, receiver: this }); bench.input = baseInput({ ...bench.input, draft: text, draftRev: bench.input.draftRev + (text === bench.input.draft ? 0 : 1) }) },
    track(draft, caret) { bench.calls.push({ method: 'track', draft, caret, receiver: this }) },
    arbitrate(key, composing) { bench.calls.push({ method: 'arbitrate', key, composing, receiver: this }); return bench.arbitration ?? 'pass' },
    steerQueue() { bench.calls.push({ method: 'steerQueue', receiver: this }) },
    submit(mode) { bench.calls.push({ method: 'submit', mode, receiver: this }) },
    undo() { bench.calls.push({ method: 'undo' }) }, redo() { bench.calls.push({ method: 'redo' }) },
    space() { bench.calls.push({ method: 'space' }); return bench.spaceConsumed ?? false },
    dismissPopup() { bench.calls.push({ method: 'dismissPopup' }) },
    pasteBegin(text, selection) { bench.calls.push({ method: 'pasteBegin', text, selection }); bench.input = baseInput({ ...bench.input, draft: bench.input.draft.slice(0, selection.start) + text + bench.input.draft.slice(selection.end), draftRev: bench.input.draftRev + 1, paste: { attemptId: 1, insertedRange: { start: selection.start, end: selection.start + text.length }, generation: 0 } }) },
    invalidatePaste() { bench.calls.push({ method: 'invalidatePaste' }); const { paste, ...rest } = bench.input; bench.input = rest },
  }
  const inputActions = {
    setDraft(text) { keyboard.setDraft(text) },
    addImages(ids) { bench.calls.push({ method: 'actionAddImages', ids }); bench.input = baseInput({ ...bench.input, imageIds: [...bench.input.imageIds, ...ids] }); return true },
    removeImage(id) { bench.calls.push({ method: 'actionRemoveImage', id }) },
    pruneImages(ids) { bench.calls.push({ method: 'pruneImages', ids }) },
    submit() { bench.calls.push({ method: 'actionSubmit' }) },
  }
  function useValue(value, selector) { return selector(value) }
  const props = {
    useInput: selector => useValue(bench.input, selector),
    useNotices: selector => useValue(bench.notice, selector),
    useLexicon: selector => useValue(bench.lexicon, selector),
    useMenuLauncher: selector => useValue(bench.menuLauncher, selector),
    useSession: selector => useValue(bench.session, selector),
    useProjection(key, selector) { const value = bench.projections.get(key); return selector === undefined ? value : selector(value) },
    inputActions, keyboard,
    addImages(files) { bench.calls.push({ method: 'addImages', files }); return bench.addResult },
    removeImage(id) { bench.calls.push({ method: 'removeImage', id }) },
    draftImages(ids) { return ids.map(id => bench.attachments.get(id)).filter(Boolean) },
    resolveSubmitMode(running, gesture, steering) { bench.calls.push({ method: 'resolveSubmitMode', running, gesture, steering }); return running && steering && gesture === 'accelerated' ? 'steer' : 'queue' },
    toggleCommandMenu(selection) { bench.calls.push({ method: 'toggleCommandMenu', selection }) },
    stop() { bench.calls.push({ method: 'stop' }) },
    command(line) { bench.calls.push({ method: 'command', line }); return Promise.resolve(true) },
    t(key, vars) {
      const copy = {
        'input.commands': 'Commands', 'input.stop': 'Stop', 'input.send': 'Send',
        'placeholder.default': 'Send a message', 'placeholder.plan': 'Describe the task',
        'placeholder.unavailable': 'Unavailable', 'placeholder.parentOffline': 'Parent offline',
        'placeholder.steerQueue': 'Steer queued messages', 'hero.chooseWorkspace': 'Choose workspace',
        'image.pending': 'Pending image', 'image.openOriginal': 'Open original',
        'image.scrollLeft': 'Scroll left', 'image.scrollRight': 'Scroll right',
        'image.original': 'Original image', 'image.preview': 'Image preview',
        'image.closePreview': 'Close preview', 'image.dropTitle': 'Drop images',
        'image.dropBlocked': 'Images unavailable', 'image.unsupportedType': 'Unsupported image type',
      }
      if (key === 'image.remove') return `Remove ${vars.name}`
      if (key === 'image.tooMany') return `Maximum ${vars.count} images`
      if (key === 'image.fileTooLarge') return `Maximum ${vars.size}`
      if (key === 'image.totalTooLarge') return `Maximum total ${vars.size}`
      if (key === 'image.dropDesc') return `${vars.count} images, ${vars.size} each`
      return copy[key] ?? key
    },
    renderSlot(name, owner) { const node = { kind: `slot:${name}`, props: owner, children: [] }; bench.slots.push({ name, owner, node }); return node },
    sessionId: 's1', variant: 'composer', disabled: false,
    workspacePickerOpen: false,
    onRequestWorkspace() { bench.calls.push({ method: 'workspace' }) },
  }
  bench.React = React; bench.uiPrimitives = uiPrimitives; bench.uiAttachment = uiAttachment
  bench.props = props; bench.keyboard = keyboard; bench.inputActions = inputActions
  bench.document = document; bench.window = window
  bench.host = { scrollTop: 0 }
  return bench
}

export function barObject(entries) { return Object.fromEntries(entries) }
export function barRender(component) {
  cursor = 0; pendingEffects = []
  Object.assign(bench.props, { variant: bench.variant, disabled: bench.disabled, blocked: bench.blocked, workspacePickerOpen: bench.workspacePickerOpen })
  const tree = component(bench.props)
  for (const pending of pendingEffects) {
    hooks[pending.index]?.cleanup?.()
    hooks[pending.index] = { type: 'effect', deps: pending.deps, cleanup: pending.effect() }
  }
  return tree
}
export function barBench() { return bench }
export function barSetInput(value) { bench.input = baseInput(value) }
export function barSetSession(value) { bench.session = { ...bench.session, ...value } }
export function barSetProjection(key, value) { if (value === undefined) bench.projections.delete(key); else bench.projections.set(key, value) }
export function barSetLexicon(slash, at) { bench.lexicon = new Map([['/', slash], ['@', at]]) }
export function barSetMenu(value) { bench.menuLauncher = value }
export function barSetNotice(value) { bench.notice = value }
export function barSetVariant(value) { bench.variant = value }
export function barSetDisabled(value) { bench.disabled = value }
export function barSetBlocked(value) { bench.blocked = value }
export function barSetProp(key, value) { bench.props[key] = value }
export function barSetArbitration(value) { bench.arbitration = value }
export function barSetSpace(value) { bench.spaceConsumed = value }
export function barSetAddResult(value) { bench.addResult = value }
export function barAddAttachment(id, name, type, size) { bench.attachments.set(id, { kind: 'image', id, file: { name, type, size }, previewUrl: `blob:${id}` }) }
export function barClearCalls() { bench.calls = []; bench.slots = [] }
export function barCalls() { return bench.calls }
export function barKey(key, options = {}) {
  return { key, shiftKey: false, metaKey: false, ctrlKey: false, repeat: false, nativeEvent: { isComposing: false, keyCode: 0 }, prevented: 0, preventDefault() { this.prevented += 1 }, ...options }
}
export function barChange(value, caret = value.length) { const target = textareaTarget(value, caret, caret); return { target } }
export function barClipboard(text, items = [], start = 0, end = 0) {
  const data = new Map([['text/plain', text]])
  const clipboardData = { items, getData(type) { return data.get(type) ?? '' }, setData(type, value) { data.set(type, value) }, value(type) { return data.get(type) } }
  const currentTarget = textareaTarget(bench.input.draft, start, end)
  return { clipboardData, currentTarget, prevented: 0, preventDefault() { this.prevented += 1 } }
}
export function barFileItem(file) { return { kind: 'file', getAsFile() { return file } } }
export function barFile(name, type, size) { return { name, type, size } }
export function barDispatchDocument(type, event) { for (const fn of [...(documentListeners.get(type) ?? [])]) fn(event) }
export function barSetScroll(top, clientHeight, scrollHeight) { bench.scroll.scrollTop = top; bench.scroll.clientHeight = clientHeight; bench.scroll.scrollHeight = scrollHeight }
export function barSetRange(top, bottom) { bench.rangeRect = { top, bottom } }
export function barWheel(deltaY) { const event = { deltaY, prevented: 0, preventDefault() { this.prevented += 1 } }; for (const fn of [...(bench.scroll.listeners.get('wheel') ?? [])]) fn(event); return event }
export function barDrag(files, options = {}) {
  return { dataTransfer: { types: ['Files'], files, dropEffect: '' }, target: bench.document.body, clientX: 10, clientY: 10, prevented: 0, preventDefault() { this.prevented += 1 }, ...options }
}
export function barFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = barFindKind(child, kind); if (found) return found }
  return undefined
}
export function barFindClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if ((value.props?.className ?? '').split(' ').includes(className)) return value
  for (const child of value.children ?? []) { const found = barFindClass(child, className); if (found) return found }
  return undefined
}
export function barFindData(value, key, expected) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.props?.[key] === expected) return value
  for (const child of value.children ?? []) { const found = barFindData(child, key, expected); if (found) return found }
  return undefined
}
export function barText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(barText).join('')
  return barText(value.children)
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = barSetup)]
    fn bar_setup() -> JsValue;
    #[wasm_bindgen(js_name = barObject)]
    fn bar_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = barRender)]
    fn bar_render(component: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = barBench)]
    fn bar_bench() -> JsValue;
    #[wasm_bindgen(js_name = barSetInput)]
    fn bar_set_input(value: &JsValue);
    #[wasm_bindgen(js_name = barSetSession)]
    fn bar_set_session(value: &JsValue);
    #[wasm_bindgen(js_name = barSetProjection)]
    fn bar_set_projection(key: &str, value: JsValue);
    #[wasm_bindgen(js_name = barSetLexicon)]
    fn bar_set_lexicon(slash: &Array, at: &Array);
    #[wasm_bindgen(js_name = barSetMenu)]
    fn bar_set_menu(value: JsValue);
    #[wasm_bindgen(js_name = barSetNotice)]
    fn bar_set_notice(value: JsValue);
    #[wasm_bindgen(js_name = barSetVariant)]
    fn bar_set_variant(value: &str);
    #[wasm_bindgen(js_name = barSetDisabled)]
    fn bar_set_disabled(value: bool);
    #[wasm_bindgen(js_name = barSetBlocked)]
    fn bar_set_blocked(value: JsValue);
    #[wasm_bindgen(js_name = barSetProp)]
    fn bar_set_prop(key: &str, value: JsValue);
    #[wasm_bindgen(js_name = barSetArbitration)]
    fn bar_set_arbitration(value: &str);
    #[wasm_bindgen(js_name = barSetSpace)]
    fn bar_set_space(value: bool);
    #[wasm_bindgen(js_name = barSetAddResult)]
    fn bar_set_add_result(value: JsValue);
    #[wasm_bindgen(js_name = barAddAttachment)]
    fn bar_add_attachment(id: &str, name: &str, media_type: &str, size: f64);
    #[wasm_bindgen(js_name = barClearCalls)]
    fn bar_clear_calls();
    #[wasm_bindgen(js_name = barCalls)]
    fn bar_calls() -> Array;
    #[wasm_bindgen(js_name = barKey)]
    fn bar_key(key: &str, options: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = barChange)]
    fn bar_change(value: &str, caret: f64) -> JsValue;
    #[wasm_bindgen(js_name = barClipboard)]
    fn bar_clipboard(text: &str, items: &Array, start: u32, end: u32) -> JsValue;
    #[wasm_bindgen(js_name = barFileItem)]
    fn bar_file_item(file: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = barFile)]
    fn bar_file(name: &str, media_type: &str, size: f64) -> JsValue;
    #[wasm_bindgen(js_name = barDispatchDocument)]
    fn bar_dispatch_document(kind: &str, event: &JsValue);
    #[wasm_bindgen(js_name = barSetScroll)]
    fn bar_set_scroll(top: f64, client_height: f64, scroll_height: f64);
    #[wasm_bindgen(js_name = barSetRange)]
    fn bar_set_range(top: f64, bottom: f64);
    #[wasm_bindgen(js_name = barWheel)]
    fn bar_wheel(delta_y: f64) -> JsValue;
    #[wasm_bindgen(js_name = barDrag)]
    fn bar_drag(files: &Array, options: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = barFindKind)]
    fn bar_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = barFindClass)]
    fn bar_find_class(value: &JsValue, class_name: &str) -> JsValue;
    #[wasm_bindgen(js_name = barFindData)]
    fn bar_find_data(value: &JsValue, key: &str, expected: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = barText)]
    fn bar_text(value: &JsValue) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key))
        .unwrap_or_else(|error| panic!("property {key:?} on {value:?} failed: {error:?}"))
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    bar_object(&array).unchecked_into()
}

fn call(target: &JsValue, method: &str, arguments: &[JsValue]) -> JsValue {
    let function = property(target, method).dyn_into::<Function>().unwrap();
    let arguments: Array = arguments.iter().collect();
    function.apply(target, &arguments).unwrap()
}

fn entries_by_method(method: &str) -> Vec<JsValue> {
    bar_calls()
        .iter()
        .filter(|call| property(call, "method").as_string().as_deref() == Some(method))
        .collect()
}

fn call_methods() -> Vec<String> {
    bar_calls()
        .iter()
        .filter_map(|call| property(&call, "method").as_string())
        .collect()
}

fn setup() -> JsValue {
    let bench = bar_setup();
    configure_client_ui_conversation_input_bar(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
        property(&bench, "uiAttachment"),
    )
    .unwrap();
    input_bar_component().unwrap()
}

fn textarea(tree: &JsValue) -> JsValue {
    bar_find_kind(tree, "textarea")
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One deterministic runtime exercises the full composer contract serially.
fn compiled_input_bar_runs_render_keyboard_clipboard_attachment_and_slot_matrix() {
    let component = setup();
    let tree = bar_render(&component);
    let input = textarea(&tree);
    assert_eq!(
        property(&property(&input, "props"), "placeholder")
            .as_string()
            .as_deref(),
        Some("Send a message")
    );
    assert_eq!(
        property(&property(&input, "props"), "disabled").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&input, "props"), "readOnly").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&input, "props"), "data-phase")
            .as_string()
            .as_deref(),
        Some("plain")
    );
    assert!(!bar_find_kind(&tree, "slot:conversation.input.plan").is_undefined());
    assert!(!bar_find_kind(&tree, "slot:conversation.input.model").is_undefined());

    bar_clear_calls();
    let shift = bar_key("Enter", object(&[("shiftKey", JsValue::TRUE)]).as_ref());
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&shift),
    );
    assert_eq!(property(&shift, "prevented").as_f64(), Some(0.0));
    assert!(entries_by_method("submit").is_empty());
    let plain = bar_key("Enter", &JsValue::UNDEFINED);
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&plain),
    );
    assert_eq!(property(&plain, "prevented").as_f64(), Some(1.0));
    assert_eq!(entries_by_method("submit").len(), 1);
    assert_eq!(
        property(&entries_by_method("submit")[0], "mode")
            .as_string()
            .as_deref(),
        Some("queue")
    );
    bar_set_arbitration("consumed");
    let up = bar_key("ArrowUp", &JsValue::UNDEFINED);
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&up),
    );
    assert_eq!(property(&up, "prevented").as_f64(), Some(1.0));

    bar_clear_calls();
    let change = bar_change("hello", 5.0);
    call(
        &property(&input, "props"),
        "onChange",
        std::slice::from_ref(&change),
    );
    assert_eq!(
        property(&entries_by_method("setDraft")[0], "text")
            .as_string()
            .as_deref(),
        Some("hello")
    );
    assert_eq!(
        property(&entries_by_method("track")[0], "caret").as_f64(),
        Some(5.0)
    );

    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("A \u{fffc} B")),
            ("draftRev", JsValue::from_f64(2.0)),
            ("phase", JsValue::from_str("plain")),
            ("imageIds", Array::new().into()),
            ("queue", Array::new().into()),
            (
                "occurrences",
                Array::of1(&object(&[
                    ("occurrenceId", JsValue::from_f64(1.0)),
                    ("source", JsValue::from_str("skills")),
                    ("ref", JsValue::from_str("item")),
                    ("offset", JsValue::from_f64(2.0)),
                    ("label", JsValue::from_str("/item")),
                    ("clipboardText", JsValue::from_str("/item")),
                ]))
                .into(),
            ),
        ])
        .as_ref(),
    );
    let tree = bar_render(&component);
    let input = textarea(&tree);
    let copy = bar_clipboard("", &Array::new(), 0, 5);
    call(
        &property(&input, "props"),
        "onCopy",
        std::slice::from_ref(&copy),
    );
    assert_eq!(property(&copy, "prevented").as_f64(), Some(1.0));
    assert_eq!(
        call(
            &property(&copy, "clipboardData"),
            "value",
            &[JsValue::from_str("text/plain")],
        )
        .as_string()
        .as_deref(),
        Some("A /item B")
    );

    bar_clear_calls();
    let file = bar_file("a.png", "image/png", 4.0);
    let paste = bar_clipboard(" pasted", &Array::of1(&bar_file_item(&file)), 5, 5);
    call(
        &property(&input, "props"),
        "onPaste",
        std::slice::from_ref(&paste),
    );
    assert_eq!(
        entries_by_method("addImages").len(),
        1,
        "calls: {:?}, prevented: {:?}, disabled: {:?}, readOnly: {:?}",
        call_methods(),
        property(&paste, "prevented").as_f64(),
        property(&property(&input, "props"), "disabled").as_bool(),
        property(&property(&input, "props"), "readOnly").as_bool(),
    );
    assert_eq!(entries_by_method("pasteBegin").len(), 1);
    assert_eq!(property(&paste, "prevented").as_f64(), Some(1.0));

    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("")),
            ("draftRev", JsValue::from_f64(0.0)),
            ("phase", JsValue::from_str("plain")),
            ("imageIds", Array::new().into()),
            (
                "queue",
                Array::of1(&object(&[
                    ("id", JsValue::from_str("q1")),
                    ("placement", JsValue::from_str("queued")),
                ]))
                .into(),
            ),
            ("occurrences", Array::new().into()),
        ])
        .as_ref(),
    );
    bar_set_session(object(&[("running", JsValue::TRUE)]).as_ref());
    bar_set_arbitration("pass");
    let tree = bar_render(&component);
    let input = textarea(&tree);
    assert_eq!(
        property(&property(&input, "props"), "placeholder")
            .as_string()
            .as_deref(),
        Some("Steer queued messages")
    );
    bar_clear_calls();
    let accelerated = bar_key("Enter", object(&[("metaKey", JsValue::TRUE)]).as_ref());
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&accelerated),
    );
    assert_eq!(entries_by_method("steerQueue").len(), 1);

    bar_set_variant("hero");
    bar_set_disabled(true);
    let tree = bar_render(&component);
    let input = textarea(&tree);
    assert_eq!(
        property(&property(&input, "props"), "readOnly").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&property(&input, "props"), "aria-haspopup")
            .as_string()
            .as_deref(),
        Some("menu")
    );
    let choose = bar_key("Enter", &JsValue::UNDEFINED);
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&choose),
    );
    assert_eq!(entries_by_method("workspace").len(), 1);

    bar_set_disabled(false);
    bar_set_variant("composer");
    bar_set_notice(
        object(&[
            ("level", JsValue::from_str("error")),
            ("text", JsValue::from_str("failed")),
            ("seq", JsValue::from_f64(1.0)),
        ])
        .into(),
    );
    bar_add_attachment("i1", "a.png", "image/png", 4.0);
    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("")),
            ("draftRev", JsValue::from_f64(0.0)),
            ("phase", JsValue::from_str("plain")),
            ("imageIds", Array::of1(&JsValue::from_str("i1")).into()),
            ("queue", Array::new().into()),
            ("occurrences", Array::new().into()),
        ])
        .as_ref(),
    );
    let tree = bar_render(&component);
    assert!(bar_text(&tree).contains("failed"));
    let rail = bar_find_kind(&tree, "AttachmentRail");
    assert_eq!(
        property(&property(&rail, "props"), "items")
            .unchecked_into::<Array>()
            .length(),
        1
    );
    call(
        &property(&rail, "props"),
        "onOpen",
        &[property(&property(&rail, "props"), "items")
            .unchecked_into::<Array>()
            .get(0)],
    );
    let tree = bar_render(&component);
    assert!(!bar_find_kind(&tree, "ImageLightbox").is_undefined());

    extended_keyboard_and_posture_matrix();
    attachment_drop_and_toast_matrix();
    decoration_selection_and_wheel_matrix();
}

#[allow(clippy::too_many_lines)]
fn extended_keyboard_and_posture_matrix() {
    let component = setup();
    let tree = bar_render(&component);
    assert_eq!(entries_by_method("focus").len(), 1);
    let input = textarea(&tree);
    bar_clear_calls();
    let composing = bar_key(
        "Enter",
        object(&[(
            "nativeEvent",
            object(&[
                ("isComposing", JsValue::TRUE),
                ("keyCode", JsValue::from_f64(0.0)),
            ])
            .into(),
        )])
        .as_ref(),
    );
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&composing),
    );
    assert!(entries_by_method("submit").is_empty());
    assert_eq!(property(&composing, "prevented").as_f64(), Some(0.0));
    let repeated = bar_key("Enter", object(&[("repeat", JsValue::TRUE)]).as_ref());
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&repeated),
    );
    assert_eq!(property(&repeated, "prevented").as_f64(), Some(1.0));
    assert!(entries_by_method("submit").is_empty());
    let undo = bar_key("z", object(&[("metaKey", JsValue::TRUE)]).as_ref());
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&undo),
    );
    assert_eq!(entries_by_method("undo").len(), 1);
    bar_set_space(true);
    let space = bar_key(" ", &JsValue::UNDEFINED);
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&space),
    );
    assert_eq!(entries_by_method("space").len(), 1);
    assert_eq!(property(&space, "prevented").as_f64(), Some(1.0));
    bar_set_arbitration("consumed");
    let escape = bar_key("Escape", &JsValue::UNDEFINED);
    call(
        &property(&input, "props"),
        "onKeyDown",
        std::slice::from_ref(&escape),
    );
    assert_eq!(entries_by_method("dismissPopup").len(), 1);
    assert_eq!(property(&escape, "prevented").as_f64(), Some(1.0));

    let component = setup();
    bar_set_projection(
        "plan",
        object(&[("active", JsValue::TRUE), ("pending", JsValue::FALSE)]).into(),
    );
    let tree = bar_render(&component);
    assert_eq!(
        property(&property(&textarea(&tree), "props"), "placeholder")
            .as_string()
            .as_deref(),
        Some("Describe the task")
    );
    bar_set_prop("placeholder", JsValue::from_str("Owner copy"));
    bar_set_prop("overlay", JsValue::NULL);
    let tree = bar_render(&component);
    assert_eq!(
        property(&property(&textarea(&tree), "props"), "placeholder")
            .as_string()
            .as_deref(),
        Some("Owner copy")
    );
    assert!(!bar_find_class(&tree, "seekdeep-conversation-inputBar-overlayAnchor").is_undefined());

    let component = setup();
    bar_set_session(
        object(&[
            ("running", JsValue::TRUE),
            (
                "subagent",
                object(&[
                    (
                        "address",
                        object(&[("mode", JsValue::from_str("continuable"))]).into(),
                    ),
                    ("parentAvailable", JsValue::TRUE),
                ])
                .into(),
            ),
        ])
        .as_ref(),
    );
    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("follow up")),
            ("phase", JsValue::from_str("plain")),
        ])
        .as_ref(),
    );
    let tree = bar_render(&component);
    let send = bar_find_data(&tree, "aria-label", &JsValue::from_str("Send"));
    let stop = bar_find_data(&tree, "aria-label", &JsValue::from_str("Stop"));
    assert!(!send.is_undefined());
    assert!(!stop.is_undefined());
    call(&property(&send, "props"), "onClick", &[]);
    call(&property(&stop, "props"), "onClick", &[]);
    assert_eq!(entries_by_method("actionSubmit").len(), 1);
    assert_eq!(entries_by_method("stop").len(), 1);
    bar_set_session(
        object(&[(
            "subagent",
            object(&[
                (
                    "address",
                    object(&[("mode", JsValue::from_str("continuable"))]).into(),
                ),
                ("parentAvailable", JsValue::FALSE),
            ])
            .into(),
        )])
        .as_ref(),
    );
    bar_set_prop("placeholder", JsValue::UNDEFINED);
    let tree = bar_render(&component);
    assert_eq!(
        property(&property(&textarea(&tree), "props"), "placeholder")
            .as_string()
            .as_deref(),
        Some("Parent offline")
    );
    bar_set_session(object(&[("subagent", JsValue::NULL), ("running", JsValue::FALSE)]).as_ref());
    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("locked")),
            ("phase", JsValue::from_str("submitting")),
        ])
        .as_ref(),
    );
    let tree = bar_render(&component);
    let input = textarea(&tree);
    assert_eq!(
        property(&property(&input, "props"), "readOnly").as_bool(),
        Some(true)
    );
    let send = bar_find_data(&tree, "aria-label", &JsValue::from_str("Send"));
    assert_eq!(
        property(&property(&send, "props"), "disabled").as_bool(),
        Some(true)
    );
}

#[allow(clippy::too_many_lines)]
fn attachment_drop_and_toast_matrix() {
    let component = setup();
    let limits = object(&[
        (
            "mediaTypes",
            Array::of1(&JsValue::from_str("image/png")).into(),
        ),
        ("maxImagesPerMessage", JsValue::from_f64(1.0)),
        ("maxImageBytes", JsValue::from_f64(1024.0)),
        ("maxMessageImageBytes", JsValue::from_f64(2048.0)),
    ]);
    bar_set_projection("imageLimits", limits.clone().into());
    bar_add_attachment("existing", "old.png", "image/png", 4.0);
    bar_set_input(
        object(&[(
            "imageIds",
            Array::of1(&JsValue::from_str("existing")).into(),
        )])
        .as_ref(),
    );
    let tree = bar_render(&component);
    let file = bar_file("new.png", "image/png", 4.0);
    let paste = bar_clipboard("", &Array::of1(&bar_file_item(&file)), 0, 0);
    call(
        &property(&textarea(&tree), "props"),
        "onPaste",
        std::slice::from_ref(&paste),
    );
    assert!(entries_by_method("addImages").is_empty());
    let tree = bar_render(&component);
    let toast = bar_find_kind(&tree, "Toast");
    assert_eq!(
        property(&property(&toast, "props"), "text")
            .as_string()
            .as_deref(),
        Some("Maximum 1 images")
    );

    let component = setup();
    bar_set_projection("imageLimits", limits.into());
    bar_set_add_result(JsValue::from_str("Unsupported from host"));
    let tree = bar_render(&component);
    let file = bar_file("bad.svg", "image/svg+xml", 4.0);
    let paste = bar_clipboard("", &Array::of1(&bar_file_item(&file)), 0, 0);
    call(
        &property(&textarea(&tree), "props"),
        "onPaste",
        std::slice::from_ref(&paste),
    );
    assert_eq!(entries_by_method("addImages").len(), 1);
    let tree = bar_render(&component);
    assert_eq!(
        property(&property(&bar_find_kind(&tree, "Toast"), "props"), "text")
            .as_string()
            .as_deref(),
        Some("Unsupported from host")
    );

    let component = setup();
    let _ = bar_render(&component);
    let file = bar_file("drop.png", "image/png", 4.0);
    let drag = bar_drag(&Array::of1(&file), &JsValue::UNDEFINED);
    bar_dispatch_document("dragenter", &drag);
    assert_eq!(property(&drag, "prevented").as_f64(), Some(1.0));
    let tree = bar_render(&component);
    assert!(!bar_find_kind(&tree, "DropOverlay").is_undefined());
    bar_clear_calls();
    let drop = bar_drag(&Array::of1(&file), &JsValue::UNDEFINED);
    bar_dispatch_document("drop", &drop);
    assert_eq!(entries_by_method("addImages").len(), 1);
    assert_eq!(property(&drop, "prevented").as_f64(), Some(1.0));

    let component = setup();
    bar_set_session(
        object(&[(
            "promptError",
            object(&[(
                "error",
                object(&[
                    ("code", JsValue::from_str("attachment-error")),
                    ("message", JsValue::from_str("bad")),
                    (
                        "details",
                        object(&[("reason", JsValue::from_str("INVALID_IMAGE"))]).into(),
                    ),
                ])
                .into(),
            )])
            .into(),
        )])
        .as_ref(),
    );
    let _ = bar_render(&component);
    let tree = bar_render(&component);
    assert_eq!(
        property(&property(&bar_find_kind(&tree, "Toast"), "props"), "text")
            .as_string()
            .as_deref(),
        Some("Unsupported image type")
    );
}

#[allow(clippy::too_many_lines)]
fn decoration_selection_and_wheel_matrix() {
    let component = setup();
    bar_set_lexicon(&Array::of1(&JsValue::from_str("skill")), &Array::new());
    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("/goal \u{fffc} /skill")),
            ("draftRev", JsValue::from_f64(3.0)),
            ("phase", JsValue::from_str("claimed")),
            (
                "claim",
                object(&[
                    ("token", JsValue::from_str("/goal ")),
                    ("hint", JsValue::from_str("describe goal")),
                ])
                .into(),
            ),
            (
                "occurrences",
                Array::of1(&object(&[
                    ("occurrenceId", JsValue::from_f64(7.0)),
                    ("source", JsValue::from_str("skills")),
                    ("ref", JsValue::from_str("item")),
                    ("offset", JsValue::from_f64(6.0)),
                    ("label", JsValue::from_str("/item")),
                    ("clipboardText", JsValue::from_str("/item")),
                    ("invalid", JsValue::TRUE),
                ]))
                .into(),
            ),
            (
                "paste",
                object(&[
                    ("attemptId", JsValue::from_f64(1.0)),
                    (
                        "insertedRange",
                        object(&[
                            ("start", JsValue::from_f64(0.0)),
                            ("end", JsValue::from_f64(1.0)),
                        ])
                        .into(),
                    ),
                    ("generation", JsValue::from_f64(0.0)),
                ])
                .into(),
            ),
        ])
        .as_ref(),
    );
    let tree = bar_render(&component);
    for decoration in ["token", "chip", "text-ref"] {
        assert!(
            !bar_find_data(&tree, "data-decoration", &JsValue::from_str(decoration)).is_undefined(),
            "{decoration}"
        );
    }
    let chip = bar_find_data(&tree, "data-decoration", &JsValue::from_str("chip"));
    assert!(
        property(&property(&chip, "props"), "className")
            .as_string()
            .unwrap()
            .contains("chipInvalid")
    );
    bar_clear_calls();
    let input = textarea(&tree);
    call(
        &property(&input, "props"),
        "onSelect",
        &[object(&[]).into()],
    );
    assert_eq!(entries_by_method("invalidatePaste").len(), 1);
    bar_set_input(
        object(&[
            ("draft", JsValue::from_str("/goal ")),
            ("phase", JsValue::from_str("claimed")),
            (
                "claim",
                object(&[
                    ("token", JsValue::from_str("/goal ")),
                    ("hint", JsValue::from_str("describe goal")),
                ])
                .into(),
            ),
        ])
        .as_ref(),
    );
    let tree = bar_render(&component);
    assert!(!bar_find_data(&tree, "data-decoration", &JsValue::from_str("hint")).is_undefined());

    let component = setup();
    let _ = bar_render(&component);
    bar_set_scroll(0.0, 100.0, 300.0);
    let inside = bar_wheel(20.0);
    assert_eq!(property(&inside, "prevented").as_f64(), Some(0.0));
    assert_eq!(
        property(&property(&bar_bench(), "host"), "scrollTop").as_f64(),
        Some(0.0)
    );
    bar_set_scroll(200.0, 100.0, 300.0);
    let chained = bar_wheel(20.0);
    assert_eq!(property(&chained, "prevented").as_f64(), Some(1.0));
    assert_eq!(
        property(&property(&bar_bench(), "host"), "scrollTop").as_f64(),
        Some(20.0)
    );

    let component = setup();
    let _ = bar_render(&component);
    bar_set_scroll(0.0, 100.0, 300.0);
    bar_set_range(90.0, 100.0);
    bar_set_input(object(&[("draft", JsValue::from_str("line\n"))]).as_ref());
    let _ = bar_render(&component);
    assert_eq!(
        property(&property(&bar_bench(), "scroll"), "scrollTop").as_f64(),
        Some(20.0)
    );
}
