//! Live WASM coverage for strict-session header and active-view body.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_session, conversation_session_component,
    conversation_session_header_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let tabs = []
let session = {}
let sessions = { byId: {} }
let store = { view: null, draft: '', inspect: null }
let input = { draft: '' }
let slotCalls = []
let opened = []
let viewsSet = []
let inspectSet = []
let inputDrafts = []
let mirrorBinds = 0
let unmirrors = 0
let releases = []
let ledgerHooks = 0
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
export function installSessionBench() {
  hooks = []; cursor = 0; tabs = []; session = {}; sessions = { byId: {} }
  store = { view: null, draft: '', inspect: null }; input = { draft: '' }
  slotCalls = []; opened = []; viewsSet = []; inspectSet = []; inputDrafts = []
  mirrorBinds = 0; unmirrors = 0; releases = []; ledgerHooks = 0
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment', createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useSyncExternalStore(subscribe, getSnapshot) { ledgerHooks += 1; subscribe(() => {}); return getSnapshot() },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index]?.cleanup?.()
        hooks[index] = { deps: [...deps], cleanup: effect() }
      }
    },
  }
  return { React }
}
export function sessionResetHooks() { for (const hook of [...hooks].reverse()) hook?.cleanup?.(); hooks = []; cursor = 0 }
export function sessionObject(entries) { return Object.fromEntries(entries) }
export function sessionSetTabs(value) { tabs = [...value] }
export function sessionSetSession(value) { session = value }
export function sessionSetSessions(value) { sessions = value }
export function sessionSetStore(value) { store = value }
export function sessionSetInput(value) { input = value }
export function sessionViews() { return { list: () => tabs, subscribe: () => () => {}, version: () => 1 } }
export function makeSessionUseSession() { return selector => selector(session) }
export function makeSessionUseSessions() { return (selector, equal) => { const value = selector(sessions); equal(value, value); return value } }
export function makeSessionUseStore() { return selector => selector(store) }
export function makeSessionUseInput() { return selector => selector(input) }
export function sessionActions() { return { setView: id => { viewsSet.push(id); store.view = id }, setDraft: text => { store.draft = text }, setInspect: value => { inspectSet.push(value); store.inspect = value } } }
export function sessionInputActions() { return { setDraft: text => { inputDrafts.push(text); input.draft = text } } }
export function sessionBindMirror() { return _setDraft => { mirrorBinds += 1; return () => { unmirrors += 1 } } }
export function sessionReleaseImages() { return id => { releases.push(id) } }
export function sessionRenderSlot() { return (name, owner, options) => { slotCalls.push({ name, owner, options }); return { kind: name, props: {}, children: [] } } }
export function sessionOpen() { return id => { opened.push(id) } }
export function makeSessionTranslate() { return key => key === 'session.hierarchy' ? 'Session hierarchy' : key }
export function sessionRender(component, props) { cursor = 0; return component(props) }
export function sessionSlotCalls() { return slotCalls }
export function sessionOpened() { return opened }
export function sessionViewsSet() { return viewsSet }
export function sessionInspectSet() { return inspectSet }
export function sessionInputDrafts() { return inputDrafts }
export function sessionMirrorBinds() { return mirrorBinds }
export function sessionUnmirrors() { return unmirrors }
export function sessionReleases() { return releases }
export function sessionLedgerHooks() { return ledgerHooks }
export function sessionText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(sessionText).join('')
  return sessionText(value.children)
}
export function sessionFindAllKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = value.kind === kind ? [value] : []
  return own.concat(...(value.children ?? []).map(child => sessionFindAllKind(child, kind)))
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installSessionBench)]
    fn install_session_bench() -> JsValue;
    #[wasm_bindgen(js_name = sessionResetHooks)]
    fn session_reset_hooks();
    #[wasm_bindgen(js_name = sessionObject)]
    fn session_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = sessionSetTabs)]
    fn session_set_tabs(value: &Array);
    #[wasm_bindgen(js_name = sessionSetSession)]
    fn session_set_session(value: &JsValue);
    #[wasm_bindgen(js_name = sessionSetSessions)]
    fn session_set_sessions(value: &JsValue);
    #[wasm_bindgen(js_name = sessionSetStore)]
    fn session_set_store(value: &JsValue);
    #[wasm_bindgen(js_name = sessionSetInput)]
    fn session_set_input(value: &JsValue);
    #[wasm_bindgen(js_name = sessionViews)]
    fn session_views() -> JsValue;
    #[wasm_bindgen(js_name = makeSessionUseSession)]
    fn make_session_use_session() -> Function;
    #[wasm_bindgen(js_name = makeSessionUseSessions)]
    fn make_session_use_sessions() -> Function;
    #[wasm_bindgen(js_name = makeSessionUseStore)]
    fn make_session_use_store() -> Function;
    #[wasm_bindgen(js_name = makeSessionUseInput)]
    fn make_session_use_input() -> Function;
    #[wasm_bindgen(js_name = sessionActions)]
    fn session_actions() -> JsValue;
    #[wasm_bindgen(js_name = sessionInputActions)]
    fn session_input_actions() -> JsValue;
    #[wasm_bindgen(js_name = sessionBindMirror)]
    fn session_bind_mirror() -> Function;
    #[wasm_bindgen(js_name = sessionReleaseImages)]
    fn session_release_images() -> Function;
    #[wasm_bindgen(js_name = sessionRenderSlot)]
    fn session_render_slot() -> Function;
    #[wasm_bindgen(js_name = sessionOpen)]
    fn session_open() -> Function;
    #[wasm_bindgen(js_name = makeSessionTranslate)]
    fn make_session_translate() -> Function;
    #[wasm_bindgen(js_name = sessionRender)]
    fn session_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = sessionSlotCalls)]
    fn session_slot_calls() -> Array;
    #[wasm_bindgen(js_name = sessionOpened)]
    fn session_opened() -> Array;
    #[wasm_bindgen(js_name = sessionViewsSet)]
    fn session_views_set() -> Array;
    #[wasm_bindgen(js_name = sessionInspectSet)]
    fn session_inspect_set() -> Array;
    #[wasm_bindgen(js_name = sessionInputDrafts)]
    fn session_input_drafts() -> Array;
    #[wasm_bindgen(js_name = sessionMirrorBinds)]
    fn session_mirror_binds() -> u32;
    #[wasm_bindgen(js_name = sessionUnmirrors)]
    fn session_unmirrors() -> u32;
    #[wasm_bindgen(js_name = sessionReleases)]
    fn session_releases() -> Array;
    #[wasm_bindgen(js_name = sessionLedgerHooks)]
    fn session_ledger_hooks() -> u32;
    #[wasm_bindgen(js_name = sessionText)]
    fn session_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = sessionFindAllKind)]
    fn session_find_all_kind(value: &JsValue, kind: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    session_object(&array).unchecked_into()
}

