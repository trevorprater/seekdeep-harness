//! Live WASM coverage for transient queue rendering, actions, and registration.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_queue_dock, queue_dock_component, queue_dock_entry_browser,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let nextId = 1
let session = { queue: [], running: true, subagent: null }
let updates = []
let notices = []
let updateMode = 'resolve'
let pendingUpdates = []
let injectCalls = []
let registerCalls = []
let translations = []
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
export function installQueueDockBench() {
  hooks = []; cursor = 0; nextId = 1; session = { queue: [], running: true, subagent: null }
  updates = []; notices = []; updateMode = 'resolve'; pendingUpdates = []; injectCalls = []; registerCalls = []; translations = []
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'memo', deps: [...deps], value: factory() }
      return hooks[index].value
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index]?.cleanup?.()
        hooks[index] = { type: 'effect', deps: [...deps], cleanup: effect() }
      }
    },
    useId() { const index = cursor++; if (!(index in hooks)) hooks[index] = { type: 'id', value: `queue-list-${nextId++}` }; return hooks[index].value },
  }
  const uiPrimitives = {
    Tooltip: 'Tooltip', IconCheckOutline16: 'IconCheckOutline16',
    IconChevronDownOutline14: 'IconChevronDownOutline14', IconChevronUpOutline14: 'IconChevronUpOutline14',
    IconCloseOutline16: 'IconCloseOutline16', IconEditOutline16: 'IconEditOutline16',
    IconQueueOutline14: 'IconQueueOutline14', IconSendOutline14: 'IconSendOutline14', IconTrashOutline16: 'IconTrashOutline16',
  }
  return { React, uiPrimitives }
}
export function queueDockObject(entries) { return Object.fromEntries(entries) }
export function queueDockSetSession(value) { session = value }
export function makeQueueDockUseSession() { return selector => selector(session) }
export function queueDockUpdate() {
  return (id, action) => {
    updates.push({ id, action })
    if (updateMode === 'resolve') return Promise.resolve()
    if (updateMode === 'reject') return Promise.reject(new Error('failed'))
    return new Promise((resolve, reject) => { pendingUpdates.push({ resolve, reject }) })
  }
}
export function queueDockNotify() { return (level, text) => { notices.push({ level, text }) } }
export function queueDockSetUpdateMode(mode) { updateMode = mode }
export function queueDockResolve() { pendingUpdates.shift()?.resolve() }
export function queueDockReject() { pendingUpdates.shift()?.reject(new Error('failed')) }
export function queueDockUpdates() { return updates }
export function queueDockNotices() { return notices }
export function makeQueueDockTranslate() {
  const copy = {
    'queue.edit': '编辑排队消息', 'queue.edit.unsupported': '包含非文本内容，暂不支持编辑',
    'queue.save': '保存排队消息', 'queue.cancelEdit': '取消编辑', 'queue.remove': '删除排队消息',
    'queue.steer': '插话发送', 'queue.steer.unavailable': '仅运行中可插话发送',
    'queue.editFailed': '编辑失败：这条消息可能已经开始发送。',
    'queue.removeFailed': '删除失败：这条消息可能已经开始发送。', 'queue.steerFailed': '插话发送失败，请重试。',
  }
  return (key, vars) => {
    translations.push(key)
    return key === 'queue.count' ? `${vars.n} 条排队消息` : copy[key] ?? key
  }
}
export function queueDockRender(component, props) { cursor = 0; return component(props) }
export function queueDockText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(queueDockText).join('')
  return queueDockText(value.children)
}
export function queueDockFindButton(value, label) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === 'button' && value.props?.['aria-label'] === label) return value
  for (const child of value.children ?? []) { const found = queueDockFindButton(child, label); if (found) return found }
  return undefined
}
export function queueDockFindAllButtons(value, label) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === 'button' && value.props?.['aria-label'] === label ? [value] : []
  return own.concat(...(value.children ?? []).map(child => queueDockFindAllButtons(child, label)))
}
export function queueDockFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = queueDockFindKind(child, kind); if (found) return found }
  return undefined
}
export function queueDockFindAllKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === kind ? [value] : []
  return own.concat(...(value.children ?? []).map(child => queueDockFindAllKind(child, kind)))
}
export function queueDockChangeEvent(value) { return { currentTarget: { value } } }
export function queueDockKeyEvent(key, composing = false) { return { key, nativeEvent: { isComposing: composing }, prevented: false, preventDefault() { this.prevented = true } } }
export function queueDockContext(mode = 'ok') {
  const conversation = {
    updateQueue(id, action) { updates.push({ id, action, injected: true }); return Promise.resolve() },
    input: { for(actx) { return { notify(level, text) { notices.push({ level, text, actx }) } } } },
  }
  const actx = { get(name) { return mode === 'no-conversation' ? undefined : name === 'conversation' ? conversation : undefined } }
  const sessions = { scope() { return mode === 'no-scope' ? undefined : actx } }
  const slots = {
    inject(name, callback) { injectCalls.push({ name }); return callback() },
    register(options, component) { registerCalls.push({ options, component }); return () => {} },
  }
  return { slots, sessions, actx }
}
export function queueDockInjectCalls() { return injectCalls }
export function queueDockRegisterCalls() { return registerCalls }
export function queueDockTranslations() { return translations }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installQueueDockBench)]
    fn install_queue_dock_bench() -> JsValue;
    #[wasm_bindgen(js_name = queueDockObject)]
    fn queue_dock_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = queueDockSetSession)]
    fn queue_dock_set_session(value: &JsValue);
    #[wasm_bindgen(js_name = makeQueueDockUseSession)]
    fn make_queue_dock_use_session() -> Function;
    #[wasm_bindgen(js_name = queueDockUpdate)]
    fn queue_dock_update() -> Function;
    #[wasm_bindgen(js_name = queueDockNotify)]
    fn queue_dock_notify() -> Function;
    #[wasm_bindgen(js_name = queueDockSetUpdateMode)]
    fn queue_dock_set_update_mode(mode: &str);
    #[wasm_bindgen(js_name = queueDockResolve)]
    fn queue_dock_resolve();
    #[wasm_bindgen(js_name = queueDockReject)]
    fn queue_dock_reject();
    #[wasm_bindgen(js_name = queueDockUpdates)]
    fn queue_dock_updates() -> Array;
    #[wasm_bindgen(js_name = queueDockNotices)]
    fn queue_dock_notices() -> Array;
    #[wasm_bindgen(js_name = makeQueueDockTranslate)]
    fn make_queue_dock_translate() -> Function;
    #[wasm_bindgen(js_name = queueDockRender)]
    fn queue_dock_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = queueDockText)]
    fn queue_dock_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = queueDockFindButton)]
    fn queue_dock_find_button(value: &JsValue, label: &str) -> JsValue;
    #[wasm_bindgen(js_name = queueDockFindAllButtons)]
    fn queue_dock_find_all_buttons(value: &JsValue, label: &str) -> Array;
    #[wasm_bindgen(js_name = queueDockFindKind)]
    fn queue_dock_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = queueDockFindAllKind)]
    fn queue_dock_find_all_kind(value: &JsValue, kind: &str) -> Array;
    #[wasm_bindgen(js_name = queueDockChangeEvent)]
    fn queue_dock_change_event(value: &str) -> JsValue;
    #[wasm_bindgen(js_name = queueDockKeyEvent)]
    fn queue_dock_key_event(key: &str, composing: bool) -> JsValue;
    #[wasm_bindgen(js_name = queueDockContext)]
    fn queue_dock_context(mode: &str) -> JsValue;
    #[wasm_bindgen(js_name = queueDockInjectCalls)]
    fn queue_dock_inject_calls() -> Array;
    #[wasm_bindgen(js_name = queueDockRegisterCalls)]
    fn queue_dock_register_calls() -> Array;
    #[wasm_bindgen(js_name = queueDockTranslations)]
    fn queue_dock_translations() -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap_or_else(|error| {
        panic!(
            "property {key:?} on {} failed: {error:?}",
            js_sys::JSON::stringify(value)
                .ok()
                .and_then(|text| text.as_string())
                .unwrap_or_else(|| format!("{value:?}"))
        )
    })
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    queue_dock_object(&array).unchecked_into()
}

