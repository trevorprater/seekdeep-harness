//! Live WASM coverage for `ChatView` flow rendering and scroll ownership.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    chat_view_component, configure_client_ui_conversation_chat_view, turn_status_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let pendingLayouts = []
let pendingEffects = []
let now = 20000
let timers = new Map()
let nextTimer = 1
let session
let sessions
let store
let props
let savedScroll = null
let saveCalls = []
let loadOlderCalls = 0
let useHost = false
let hitRow = null
let observers = []
function sameDeps(left, right) {
  if (left === undefined || right === undefined) return false
  return left.length === right.length && left.every((value, index) => Object.is(value, right[index]))
}
class RowMock {
  constructor(key, top) { this.dataset = { chatAnchorKey: key }; this.top = top }
  getBoundingClientRect() { return { top: this.top, bottom: this.top + 40, left: 0, right: 500 } }
  closest(selector) { return selector === '[data-chat-anchor-key]' ? this : null }
}
class ElementMock {
  constructor(kind) {
    this.kind = kind; this._scrollTop = 0; this.scrollHeight = 1000; this.clientHeight = 200
    this.rows = []; this.listeners = new Map(); this.composer = null
  }
  get scrollTop() { return this._scrollTop }
  set scrollTop(value) { this._scrollTop = Math.max(0, Math.min(value, Math.max(0, this.scrollHeight - this.clientHeight))) }
  closest(selector) { return selector === '[data-conversation-scroll]' && useHost ? hostElement : null }
  querySelector(selector) { return selector === '[data-composer-seat]' ? this.composer : null }
  querySelectorAll(selector) {
    const values = selector === '[data-chat-anchor-key]' ? this.rows : []
    return { length: values.length, item(index) { return values[index] ?? null } }
  }
  contains(value) { return this.rows.includes(value) }
  getBoundingClientRect() { return { top: 0, bottom: this.clientHeight, left: 0, right: 500 } }
  addEventListener(name, listener) { if (!this.listeners.has(name)) this.listeners.set(name, new Set()); this.listeners.get(name).add(listener) }
  removeEventListener(name, listener) { this.listeners.get(name)?.delete(listener) }
  dispatch(name) { for (const listener of this.listeners.get(name) ?? []) listener() }
}
class ComposerMock { getBoundingClientRect() { return { top: 160, bottom: 200, left: 0, right: 500 } } }
class BenchResizeObserver {
  constructor(callback) { this.callback = callback; this.targets = []; this.disconnected = false; observers.push(this) }
  observe(target) { this.targets.push(target) }
  disconnect() { this.disconnected = true }
}
let listElement
let columnElement
let hostElement
function resetState() {
  hooks = []; cursor = 0; pendingLayouts = []; pendingEffects = []; now = 20000; timers = new Map(); nextTimer = 1
  savedScroll = null; saveCalls = []; loadOlderCalls = 0; useHost = false; hitRow = null; observers = []
  listElement = new ElementMock('list'); columnElement = new ElementMock('column'); hostElement = new ElementMock('host')
  hostElement.composer = new ComposerMock()
  session = {
    chat: { order: [], nodes: new Map(), timeline: { turns: new Map() } }, queue: [], running: false,
    openState: 'open', openError: null, hasMore: false, loadingOlder: false,
  }
  sessions = { byId: { s1: { cwd: '/repo' } } }
  store = { selection: null }
}
export function installChatBench() {
  resetState()
  Date.now = () => now
  globalThis.window = globalThis
  globalThis.setInterval = (callback, delay) => { const id = nextTimer++; timers.set(id, { callback, delay, due: now + delay }); return id }
  globalThis.clearInterval = id => { timers.delete(id) }
  globalThis.ResizeObserver = BenchResizeObserver
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
    elementsFromPoint() { return hitRow === null ? [] : [hitRow] },
  }
  const React = {
    createElement(kind, elementProps, ...children) {
      const className = elementProps?.className
      if (kind === 'div' && className === 'seekdeep-conversation-chat-scroll' && elementProps?.ref) elementProps.ref.current = listElement
      if (kind === 'div' && className === 'seekdeep-conversation-chat-column' && elementProps?.ref) elementProps.ref.current = columnElement
      return { kind, props: elementProps ?? {}, children }
    },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'memo', deps: [...deps], value: factory() }
      return hooks[index].value
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'ref', value: { current: initial } }
      return hooks[index].value
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: typeof initial === 'function' ? initial() : initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useLayoutEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        const previous = hooks[index]
        pendingLayouts.push(() => { previous?.cleanup?.(); hooks[index] = { type: 'layout', deps, cleanup: effect() } })
      }
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        const previous = hooks[index]
        pendingEffects.push(() => { previous?.cleanup?.(); hooks[index] = { type: 'effect', deps, cleanup: effect() } })
      }
    },
  }
  const uiPrimitives = { IconChevronDownOutline14: 'IconChevronDownOutline14' }
  const dependencies = { ChatNodeSeat: 'ChatNodeSeat', PendingSteeringBubble: 'PendingSteeringBubble' }
  const translate = (key, vars) => ({
    'chat.loadingHistory': '正在加载历史记录', 'chat.loadError': `加载失败：${vars?.message}（${vars?.code}）`,
    loading: '加载中', 'chat.loadOlder': '加载更早', 'chat.toBottom': '回到底部',
    'duration.seconds': `${vars?.seconds}秒`, 'duration.minutes': `${vars?.minutes}分${vars?.seconds}秒`,
  })[key] ?? key
  const chatScroll = { save(position) { savedScroll = position; saveCalls.push(position) }, read() { return savedScroll } }
  props = {
    useSession: selector => selector(session), useSessions: selector => selector(sessions), useStore: selector => selector(store),
    renderSlot() {}, sessionId: 's1', openFile() {}, loadOlder() { loadOlderCalls += 1 }, loadImage() {}, inspectCall() {},
    chatScroll, forkAt() {}, fileMentions() {}, t: translate,
  }
  return { React, uiPrimitives, dependencies, props }
}
export function chatRender(component) {
  cursor = 0; pendingLayouts = []; pendingEffects = []
  const tree = component(props)
  for (const run of pendingLayouts) run()
  for (const run of pendingEffects) run()
  return tree
}
export function chatRenderWithProps(component, value) {
  cursor = 0; pendingLayouts = []; pendingEffects = []
  const tree = component(value)
  for (const run of pendingLayouts) run()
  for (const run of pendingEffects) run()
  return tree
}
export function chatUnmount() { for (const hook of [...hooks].reverse()) hook?.cleanup?.(); hooks = [] }
export function chatSetFlow(entries) {
  session.chat.order = entries.map(entry => entry.key)
  session.chat.nodes = new Map(entries.map(entry => [entry.key, { anchorSeq: entry.seq, kind: entry.kind }]))
  listElement.rows = entries.map((entry, index) => new RowMock(entry.key, entry.top ?? index * 100))
  columnElement.rows = listElement.rows
}
export function chatSetSession(patch) { Object.assign(session, patch) }
export function chatSetTimeline(turns) { session.chat.timeline = { turns: new Map(turns.map((turn, index) => [index, turn])) } }
export function chatSetQueue(queue) { session.queue = queue }
export function chatSetSelection(callId) { store.selection = callId === undefined ? null : { callId } }
export function chatSetSaved(value) { savedScroll = value }
export function chatSaved() { return savedScroll }
export function chatSaveCalls() { return saveCalls }
export function chatLoadOlderCalls() { return loadOlderCalls }
export function chatSetMetrics(scrollHeight, clientHeight, scrollTop) {
  for (const element of [listElement, hostElement]) { element.scrollHeight = scrollHeight; element.clientHeight = clientHeight; element.scrollTop = scrollTop }
}
export function chatScrollTop() { return (useHost ? hostElement : listElement).scrollTop }
export function chatSetRowTop(key, top) { const row = listElement.rows.find(row => row.dataset.chatAnchorKey === key); if (row) row.top = top }
export function chatHitRow(key) { hitRow = listElement.rows.find(row => row.dataset.chatAnchorKey === key) ?? null }
export function chatReaderScroll(top) { const element = useHost ? hostElement : listElement; element.scrollTop = top; element.dispatch('scroll') }
export function chatUseHost(value) { useHost = value }
export function chatTriggerResize() { for (const observer of observers) if (!observer.disconnected) observer.callback([]) }
export function chatObserverState() { return { count: observers.length, active: observers.filter(observer => !observer.disconnected).length, targets: observers.flatMap(observer => observer.targets).length } }
export function chatAdvance(milliseconds) {
  const target = now + milliseconds
  while (true) {
    let due = Infinity
    for (const timer of timers.values()) due = Math.min(due, timer.due)
    if (due > target) break
    now = due
    for (const [id, timer] of [...timers]) { if (timer.due === due) { timer.callback(); if (timers.has(id)) timer.due += timer.delay } }
  }
  now = target
}
export function chatTimerCount() { return timers.size }
export function chatTranslator() { return props.t }
export function chatText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(chatText).join('')
  return chatText(value.children)
}
export function chatFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = chatFindKind(child, kind); if (found) return found }
  return undefined
}
export function chatFindAllKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === kind ? [value] : []
  return own.concat(...(value.children ?? []).map(child => chatFindAllKind(child, kind)))
}
export function chatFunctionKindCount(value) {
  if (value === null || value === undefined || typeof value !== 'object') return 0
  const own = typeof value.kind === 'function' ? 1 : 0
  return own + (value.children ?? []).reduce((total, child) => total + chatFunctionKindCount(child), 0)
}
export function chatFindAllClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = String(value.props?.className ?? '').split(/\s+/).includes(className) ? [value] : []
  return own.concat(...(value.children ?? []).map(child => chatFindAllClass(child, className)))
}
export function chatObject(entries) { return Object.fromEntries(entries) }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installChatBench)]
    fn install_chat_bench() -> JsValue;
    #[wasm_bindgen(js_name = chatRender)]
    fn chat_render(component: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = chatRenderWithProps)]
    fn chat_render_with_props(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = chatUnmount)]
    fn chat_unmount();
    #[wasm_bindgen(js_name = chatSetFlow)]
    fn chat_set_flow(entries: &Array);
    #[wasm_bindgen(js_name = chatSetSession)]
    fn chat_set_session(patch: &JsValue);
    #[wasm_bindgen(js_name = chatSetTimeline)]
    fn chat_set_timeline(turns: &Array);
    #[wasm_bindgen(js_name = chatSetQueue)]
    fn chat_set_queue(queue: &Array);
    #[wasm_bindgen(js_name = chatSetSelection)]
    fn chat_set_selection(call_id: &JsValue);
    #[wasm_bindgen(js_name = chatSetSaved)]
    fn chat_set_saved(value: &JsValue);
    #[wasm_bindgen(js_name = chatSaved)]
    fn chat_saved() -> JsValue;
    #[wasm_bindgen(js_name = chatSaveCalls)]
    fn chat_save_calls() -> Array;
    #[wasm_bindgen(js_name = chatLoadOlderCalls)]
    fn chat_load_older_calls() -> u32;
    #[wasm_bindgen(js_name = chatSetMetrics)]
    fn chat_set_metrics(scroll_height: f64, client_height: f64, scroll_top: f64);
    #[wasm_bindgen(js_name = chatScrollTop)]
    fn chat_scroll_top() -> f64;
    #[wasm_bindgen(js_name = chatSetRowTop)]
    fn chat_set_row_top(key: &str, top: f64);
    #[wasm_bindgen(js_name = chatHitRow)]
    fn chat_hit_row(key: &str);
    #[wasm_bindgen(js_name = chatReaderScroll)]
    fn chat_reader_scroll(top: f64);
    #[wasm_bindgen(js_name = chatUseHost)]
    fn chat_use_host(value: bool);
    #[wasm_bindgen(js_name = chatTriggerResize)]
    fn chat_trigger_resize();
    #[wasm_bindgen(js_name = chatObserverState)]
    fn chat_observer_state() -> JsValue;
    #[wasm_bindgen(js_name = chatAdvance)]
    fn chat_advance(milliseconds: f64);
    #[wasm_bindgen(js_name = chatTimerCount)]
    fn chat_timer_count() -> u32;
    #[wasm_bindgen(js_name = chatTranslator)]
    fn chat_translator() -> Function;
    #[wasm_bindgen(js_name = chatText)]
    fn chat_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = chatFindKind)]
    fn chat_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = chatFindAllKind)]
    fn chat_find_all_kind(value: &JsValue, kind: &str) -> Array;
    #[wasm_bindgen(js_name = chatFunctionKindCount)]
    fn chat_function_kind_count(value: &JsValue) -> u32;
    #[wasm_bindgen(js_name = chatFindAllClass)]
    fn chat_find_all_class(value: &JsValue, class_name: &str) -> Array;
    #[wasm_bindgen(js_name = chatObject)]
    fn chat_object(entries: &Array) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    chat_object(&array).unchecked_into()
}