fn tab(id: &str, label: &str) -> Object {
    object(&[
        ("id", JsValue::from_str(id)),
        ("label", JsValue::from_str(label)),
    ])
}

fn setup() -> (JsValue, JsValue) {
    let bench = install_session_bench();
    configure_client_ui_conversation_session(property(&bench, "React")).unwrap();
    (
        conversation_session_header_component().unwrap(),
        conversation_session_component().unwrap(),
    )
}

fn common_props() -> Vec<(&'static str, JsValue)> {
    vec![
        ("sessionId", JsValue::from_str("child")),
        ("useSession", make_session_use_session().into()),
        ("useSessions", make_session_use_sessions().into()),
        ("useStore", make_session_use_store().into()),
        ("actions", session_actions()),
        ("renderSlot", session_render_slot().into()),
        ("views", session_views()),
    ]
}

#[wasm_bindgen_test]
fn header_derives_subagent_ancestry_opens_parent_and_falls_back_to_chat_tab() {
    let (header, _) = setup();
    session_set_tabs(&Array::of3(
        tab("trajectory", "Trajectory").as_ref(),
        tab("chat", "Chat").as_ref(),
        tab("other", "Other").as_ref(),
    ));
    session_set_store(object(&[("view", JsValue::from_str("stale"))]).as_ref());
    session_set_session(
        object(&[
            ("composerPhase", JsValue::from_str("active")),
            ("blank", JsValue::FALSE),
        ])
        .as_ref(),
    );
    session_set_sessions(
        object(&[(
            "byId",
            object(&[
                (
                    "root",
                    object(&[
                        ("id", JsValue::from_str("root")),
                        ("displayTitle", JsValue::from_str("Root")),
                    ])
                    .into(),
                ),
                (
                    "child",
                    object(&[
                        ("id", JsValue::from_str("child")),
                        ("displayTitle", JsValue::from_str("Child")),
                        ("origin", JsValue::from_str("subagent")),
                        ("parentId", JsValue::from_str("root")),
                    ])
                    .into(),
                ),
            ])
            .into(),
        )])
        .as_ref(),
    );
    let mut entries = common_props();
    entries.extend([
        ("open", session_open().into()),
        ("t", make_session_translate().into()),
    ]);
    let tree = session_render(&header, object(&entries).as_ref());
    assert!(session_text(&tree).contains("Root/Child"));
    let buttons = session_find_all_kind(&tree, "button");
    let parent = buttons.get(0);
    property(&property(&parent, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(session_opened().get(0).as_string().as_deref(), Some("root"));
    assert_eq!(
        property(&property(&buttons.get(1), "props"), "disabled").as_bool(),
        Some(true)
    );
    let tab_buttons = buttons;
    let chat = tab_buttons.get(3);
    assert_eq!(
        property(&property(&chat, "props"), "aria-selected").as_bool(),
        Some(true)
    );
    property(&property(&tab_buttons.get(2), "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(
        session_views_set().get(0).as_string().as_deref(),
        Some("trajectory")
    );
    assert_eq!(session_ledger_hooks(), 1);
    assert_eq!(
        property(&session_slot_calls().get(0), "name")
            .as_string()
            .as_deref(),
        Some("conversation.session.header.actions")
    );
    assert_eq!(
        property(&session_slot_calls().get(1), "name")
            .as_string()
            .as_deref(),
        Some("conversation.session.header.utilities")
    );
}

#[wasm_bindgen_test]
fn blank_header_keeps_mounted_hidden_chrome_with_no_slot_dispatch() {
    let (header, _) = setup();
    session_set_tabs(&Array::of1(tab("chat", "Chat").as_ref()));
    session_set_store(object(&[("view", JsValue::NULL)]).as_ref());
    session_set_session(
        object(&[
            ("composerPhase", JsValue::from_str("blank")),
            ("blank", JsValue::TRUE),
        ])
        .as_ref(),
    );
    session_set_sessions(object(&[("byId", Object::new().into())]).as_ref());
    let mut entries = common_props();
    entries.extend([
        ("open", session_open().into()),
        ("t", make_session_translate().into()),
    ]);
    let tree = session_render(&header, object(&entries).as_ref());
    assert_eq!(
        property(&property(&tree, "props"), "aria-hidden").as_bool(),
        Some(true)
    );
    assert_eq!(session_slot_calls().length(), 0);
}

#[wasm_bindgen_test]
fn body_seeds_draft_binds_mirror_dispatches_active_view_and_acknowledges_inspect() {
    let (_, body) = setup();
    session_set_tabs(&Array::of2(
        tab("trajectory", "Trajectory").as_ref(),
        tab("chat", "Chat").as_ref(),
    ));
    session_set_store(
        object(&[
            ("view", JsValue::from_str("stale")),
            ("draft", JsValue::from_str("persisted")),
            (
                "inspect",
                object(&[("callId", JsValue::from_str("c1"))]).into(),
            ),
        ])
        .as_ref(),
    );
    session_set_input(object(&[("draft", JsValue::from_str(""))]).as_ref());
    session_set_session(
        object(&[
            ("composerPhase", JsValue::from_str("active")),
            ("blank", JsValue::FALSE),
        ])
        .as_ref(),
    );
    let mut entries = common_props();
    entries.extend([
        ("useInput", make_session_use_input().into()),
        ("inputActions", session_input_actions()),
        ("bindDraftMirror", session_bind_mirror().into()),
        ("releaseSessionImages", session_release_images().into()),
    ]);
    let tree = session_render(&body, object(&entries).as_ref());
    assert_eq!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .as_deref(),
        Some("seekdeep-conversation-session-viewArea")
    );
    assert_eq!(
        session_input_drafts().get(0).as_string().as_deref(),
        Some("persisted")
    );
    assert_eq!(session_mirror_binds(), 1);
    let call = session_slot_calls().get(0);
    assert_eq!(
        property(&call, "name").as_string().as_deref(),
        Some("conversation.view")
    );
    assert_eq!(
        property(&property(&call, "options"), "only")
            .as_string()
            .as_deref(),
        Some("chat")
    );
    assert_eq!(
        property(&property(&property(&call, "owner"), "inspect"), "callId")
            .as_string()
            .as_deref(),
        Some("c1")
    );
    property(&property(&call, "owner"), "onInspectDone")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(session_inspect_set().get(0).is_null());
    session_reset_hooks();
    assert_eq!(session_unmirrors(), 1);
    assert_eq!(
        session_releases().get(0).as_string().as_deref(),
        Some("child")
    );
}

#[wasm_bindgen_test]
fn blank_body_returns_null_after_mount_effects_and_missing_inspect_rehydrates_as_null() {
    let (_, body) = setup();
    session_set_tabs(&Array::of1(tab("chat", "Chat").as_ref()));
    session_set_store(
        object(&[("view", JsValue::NULL), ("draft", JsValue::from_str(""))]).as_ref(),
    );
    session_set_input(object(&[("draft", JsValue::from_str(""))]).as_ref());
    session_set_session(
        object(&[
            ("composerPhase", JsValue::from_str("blank")),
            ("blank", JsValue::TRUE),
        ])
        .as_ref(),
    );
    let mut entries = common_props();
    entries.extend([
        ("useInput", make_session_use_input().into()),
        ("inputActions", session_input_actions()),
        ("bindDraftMirror", session_bind_mirror().into()),
        ("releaseSessionImages", session_release_images().into()),
    ]);
    assert!(session_render(&body, object(&entries).as_ref()).is_null());
    assert_eq!(session_mirror_binds(), 1);
    session_set_session(
        object(&[
            ("composerPhase", JsValue::from_str("active")),
            ("blank", JsValue::FALSE),
        ])
        .as_ref(),
    );
    let _ = session_render(&body, object(&entries).as_ref());
    let call = session_slot_calls().get(0);
    assert!(property(&property(&call, "owner"), "inspect").is_null());
    session_reset_hooks();
    assert_eq!(session_releases().length(), 1);
}
