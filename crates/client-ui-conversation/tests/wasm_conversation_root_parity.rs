//! Live WASM coverage for the optional-session conversation root.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_root, conversation_root_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let bench
let hooks = []
let cursor = 0
let pendingEffects = []

function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}
function deferred() {
  let resolve
  let reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

export function rootSetup() {
  hooks = []; cursor = 0; pendingEffects = []
  bench = {
    sessionId: undefined,
    session: undefined,
    input: undefined,
    sessions: { byId: {} },
    workspaces: { phase: 'loading', items: [] },
    block: undefined,
    slots: [], chains: [], selections: [], selectionMode: 'resolve', selectionPending: [],
    resizeObservers: [], styleWrites: [],
  }
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  globalThis.ResizeObserver = class {
    constructor(callback) { this.callback = callback; this.observed = []; this.disconnected = 0; bench.resizeObservers.push(this) }
    observe(value) { this.observed.push(value) }
    disconnect() { this.disconnected += 1 }
  }
  const React = {
    createElement(kind, props, ...children) {
      const node = { kind, props: props ?? {}, children }
      if (typeof props?.ref === 'function') {
        const scroller = { style: { setProperty(name, value) { bench.styleWrites.push({ name, value, receiver: this }) } } }
        const seat = { parentElement: scroller, offsetHeight: 180 }
        props.ref(seat)
        bench.seat = seat
      } else if (props?.ref && typeof props.ref === 'object') props.ref.current = node
      return node
    },
    useId() { return ':root:1:' },
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
    useCallback(callback, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'callback', value: callback, deps: [...deps] }
      return hooks[index].value
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) pendingEffects.push({ index, effect, deps: [...deps] })
    },
  }
  const uiPrimitives = {
    FishLogo: 'FishLogo', IconChevronDownOutline14: 'IconChevronDownOutline14',
    IconFolderClose16: 'IconFolderClose16', IconFolderOpen16: 'IconFolderOpen16',
  }
  const props = {
    get sessionId() { return bench.sessionId },
    useSession(selector) { return selector(bench.session) },
    useInput(selector) { return selector(bench.input) },
    useSessions(selector) { return selector(bench.sessions) },
    useWorkspaces(selector) { return selector(bench.workspaces) },
    useComposerBlock(selector) { return selector(bench.block) },
    renderSlot(name, owner) { const node = { kind: `slot:${name}`, props: owner, children: [] }; bench.slots.push({ name, owner, node }); return node },
    renderSlotChain(name, owner, options) { const node = { kind: `chain:${name}`, props: { owner, options }, children: [options.fallback] }; bench.chains.push({ name, owner, options, node, argc: arguments.length }); return node },
    selectWorkspace(id) {
      bench.selections.push(id)
      if (bench.selectionMode === 'resolve') return Promise.resolve()
      if (bench.selectionMode === 'reject') return Promise.reject(new Error('selection failed'))
      const pending = deferred(); bench.selectionPending.push(pending); return pending.promise
    },
    t(key) {
      const copy = {
        'placeholder.workspace': 'Choose a workspace', 'placeholder.hero': 'Ask anything',
        'hero.chooseWorkspace': 'Choose workspace', 'hero.headline': 'Into the Unknown', 'hero.preview': 'Preview',
      }
      return copy[key] ?? key
    },
  }
  bench.React = React; bench.uiPrimitives = uiPrimitives; bench.props = props
  return bench
}