fn flow(entries: &[(&str, f64, &str, f64)]) -> Array {
    entries
        .iter()
        .map(|(key, seq, kind, top)| {
            JsValue::from(object(&[
                ("key", JsValue::from_str(key)),
                ("seq", JsValue::from_f64(*seq)),
                ("kind", JsValue::from_str(kind)),
                ("top", JsValue::from_f64(*top)),
            ]))
        })
        .collect()
}

fn setup() -> (JsValue, JsValue) {
    let bench = install_chat_bench();
    configure_client_ui_conversation_chat_view(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
        property(&bench, "dependencies"),
    )
    .unwrap();
    (
        chat_view_component().unwrap(),
        turn_status_component().unwrap(),
    )
}

#[wasm_bindgen_test]
fn render_threads_keyed_rows_pending_selection_cwd_and_open_states() {
    let (component, _) = setup();
    chat_set_flow(&flow(&[
        ("u1", 1.0, "user", 10.0),
        ("a2", 2.0, "assistant-step", 80.0),
    ]));
    chat_set_selection(&JsValue::from_str("call-7"));
    chat_set_queue(&Array::of2(
        object(&[
            ("id", JsValue::from_str("steer")),
            ("placement", JsValue::from_str("steering")),
            ("content", Array::new().into()),
        ])
        .as_ref(),
        object(&[
            ("id", JsValue::from_str("queued")),
            ("placement", JsValue::from_str("queued")),
            ("content", Array::new().into()),
        ])
        .as_ref(),
    ));
    chat_set_session(
        object(&[
            ("openState", JsValue::from_str("loading")),
            ("hasMore", JsValue::TRUE),
            ("loadingOlder", JsValue::FALSE),
            ("running", JsValue::TRUE),
        ])
        .as_ref(),
    );
    chat_set_timeline(&Array::of1(
        object(&[
            ("status", JsValue::from_str("open")),
            (
                "start",
                object(&[("time", JsValue::from_f64(5_000.0))]).into(),
            ),
        ])
        .as_ref(),
    ));
    let tree = chat_render(&component);
    assert!(chat_text(&tree).contains("正在加载历史记录"));
    assert!(chat_text(&tree).contains("加载更早"));
    let seats = chat_find_all_kind(&tree, "ChatNodeSeat");
    assert_eq!(seats.length(), 2);
    assert_eq!(
        property(&property(&seats.get(0), "props"), "nodeKey")
            .as_string()
            .as_deref(),
        Some("u1")
    );
    assert_eq!(
        property(&property(&seats.get(1), "props"), "selectedCallId")
            .as_string()
            .as_deref(),
        Some("call-7")
    );
    assert_eq!(
        property(&property(&seats.get(0), "props"), "cwd")
            .as_string()
            .as_deref(),
        Some("/repo")
    );
    assert_eq!(
        chat_find_all_kind(&tree, "PendingSteeringBubble").length(),
        1
    );
    assert_eq!(chat_function_kind_count(&tree), 1);

    chat_set_session(
        object(&[
            ("openState", JsValue::from_str("error")),
            (
                "openError",
                object(&[
                    ("message", JsValue::from_str("offline")),
                    ("code", JsValue::from_str("NET")),
                ])
                .into(),
            ),
            ("hasMore", JsValue::FALSE),
            ("running", JsValue::FALSE),
        ])
        .as_ref(),
    );
    let error = chat_render(&component);
    assert!(chat_text(&error).contains("加载失败：offline（NET）"));
    chat_set_flow(&Array::new());
    let emptied = chat_render(&component);
    assert_eq!(chat_find_all_kind(&emptied, "ChatNodeSeat").length(), 0);
    chat_unmount();
}