fn row(id: &str, placement: &str, text: JsValue, preview: &str) -> Object {
    object(&[
        ("id", JsValue::from_str(id)),
        ("placement", JsValue::from_str(placement)),
        ("text", text),
        ("preview", JsValue::from_str(preview)),
    ])
}

fn session(rows: &Array, running: bool, subagent: JsValue) -> Object {
    object(&[
        ("queue", rows.clone().into()),
        ("running", JsValue::from_bool(running)),
        ("subagent", subagent),
    ])
}

fn setup(rows: &Array, running: bool, subagent: JsValue) -> (JsValue, JsValue, Object) {
    let bench = install_queue_dock_bench();
    configure_client_ui_conversation_queue_dock(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    let state = session(rows, running, subagent);
    queue_dock_set_session(state.as_ref());
    let props = object(&[
        ("useSession", make_queue_dock_use_session().into()),
        ("updateQueue", queue_dock_update().into()),
        ("notify", queue_dock_notify().into()),
        ("t", make_queue_dock_translate().into()),
    ]);
    (
        queue_dock_component().unwrap(),
        queue_dock_entry_browser().unwrap(),
        props,
    )
}

async fn flush_microtasks() {
    for _ in 0..3 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
fn empty_and_steering_only_inboxes_render_null() {
    let rows = Array::new();
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    assert!(queue_dock_render(&component, props.as_ref()).is_null());
    rows.push(row("q", "queued", JsValue::from_str("queued"), "queued").as_ref());
    queue_dock_set_session(session(&rows, true, JsValue::NULL).as_ref());
    assert!(queue_dock_render(&component, props.as_ref()).is_null());
    let replacement =
        Array::of1(row("q", "queued", JsValue::from_str("queued"), "queued").as_ref());
    queue_dock_set_session(session(&replacement, true, JsValue::NULL).as_ref());
    assert!(queue_dock_text(&queue_dock_render(&component, props.as_ref())).contains("queued"));
    let steering = Array::of1(row("s", "steering", JsValue::from_str("x"), "x").as_ref());
    queue_dock_set_session(session(&steering, true, JsValue::NULL).as_ref());
    assert!(queue_dock_render(&component, props.as_ref()).is_null());
}

#[wasm_bindgen_test]
fn single_row_is_direct_while_multiple_rows_toggle_a_linked_hidden_list() {
    let rows = Array::of1(row("one", "queued", JsValue::from_str("one"), "one").as_ref());
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    let single = queue_dock_render(&component, props.as_ref());
    assert!(queue_dock_text(&single).contains("one"));
    assert!(queue_dock_find_button(&single, "2 条排队消息").is_undefined());
    let many = Array::of2(
        row("one", "queued", JsValue::from_str("one"), "one").as_ref(),
        row("two", "queued", JsValue::from_str("two"), "two").as_ref(),
    );
    queue_dock_set_session(session(&many, true, JsValue::NULL).as_ref());
    let collapsed = queue_dock_render(&component, props.as_ref());
    let header = queue_dock_find_kind(&collapsed, "button");
    assert_eq!(
        property(&property(&header, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    let list = queue_dock_find_kind(&collapsed, "ul");
    assert_eq!(
        property(&property(&list, "props"), "hidden").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&property(&header, "props"), "aria-controls").as_string(),
        property(&property(&list, "props"), "id").as_string()
    );
    property(&property(&header, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let expanded = queue_dock_render(&component, props.as_ref());
    assert!(queue_dock_text(&expanded).contains("one"));
    assert!(queue_dock_text(&expanded).contains("two"));
}

#[wasm_bindgen_test]
fn editing_forces_multiple_rows_open_and_disables_the_collapse_header() {
    let rows = Array::of1(row("edit", "queued", JsValue::from_str("before"), "before").as_ref());
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    let tree = queue_dock_render(&component, props.as_ref());
    property(
        &property(&queue_dock_find_button(&tree, "编辑排队消息"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();

    let expanded_rows = Array::of2(
        row("edit", "queued", JsValue::from_str("before"), "before").as_ref(),
        row("second", "queued", JsValue::from_str("second"), "second").as_ref(),
    );
    queue_dock_set_session(session(&expanded_rows, true, JsValue::NULL).as_ref());
    let editing = queue_dock_render(&component, props.as_ref());
    let header = queue_dock_find_kind(&editing, "button");
    assert_eq!(
        property(&property(&header, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&property(&header, "props"), "disabled").as_bool(),
        Some(true)
    );
    assert!(queue_dock_text(&editing).contains("second"));
    assert!(queue_dock_find_kind(&editing, "input").is_object());
}

#[wasm_bindgen_test(async)]
async fn edit_change_ime_cancel_and_save_preserve_item_identity_and_busy_state() {
    let rows = Array::of1(row("edit", "queued", JsValue::from_str("before"), "before").as_ref());
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    let tree = queue_dock_render(&component, props.as_ref());
    property(
        &property(&queue_dock_find_button(&tree, "编辑排队消息"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let editing = queue_dock_render(&component, props.as_ref());
    let editor = queue_dock_find_kind(&editing, "input");
    property(&property(&editor, "props"), "onChange")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &queue_dock_change_event("after"))
        .unwrap();
    let editing = queue_dock_render(&component, props.as_ref());
    let editor = queue_dock_find_kind(&editing, "input");
    property(&property(&editor, "props"), "onKeyDown")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &queue_dock_key_event("Enter", true))
        .unwrap();
    assert_eq!(queue_dock_updates().length(), 0);
    property(&property(&editor, "props"), "onKeyDown")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &queue_dock_key_event("Enter", false))
        .unwrap();
    flush_microtasks().await;
    let update = queue_dock_updates().get(0);
    assert_eq!(property(&update, "id").as_string().as_deref(), Some("edit"));
    assert_eq!(
        property(&property(&update, "action"), "kind")
            .as_string()
            .as_deref(),
        Some("edit")
    );
    assert_eq!(
        property(
            &property(&property(&update, "action"), "content")
                .unchecked_into::<Array>()
                .get(0),
            "text"
        )
        .as_string()
        .as_deref(),
        Some("after")
    );
    let settled = queue_dock_render(&component, props.as_ref());
    assert!(queue_dock_find_kind(&settled, "input").is_undefined());

    property(
        &property(&queue_dock_find_button(&settled, "编辑排队消息"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let editing = queue_dock_render(&component, props.as_ref());
    property(
        &property(&queue_dock_find_button(&editing, "取消编辑"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    assert!(
        queue_dock_find_kind(&queue_dock_render(&component, props.as_ref()), "input")
            .is_undefined()
    );
}

#[wasm_bindgen_test]
fn escape_and_cancel_each_retire_the_editor_without_updating() {
    let rows = Array::of1(row("edit", "queued", JsValue::from_str("before"), "before").as_ref());
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    let tree = queue_dock_render(&component, props.as_ref());
    let edit = property(
        &property(&queue_dock_find_button(&tree, "编辑排队消息"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap();
    edit.call0(&JsValue::UNDEFINED).unwrap();
    let editing = queue_dock_render(&component, props.as_ref());
    let editor = queue_dock_find_kind(&editing, "input");
    property(&property(&editor, "props"), "onKeyDown")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &queue_dock_key_event("Escape", false))
        .unwrap();
    let escaped = queue_dock_render(&component, props.as_ref());
    assert!(queue_dock_find_kind(&escaped, "input").is_undefined());

    edit.call0(&JsValue::UNDEFINED).unwrap();
    let editing = queue_dock_render(&component, props.as_ref());
    property(
        &property(&queue_dock_find_button(&editing, "取消编辑"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    assert!(
        queue_dock_find_kind(&queue_dock_render(&component, props.as_ref()), "input")
            .is_undefined()
    );
    assert_eq!(queue_dock_updates().length(), 0);
}

#[wasm_bindgen_test(async)]
async fn busy_lock_and_racing_settlements_release_only_the_current_item() {
    let first = Array::of1(row("first", "queued", JsValue::from_str("one"), "one").as_ref());
    let (component, _, props) = setup(&first, true, JsValue::NULL);
    queue_dock_set_update_mode("pending");
    let first_tree = queue_dock_render(&component, props.as_ref());
    property(
        &property(
            &queue_dock_find_button(&first_tree, "删除排队消息"),
            "props",
        ),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let locked = queue_dock_render(&component, props.as_ref());
    for label in ["编辑排队消息", "删除排队消息", "插话发送"] {
        assert_eq!(
            property(
                &property(&queue_dock_find_button(&locked, label), "props"),
                "disabled"
            )
            .as_bool(),
            Some(true)
        );
    }

    let second = Array::of1(row("second", "queued", JsValue::from_str("two"), "two").as_ref());
    queue_dock_set_session(session(&second, true, JsValue::NULL).as_ref());
    let second_tree = queue_dock_render(&component, props.as_ref());
    property(
        &property(
            &queue_dock_find_button(&second_tree, "删除排队消息"),
            "props",
        ),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    queue_dock_resolve();
    flush_microtasks().await;
    let still_locked = queue_dock_render(&component, props.as_ref());
    assert_eq!(
        property(
            &property(
                &queue_dock_find_button(&still_locked, "删除排队消息"),
                "props"
            ),
            "disabled"
        )
        .as_bool(),
        Some(true)
    );
    queue_dock_resolve();
    flush_microtasks().await;
    let released = queue_dock_render(&component, props.as_ref());
    for label in ["编辑排队消息", "删除排队消息", "插话发送"] {
        assert_eq!(
            property(
                &property(&queue_dock_find_button(&released, label), "props"),
                "disabled"
            )
            .as_bool(),
            Some(false)
        );
    }
}

#[wasm_bindgen_test(async)]
async fn remove_steer_failure_and_subagent_action_suppression_match_source() {
    let rows = Array::of1(row("item", "queued", JsValue::NULL, "image [image]").as_ref());
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    let tree = queue_dock_render(&component, props.as_ref());
    let edit = queue_dock_find_button(&tree, "编辑排队消息");
    assert_eq!(
        property(&property(&edit, "props"), "disabled").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&property(&edit, "props"), "title")
            .as_string()
            .as_deref(),
        Some("包含非文本内容，暂不支持编辑")
    );
    let tooltip = queue_dock_find_kind(&tree, "Tooltip");
    assert_eq!(
        property(&property(&tooltip, "props"), "delayMs").as_f64(),
        Some(500.0)
    );
    assert!(!queue_dock_translations().includes(&JsValue::from_str("queue.steerFailed"), 0));
    property(
        &property(&queue_dock_find_button(&tree, "插话发送"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    flush_microtasks().await;
    assert!(queue_dock_translations().includes(&JsValue::from_str("queue.steerFailed"), 0));
    assert_eq!(
        property(&property(&queue_dock_updates().get(0), "action"), "kind")
            .as_string()
            .as_deref(),
        Some("steer")
    );

    queue_dock_set_session(session(&rows, false, JsValue::NULL).as_ref());
    let stopped = queue_dock_render(&component, props.as_ref());
    let steer = queue_dock_find_button(&stopped, "插话发送");
    assert_eq!(
        property(&property(&steer, "props"), "disabled").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&property(&steer, "props"), "title")
            .as_string()
            .as_deref(),
        Some("仅运行中可插话发送")
    );
    queue_dock_set_session(session(&rows, true, JsValue::NULL).as_ref());

    queue_dock_set_update_mode("reject");
    let tree = queue_dock_render(&component, props.as_ref());
    assert!(!queue_dock_translations().includes(&JsValue::from_str("queue.removeFailed"), 0));
    property(
        &property(&queue_dock_find_button(&tree, "删除排队消息"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    flush_microtasks().await;
    assert!(queue_dock_translations().includes(&JsValue::from_str("queue.removeFailed"), 0));
    assert_eq!(
        property(&property(&queue_dock_updates().get(1), "action"), "kind")
            .as_string()
            .as_deref(),
        Some("remove")
    );
    assert_eq!(
        property(&queue_dock_notices().get(0), "level")
            .as_string()
            .as_deref(),
        Some("error")
    );
    assert_eq!(
        property(&queue_dock_notices().get(0), "text")
            .as_string()
            .as_deref(),
        Some("删除失败：这条消息可能已经开始发送。")
    );

    queue_dock_set_session(
        session(&rows, true, object(&[("child", JsValue::TRUE)]).into()).as_ref(),
    );
    let child = queue_dock_render(&component, props.as_ref());
    assert_eq!(
        queue_dock_find_all_buttons(&child, "编辑排队消息").length(),
        0
    );
    assert!(queue_dock_text(&child).contains("image [image]"));
}

#[wasm_bindgen_test]
fn effect_retires_missing_editor_and_recollapses_after_empty_queue() {
    let rows = Array::of2(
        row("one", "queued", JsValue::from_str("one"), "one").as_ref(),
        row("two", "queued", JsValue::from_str("two"), "two").as_ref(),
    );
    let (component, _, props) = setup(&rows, true, JsValue::NULL);
    let tree = queue_dock_render(&component, props.as_ref());
    let header = queue_dock_find_kind(&tree, "button");
    property(&property(&header, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = queue_dock_render(&component, props.as_ref());
    property(
        &property(
            &queue_dock_find_all_buttons(&open, "编辑排队消息").get(0),
            "props",
        ),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    queue_dock_set_session(session(&Array::new(), true, JsValue::NULL).as_ref());
    assert!(queue_dock_render(&component, props.as_ref()).is_null());
    let replacement = Array::of2(
        row("three", "queued", JsValue::from_str("three"), "three").as_ref(),
        row("four", "queued", JsValue::from_str("four"), "four").as_ref(),
    );
    queue_dock_set_session(session(&replacement, true, JsValue::NULL).as_ref());
    let collapsed = queue_dock_render(&component, props.as_ref());
    let header = queue_dock_find_kind(&collapsed, "button");
    assert_eq!(
        property(&property(&header, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    assert!(queue_dock_find_kind(&collapsed, "input").is_undefined());
}

#[wasm_bindgen_test]
fn registration_entry_injects_terminal_order_and_session_services() {
    let (_, entry, _) = setup(&Array::new(), true, JsValue::NULL);
    assert_eq!(
        property(&entry, "name").as_string().as_deref(),
        Some("conversation-queue-dock")
    );
    let inject = property(&entry, "inject").unchecked_into::<Array>();
    assert_eq!(inject.length(), 3);
    let context = queue_dock_context("ok");
    property(&entry, "apply")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &context)
        .unwrap();
    assert_eq!(
        property(&queue_dock_inject_calls().get(0), "name")
            .as_string()
            .as_deref(),
        Some("conversation.input.dock")
    );
    let register = queue_dock_register_calls().get(0);
    let options = property(&register, "options");
    assert_eq!(
        property(&options, "id").as_string().as_deref(),
        Some("queue")
    );
    assert_eq!(property(&options, "order").as_f64(), Some(20.0));
    let injected = property(&options, "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("s1"))
        .unwrap();
    property(&injected, "updateQueue")
        .dyn_into::<Function>()
        .unwrap()
        .call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("item"),
            object(&[("kind", JsValue::from_str("remove"))]).as_ref(),
        )
        .unwrap();
    property(&injected, "notify")
        .dyn_into::<Function>()
        .unwrap()
        .call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("error"),
            &JsValue::from_str("failed"),
        )
        .unwrap();
    assert_eq!(
        property(&queue_dock_updates().get(0), "injected").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&queue_dock_notices().get(0), "text")
            .as_string()
            .as_deref(),
        Some("failed")
    );

    for (mode, expected) in [
        ("no-scope", "queue dock: session \"s1\" resolved no scope"),
        (
            "no-conversation",
            "queue dock: conversation service unavailable",
        ),
    ] {
        let context = queue_dock_context(mode);
        property(&entry, "apply")
            .dyn_into::<Function>()
            .unwrap()
            .call1(&JsValue::UNDEFINED, &context)
            .unwrap();
        let calls = queue_dock_register_calls();
        let call = calls.get(calls.length() - 1);
        let inject = property(&property(&call, "options"), "inject")
            .dyn_into::<Function>()
            .unwrap();
        let error = inject
            .call1(&JsValue::UNDEFINED, &JsValue::from_str("s1"))
            .unwrap_err();
        assert_eq!(
            property(&error, "message").as_string().as_deref(),
            Some(expected)
        );
    }
}