export function rootObject(entries) { return Object.fromEntries(entries) }
export function rootRender(component) {
  cursor = 0; pendingEffects = []; bench.slots = []; bench.chains = []
  const tree = component(bench.props)
  for (const pending of pendingEffects) {
    hooks[pending.index]?.cleanup?.()
    hooks[pending.index] = { type: 'effect', deps: pending.deps, cleanup: pending.effect() }
  }
  return tree
}
export function rootBench() { return bench }
export function rootSetSessionId(value) { bench.sessionId = value }
export function rootSetSession(value) { bench.session = value }
export function rootSetInput(value) { bench.input = value }
export function rootSetSessions(value) { bench.sessions = value }
export function rootSetWorkspaces(value) { bench.workspaces = value }
export function rootSetBlock(value) { bench.block = value }
export function rootSetSelectionMode(value) { bench.selectionMode = value }
export function rootRejectSelection(index) { bench.selectionPending[index].reject(new Error('selection failed')) }
export function rootSlots() { return bench.slots }
export function rootChains() { return bench.chains }
export function rootSelections() { return bench.selections }
export function rootFireResize(index) { bench.resizeObservers[index].callback() }
export function rootStyleWrites() { return bench.styleWrites }
export function rootFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = rootFindKind(child, kind); if (found) return found }
  return undefined
}
export function rootFindClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if ((value.props?.className ?? '').split(' ').includes(className)) return value
  for (const child of value.children ?? []) { const found = rootFindClass(child, className); if (found) return found }
  return undefined
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = rootSetup)]
    fn root_setup() -> JsValue;
    #[wasm_bindgen(js_name = rootObject)]
    fn root_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = rootRender)]
    fn root_render(component: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = rootBench)]
    fn root_bench() -> JsValue;
    #[wasm_bindgen(js_name = rootSetSessionId)]
    fn root_set_session_id(value: JsValue);
    #[wasm_bindgen(js_name = rootSetSession)]
    fn root_set_session(value: JsValue);
    #[wasm_bindgen(js_name = rootSetInput)]
    fn root_set_input(value: JsValue);
    #[wasm_bindgen(js_name = rootSetSessions)]
    fn root_set_sessions(value: &JsValue);
    #[wasm_bindgen(js_name = rootSetWorkspaces)]
    fn root_set_workspaces(value: &JsValue);
    #[wasm_bindgen(js_name = rootSetBlock)]
    fn root_set_block(value: JsValue);
    #[wasm_bindgen(js_name = rootSetSelectionMode)]
    fn root_set_selection_mode(value: &str);
    #[wasm_bindgen(js_name = rootRejectSelection)]
    fn root_reject_selection(index: u32);
    #[wasm_bindgen(js_name = rootSlots)]
    fn root_slots() -> Array;
    #[wasm_bindgen(js_name = rootChains)]
    fn root_chains() -> Array;
    #[wasm_bindgen(js_name = rootSelections)]
    fn root_selections() -> Array;
    #[wasm_bindgen(js_name = rootFireResize)]
    fn root_fire_resize(index: u32);
    #[wasm_bindgen(js_name = rootStyleWrites)]
    fn root_style_writes() -> Array;
    #[wasm_bindgen(js_name = rootFindKind)]
    fn root_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = rootFindClass)]
    fn root_find_class(value: &JsValue, class_name: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key))
        .unwrap_or_else(|error| panic!("property {key:?} on {value:?} failed: {error:?}"))
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&Array::of2(&JsValue::from_str(key), value));
    }
    root_object(&values).unchecked_into()
}

fn slot(name: &str) -> JsValue {
    root_slots()
        .iter()
        .find(|row| property(row, "name").as_string().as_deref() == Some(name))
        .unwrap()
}

fn owner(name: &str) -> JsValue {
    property(&slot(name), "owner")
}

fn base_session(over: &[(&str, JsValue)]) -> Object {
    let mut values = vec![
        ("openState", JsValue::from_str("cold")),
        ("composerPhase", JsValue::from_str("blank")),
        ("pending", Array::new().into()),
    ];
    values.extend_from_slice(over);
    object(&values)
}

fn input() -> Object {
    object(&[
        ("draft", JsValue::from_str("")),
        ("imageIds", Array::new().into()),
        ("draftRev", JsValue::from_f64(0.0)),
        ("phase", JsValue::from_str("plain")),
        ("occurrences", Array::new().into()),
        ("queue", Array::new().into()),
    ])
}

fn workspace(id: &str, title: &str, sessions: &[&str]) -> Object {
    object(&[
        ("workspaceId", JsValue::from_str(id)),
        ("title", JsValue::from_str(title)),
        (
            "sessionIds",
            sessions
                .iter()
                .map(|id| JsValue::from_str(id))
                .collect::<Array>()
                .into(),
        ),
    ])
}

fn sessions(cwd: &str, blank: bool) -> Object {
    object(&[(
        "byId",
        object(&[(
            "s1",
            object(&[
                ("cwd", JsValue::from_str(cwd)),
                ("blank", JsValue::from_bool(blank)),
            ])
            .into(),
        )])
        .into(),
    )])
}

fn workspaces(phase: &str, items: &[Object]) -> Object {
    object(&[
        ("phase", JsValue::from_str(phase)),
        (
            "items",
            items
                .iter()
                .map(|item| JsValue::from(item.clone()))
                .collect::<Array>()
                .into(),
        ),
    ])
}