#[wasm_bindgen_test]
fn initial_open_jumps_to_bottom_or_restores_saved_semantic_position() {
    let (component, _) = setup();
    chat_set_flow(&flow(&[("a", 1.0, "assistant-step", 50.0)]));
    chat_set_metrics(1_000.0, 200.0, 0.0);
    chat_render(&component);
    assert!((chat_scroll_top() - 800.0).abs() < f64::EPSILON);
    assert!(chat_saved().is_null());
    chat_unmount();

    let (component, _) = setup();
    chat_set_flow(&flow(&[("a", 1.0, "assistant-step", 50.0)]));
    chat_set_metrics(1_000.0, 200.0, 0.0);
    chat_set_saved(
        object(&[
            ("anchorKey", JsValue::from_str("a")),
            ("anchorTop", JsValue::from_f64(20.0)),
            ("scrollTop", JsValue::from_f64(100.0)),
        ])
        .as_ref(),
    );
    chat_render(&component);
    assert!((chat_scroll_top() - 130.0).abs() < f64::EPSILON);
    let saved = chat_saved();
    assert_eq!(
        property(&saved, "anchorKey").as_string().as_deref(),
        Some("a")
    );
    assert!(
        (property(&saved, "scrollTop").as_f64().unwrap_or_default() - 130.0).abs() < f64::EPSILON
    );
    chat_unmount();
}

#[wasm_bindgen_test]
fn reader_scroll_disables_follow_and_back_to_bottom_restores_it() {
    let (component, _) = setup();
    chat_set_flow(&flow(&[("a", 1.0, "assistant-step", 50.0)]));
    chat_set_metrics(1_000.0, 200.0, 0.0);
    chat_render(&component);
    chat_reader_scroll(500.0);
    let away = chat_render(&component);
    let button = chat_find_kind(&away, "button");
    assert_eq!(
        property(&property(&button, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("回到底部")
    );
    let saved = chat_saved();
    assert_eq!(
        property(&saved, "anchorKey").as_string().as_deref(),
        Some("a")
    );
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!((chat_scroll_top() - 800.0).abs() < f64::EPSILON);
    assert!(chat_saved().is_null());
    chat_unmount();
}

#[wasm_bindgen_test]
fn prepend_uses_latest_reader_anchor_and_paging_button_calls_loader() {
    let (component, _) = setup();
    chat_set_flow(&flow(&[
        ("u9", 9.0, "user", 100.0),
        ("u10", 10.0, "user", 300.0),
    ]));
    chat_set_session(object(&[("hasMore", JsValue::TRUE)]).as_ref());
    chat_set_metrics(800.0, 200.0, 50.0);
    let tree = chat_render(&component);
    chat_hit_row("u9");
    let older = chat_find_kind(&tree, "button");
    property(&property(&older, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(chat_load_older_calls(), 1);
    chat_set_row_top("u9", -200.0);
    chat_set_row_top("u10", 60.0);
    chat_hit_row("u10");
    chat_reader_scroll(90.0);
    chat_set_flow(&flow(&[
        ("old", 2.0, "assistant-step", -50.0),
        ("u9", 9.0, "user", 300.0),
        ("u10", 10.0, "user", 560.0),
    ]));
    chat_set_metrics(1_300.0, 200.0, 90.0);
    chat_render(&component);
    assert!((chat_scroll_top() - 590.0).abs() < f64::EPSILON);
    chat_unmount();
}

#[wasm_bindgen_test]
fn resize_observer_follows_only_while_pinned_and_targets_host_composer() {
    let (component, _) = setup();
    chat_use_host(true);
    chat_set_flow(&flow(&[("a", 1.0, "assistant-step", 50.0)]));
    chat_set_metrics(1_000.0, 200.0, 0.0);
    chat_render(&component);
    let state = chat_observer_state();
    assert_eq!(property(&state, "active").as_f64(), Some(1.0));
    assert_eq!(property(&state, "targets").as_f64(), Some(2.0));
    chat_set_metrics(1_200.0, 200.0, chat_scroll_top());
    chat_trigger_resize();
    assert!((chat_scroll_top() - 1_000.0).abs() < f64::EPSILON);
    chat_reader_scroll(600.0);
    chat_set_metrics(1_400.0, 200.0, 600.0);
    chat_trigger_resize();
    assert!((chat_scroll_top() - 600.0).abs() < f64::EPSILON);
    chat_unmount();
    assert_eq!(
        property(&chat_observer_state(), "active").as_f64(),
        Some(0.0)
    );
}

#[wasm_bindgen_test]
fn turn_status_uses_logged_start_delays_clock_and_cleans_interval() {
    let (_, status) = setup();
    let status_props = object(&[
        ("startTime", JsValue::from_f64(5_000.0)),
        ("t", chat_translator().into()),
    ]);
    let first = chat_render_with_props(&status, status_props.as_ref());
    assert_eq!(chat_text(&first), "Deep diving...15秒");
    let clock = chat_find_all_class(&first, "seekdeep-conversation-chat-turnStatusClock").get(0);
    assert_eq!(
        property(&property(&clock, "props"), "aria-hidden").as_bool(),
        Some(true)
    );
    assert_eq!(chat_timer_count(), 1);
    chat_advance(1_000.0);
    let second = chat_render_with_props(&status, status_props.as_ref());
    assert_eq!(chat_text(&second), "Deep diving...16秒");
    chat_unmount();
    assert_eq!(chat_timer_count(), 0);
}