fn setup() -> JsValue {
    let bench = root_setup();
    configure_client_ui_conversation_root(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    conversation_root_component().unwrap()
}

async fn flush_microtasks() {
    for _ in 0..6 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One Hook runtime owns the full resident transition sequence.
async fn compiled_conversation_root_runs_resident_phase_workspace_chain_and_resize_matrix() {
    let component = setup();
    let tree = root_render(&component);
    assert_eq!(
        property(&property(&tree, "props"), "data-phase")
            .as_string()
            .as_deref(),
        Some("hero")
    );
    let bar = owner("conversation.composer.bar");
    assert_eq!(
        property(&bar, "variant").as_string().as_deref(),
        Some("hero")
    );
    assert_eq!(property(&bar, "disabled").as_bool(), Some(true));
    assert_eq!(
        property(&bar, "placeholder").as_string().as_deref(),
        Some("Choose a workspace")
    );
    assert!(property(&bar, "leftItems").is_null());
    assert!(property(&bar, "rightItems").is_null());
    assert!(property(&bar, "footer").is_null());
    let chain = root_chains().get(0);
    assert_eq!(property(&chain, "argc").as_f64(), Some(3.0));
    assert_eq!(
        property(&property(&chain, "options"), "overlay").as_bool(),
        Some(true)
    );
    root_fire_resize(0);
    let write = root_style_writes().get(0);
    assert_eq!(
        property(&write, "name").as_string().as_deref(),
        Some("--seekdeep-composer-height")
    );
    assert_eq!(
        property(&write, "value").as_string().as_deref(),
        Some("180px")
    );

    root_set_session_id(JsValue::from_str("s1"));
    root_set_session(base_session(&[("openState", JsValue::from_str("loading"))]).into());
    root_set_input(input().into());
    root_set_sessions(sessions("/work/project", false).as_ref());
    root_set_workspaces(workspaces("loading", &[]).as_ref());
    let tree = root_render(&component);
    assert_eq!(
        property(&property(&tree, "props"), "data-phase")
            .as_string()
            .as_deref(),
        Some("settling")
    );
    assert_eq!(
        property(&owner("conversation.composer.bar"), "variant")
            .as_string()
            .as_deref(),
        Some("composer")
    );

    root_set_sessions(sessions("/work/project", true).as_ref());
    let tree = root_render(&component);
    assert_eq!(
        property(&property(&tree, "props"), "data-phase")
            .as_string()
            .as_deref(),
        Some("hero")
    );
    assert_eq!(
        property(&owner("conversation.composer.bar"), "placeholder")
            .as_string()
            .as_deref(),
        Some("Ask anything")
    );

    root_set_session(
        base_session(&[
            ("openState", JsValue::from_str("open")),
            ("composerPhase", JsValue::from_str("active")),
        ])
        .into(),
    );
    root_set_workspaces(workspaces("ready", &[workspace("w1", "Project", &["s1"])]).as_ref());
    root_set_block(object(&[("reason", JsValue::from_str("Choose a model"))]).into());
    let tree = root_render(&component);
    assert_eq!(
        property(&property(&tree, "props"), "data-phase")
            .as_string()
            .as_deref(),
        Some("active")
    );
    let bar = owner("conversation.composer.bar");
    assert!(property(&bar, "disabled").is_undefined());
    assert_eq!(
        property(&property(&bar, "blocked"), "reason")
            .as_string()
            .as_deref(),
        Some("Choose a model")
    );
    assert_eq!(
        property(&bar, "placeholder").as_string().as_deref(),
        Some("Choose a model")
    );
    assert!(!property(&bar, "footer").is_null());
    let left = owner("conversation.input.left");
    assert!(Object::is(
        &property(&left, "session"),
        &property(&root_bench(), "session")
    ));
    assert!(Object::is(
        &property(&left, "input"),
        &property(&root_bench(), "input")
    ));

    let component = setup();
    root_set_session_id(JsValue::from_str("s1"));
    root_set_session(base_session(&[("openState", JsValue::from_str("open"))]).into());
    root_set_input(input().into());
    root_set_sessions(sessions("/work/old", true).as_ref());
    root_set_workspaces(
        workspaces(
            "ready",
            &[workspace("w1", "Old", &["s1"]), workspace("w2", "New", &[])],
        )
        .as_ref(),
    );
    let _ = root_render(&component);
    let workspace_owner = owner("conversation.hero.workspace");
    property(&workspace_owner, "onPick")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("w2"))
        .unwrap();
    assert_eq!(root_selections().get(0).as_string().as_deref(), Some("w2"));
    let _ = root_render(&component);
    assert_eq!(
        property(&owner("conversation.hero.workspace"), "selectedId")
            .as_string()
            .as_deref(),
        Some("w2")
    );

    root_set_selection_mode("pending");
    let workspace_owner = owner("conversation.hero.workspace");
    property(&workspace_owner, "onPick")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("w2"))
        .unwrap();
    root_reject_selection(0);
    flush_microtasks().await;
    let _ = root_render(&component);
    assert_eq!(
        property(&owner("conversation.hero.workspace"), "selectedId")
            .as_string()
            .as_deref(),
        Some("w1")
    );
    assert!(!root_find_class(&tree, "seekdeep-conversation-session-composerSeat").is_undefined());
    assert!(!root_find_kind(&tree, "slot:conversation.session").is_undefined());
}
