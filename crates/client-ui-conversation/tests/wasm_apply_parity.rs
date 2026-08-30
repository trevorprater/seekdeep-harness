//! Live browser conversation plugin assembly and teardown parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_runtime::{WasmConversationEventRegistry, WasmConversationViewRegistry};
use seekdeep_client_ui_conversation::{
    apply_client_ui_conversation, configure_client_ui_conversation_apply,
    conversation_inject_browser,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function makeConversationApplyBench() {
  const effects = []
  const services = new Map()
  const entries = []
  const specs = new Map([
    ['conversation', { kind: 'single', scope: 'session-maybe' }],
    ['details', { kind: 'single', scope: 'session' }],
    ['settings.general.item', { kind: 'list', scope: 'root' }],
  ])
  const pending = new Map()
  const localeCalls = []
  const pluginCalls = []
  const providerCalls = []
  const layoutCalls = []
  const sessionCalls = []
  const workspaceCalls = []
  const actionCalls = []
  const settingsCalls = []

  function own(dispose) {
    effects.push(typeof dispose === 'function' ? dispose : () => {})
    return dispose
  }
  function flush(name) {
    const installers = pending.get(name) ?? []
    pending.delete(name)
    for (const install of installers) install()
  }
  const slots = {
    register(options, component) {
      const entry = { options, component, inject: options.inject, store: options.store }
      entries.push(entry)
      for (const [name, spec] of Object.entries(options.children ?? {})) {
        specs.set(name, spec)
        flush(name)
      }
      return own(() => {
        const at = entries.indexOf(entry)
        if (at >= 0) entries.splice(at, 1)
        for (const name of Object.keys(options.children ?? {})) specs.delete(name)
      })
    },
    inject(name, install) {
      if (specs.has(name)) install()
      else pending.set(name, [...(pending.get(name) ?? []), install])
      return own(() => {})
    },
    entries(name) { return entries.filter(entry => entry.options.name === name) },
    spec(name) { return specs.get(name) },
    subscribe() { return () => {} },
    getVersion(name) { return entries.filter(entry => entry.options.name === name).length },
  }
  const locale = {
    register(namespace, dictionaries) {
      localeCalls.push({ namespace, dictionaries })
      return own(() => localeCalls.push({ namespace: `disposed:${namespace}` }))
    },
    bind() { return key => key === 'view.chat' ? 'Chat' : key },
  }
  const session = {
    sessionId: 's1',
    getSnapshot() { return { queue: [] } },
    prompt() { return Promise.resolve({ ok: true, value: { accepted: true } }) },
    subscribe() { return () => {} },
    cancel() { sessionCalls.push(['cancel']); return Promise.resolve({ ok: true, value: { accepted: true } }) },
    loadOlder() { sessionCalls.push(['loadOlder']); return Promise.resolve() },
    command() { return Promise.resolve({ ok: true, value: { matched: true } }) },
  }
  const scopedServices = new Map()
  const actx = {
    effect(setup) { return own(setup()) },
    provide(name, value) {
      scopedServices.set(name, value)
      return own(() => { if (scopedServices.get(name) === value) scopedServices.delete(name) })
    },
    get(name) { return scopedServices.has(name) ? scopedServices.get(name) : services.get(name) },
    on() { return () => {} },
  }
  const binding = { sessionId: 's1', session, ctx: actx }
  const sessions = {
    list: { getSnapshot() { return { byId: { s1: { cwd: '/workspace' } } } } },
    provide(definition) { providerCalls.push(definition); return own(() => providerCalls.splice(providerCalls.indexOf(definition), 1)) },
    binding(id) { return id === 's1' ? binding : undefined },
    scope(id) { return id === 's1' ? actx : undefined },
    scopeOf(scope) { return scope === actx ? 's1' : undefined },
    open(id) { sessionCalls.push(['open', id]) },
    fork(options) { sessionCalls.push(['fork', options]); return Promise.resolve('s1') },
  }
  const settingsStore = {
    value: { busyEnter: 'queue' },
    listeners: new Set(),
    getSnapshot() { return { value: this.value } },
    subscribe(listener) { this.listeners.add(listener); return () => this.listeners.delete(listener) },
    set(field, value) {
      settingsCalls.push([field, value])
      this.value = { ...this.value, [field]: value }
      for (const listener of this.listeners) listener()
      return Promise.resolve()
    },
  }
  const ctx = {
    effect(setup) { return own(setup()) },
    provide(name, value) {
      services.set(name, value)
      return own(() => { if (services.get(name) === value) services.delete(name) })
    },
    get(name) { return services.get(name) },
    plugin(entry) { pluginCalls.push(entry); return own(() => pluginCalls.splice(pluginCalls.indexOf(entry), 1)) },
    slots,
    layout: { openDetails() { layoutCalls.push('open') }, closeDetails() { layoutCalls.push('close') } },
    sessions,
    workspaces: {
      connectWorkspace(id) { workspaceCalls.push(['connectWorkspace', id]); return Promise.resolve('s1') },
      openPath(path) { workspaceCalls.push(['openPath', path]); return Promise.resolve() },
    },
    locale,
    connection: { api: {}, isLoopback: false },
    remote: {},
    settingsScope: { bind() { return settingsStore } },
  }
  for (const [key, value] of Object.entries(ctx)) services.set(key, value)
  actx.sessions = sessions
  const actions = {
    select(value) { actionCalls.push(['select', value]) },
    setInspect(value) { actionCalls.push(['setInspect', value]) },
    setView(value) { actionCalls.push(['setView', value]) },
  }
  const component = function Component() {}
  const components = {
    ConversationRoot: component, ConversationSession: component,
    ConversationSessionHeader: component, InputBar: component, ApprovalPanel: component,
    ChatView: component, StatsLine: component, DetailsPanel: component,
    EnterBehaviorRow: component, todoDockEntry: { name: 'todo' }, queueDockEntry: { name: 'queue' },
    UserMessageNodeView: component, ContextMessageNodeView: component, AssistantNodeView: component,
    CommandNodeView: component, ManualCompactionNodeView: component, CompactionNodeView: component,
    RetryNodeView: component, TurnErrorNodeView: component, TurnMaxTokensNodeView: component,
    TurnTailNodeView: component, UnknownNodeView: component,
  }
  return {
    ctx, actx, components, effects, entries, specs, services, localeCalls, pluginCalls,
    providerCalls, layoutCalls, sessionCalls, workspaceCalls, settingsStore, settingsCalls,
    binding, actions, actionCalls,
    defineStore(declaration) { return { declaration, id: Symbol('chat-store') } },
    uuid() { return '00000000-0000-4000-8000-000000000001' },
  }
}
export function setConversationApplyService(bench, name, value) {
  bench.ctx[name] = value
  bench.services.set(name, value)
}
export function conversationApplyEntries(bench, name) { return bench.entries.filter(entry => entry.options.name === name) }
export function conversationApplySpec(bench, name) { return bench.specs.get(name) }
export function conversationApplyService(bench, name) { return bench.services.get(name) }
export function conversationApplyProvider(bench) { return bench.providerCalls[0] }
export function conversationApplyCallInject(entry, ...args) { return entry.inject(...args) }
export function conversationApplyDisposeAll(bench) { for (const dispose of [...bench.effects].reverse()) dispose() }
export function conversationApplyComponents(bench) { return bench.components }
export function conversationApplyDefineStore(bench) { return bench.defineStore }
export function conversationApplyUuid(bench) { return bench.uuid }
export function conversationApplyArray(bench, name) { return bench[name] }
export function conversationApplyObjectIs(a, b) { return Object.is(a, b) }
export function conversationApplyProperty(value, name) { return value?.[name] }
export function conversationApplyCall(value, name, ...args) { return value[name](...args) }
export function conversationApplyProduce() {
  return (base, recipe) => {
    const draft = Array.isArray(base) ? [...base] : { ...base }
    recipe(draft)
    return draft
  }
}
"#)]
extern "C" {
    fn makeConversationApplyBench() -> JsValue;
    fn setConversationApplyService(bench: &JsValue, name: &str, value: &JsValue);
    fn conversationApplyEntries(bench: &JsValue, name: &str) -> Array;
    fn conversationApplySpec(bench: &JsValue, name: &str) -> JsValue;
    fn conversationApplyService(bench: &JsValue, name: &str) -> JsValue;
    fn conversationApplyProvider(bench: &JsValue) -> JsValue;
    fn conversationApplyCallInject(entry: &JsValue, first: &JsValue, second: &JsValue) -> JsValue;
    fn conversationApplyDisposeAll(bench: &JsValue);
    fn conversationApplyComponents(bench: &JsValue) -> JsValue;
    fn conversationApplyDefineStore(bench: &JsValue) -> Function;
    fn conversationApplyUuid(bench: &JsValue) -> Function;
    fn conversationApplyArray(bench: &JsValue, name: &str) -> Array;
    fn conversationApplyObjectIs(left: &JsValue, right: &JsValue) -> bool;
    fn conversationApplyProperty(value: &JsValue, name: &str) -> JsValue;
    fn conversationApplyCall(value: &JsValue, name: &str, first: &JsValue) -> JsValue;
    fn conversationApplyProduce() -> Function;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One registration ledger proves the complete apply surface.
fn apply_registers_services_slots_shared_store_and_tears_down() {
    let bench = makeConversationApplyBench();
    let ctx = property(&bench, "ctx");
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    setConversationApplyService(
        &bench,
        "conversationEvents",
        &events.face_for(ctx.clone()).unwrap(),
    );
    setConversationApplyService(
        &bench,
        "conversationViews",
        &views.face_for(ctx.clone()).unwrap(),
    );
    configure_client_ui_conversation_apply(
        conversationApplyComponents(&bench),
        conversationApplyDefineStore(&bench),
        conversationApplyUuid(&bench),
    )
    .unwrap();
    apply_client_ui_conversation(ctx).unwrap();

    assert_eq!(conversation_inject_browser().length(), 10);
    assert_eq!(events.entries().length(), 11);
    assert_eq!(views.entries().length(), 1);
    assert!(!conversationApplyService(&bench, "conversation").is_undefined());
    assert!(!conversationApplyProvider(&bench).is_undefined());
    assert_eq!(conversationApplyArray(&bench, "localeCalls").length(), 1);
    assert_eq!(conversationApplyArray(&bench, "pluginCalls").length(), 2);

    let root = conversationApplyEntries(&bench, "conversation");
    let session = conversationApplyEntries(&bench, "conversation.session");
    let header = conversationApplyEntries(&bench, "conversation.session.header");
    let composer = conversationApplyEntries(&bench, "conversation.composer.bar");
    let chat = conversationApplyEntries(&bench, "conversation.view");
    let details = conversationApplyEntries(&bench, "details");
    assert_eq!(root.length(), 1);
    assert_eq!(session.length(), 1);
    assert_eq!(header.length(), 1);
    assert_eq!(composer.length(), 1);
    assert_eq!(chat.length(), 1);
    assert_eq!(details.length(), 1);
    assert_eq!(
        conversationApplyEntries(&bench, "settings.general.item").length(),
        1
    );
    assert_eq!(
        conversationApplyEntries(&bench, "conversation.composer.dock").length(),
        1
    );
    assert_eq!(
        conversationApplyEntries(&bench, "conversation.chat.node").length(),
        12
    );
    assert_eq!(
        property(&property(&chat.get(0), "options"), "id")
            .as_string()
            .as_deref(),
        Some("chat")
    );
    assert_eq!(
        property(&property(&chat.get(0), "options"), "order").as_f64(),
        Some(0.0)
    );
    assert!(
        property(
            &conversationApplySpec(&bench, "conversation.chat.node"),
            "inject"
        )
        .is_object()
    );
    assert_eq!(
        property(
            &conversationApplySpec(&bench, "conversation.hero.workspace"),
            "scope"
        )
        .as_string()
        .as_deref(),
        Some("root")
    );

    let shared = conversationApplyProperty(&session.get(0), "store");
    assert!(conversationApplyObjectIs(
        &shared,
        &conversationApplyProperty(&header.get(0), "store")
    ));
    assert!(conversationApplyObjectIs(
        &shared,
        &conversationApplyProperty(&chat.get(0), "store")
    ));
    assert!(conversationApplyObjectIs(
        &shared,
        &conversationApplyProperty(&details.get(0), "store")
    ));

    let absent =
        conversationApplyCallInject(&composer.get(0), &JsValue::UNDEFINED, &JsValue::UNDEFINED);
    assert!(conversationApplyProperty(&absent, "keyboard").is_undefined());
    let hooks = conversationApplyProperty(&absent, "hooks");
    assert!(conversationApplyProperty(&hooks, "notices").is_object());
    assert_eq!(
        conversationApplyCall(
            &conversationApplyProperty(&hooks, "notices"),
            "getSnapshot",
            &JsValue::UNDEFINED
        ),
        JsValue::NULL
    );

    conversationApplyDisposeAll(&bench);
    assert_eq!(events.entries().length(), 0);
    assert_eq!(views.entries().length(), 0);
    assert!(conversationApplyService(&bench, "conversation").is_undefined());
    assert_eq!(conversationApplyEntries(&bench, "conversation").length(), 0);
    assert_eq!(
        conversationApplyEntries(&bench, "conversation.view").length(),
        0
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // One mounted fixture drives every public inject family.
async fn inject_faces_drive_provider_session_layout_workspace_and_details_actions()
-> Result<(), JsValue> {
    let bench = makeConversationApplyBench();
    let ctx = property(&bench, "ctx");
    let events = WasmConversationEventRegistry::new();
    let views = WasmConversationViewRegistry::new();
    setConversationApplyService(
        &bench,
        "conversationEvents",
        &events.face_for(ctx.clone()).unwrap(),
    );
    setConversationApplyService(
        &bench,
        "conversationViews",
        &views.face_for(ctx.clone()).unwrap(),
    );
    configure_client_ui_conversation_apply(
        conversationApplyComponents(&bench),
        conversationApplyDefineStore(&bench),
        conversationApplyUuid(&bench),
    )
    .unwrap();
    apply_client_ui_conversation(ctx).unwrap();

    seekdeep_client_runtime::install_store_produce(conversationApplyProduce());

    let provider = conversationApplyProvider(&bench);
    let provider_output = property(&provider, "resolve")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&provider, &property(&bench, "binding"))
        .unwrap();
    assert!(property(&property(&provider_output, "hooks"), "input").is_object());
    assert!(property(&property(&provider_output, "props"), "inputActions").is_object());
    let scoped_service = conversationApplyCall(
        &property(&bench, "actx"),
        "get",
        &JsValue::from_str("conversation"),
    );
    assert!(!conversationApplyObjectIs(
        &scoped_service,
        &conversationApplyService(&bench, "conversation")
    ));

    let composer = conversationApplyEntries(&bench, "conversation.composer.bar").get(0);
    let composer_face =
        conversationApplyCallInject(&composer, &JsValue::from_str("s1"), &JsValue::UNDEFINED);
    assert!(property(&composer_face, "keyboard").is_object());
    let resolve_mode = property(&composer_face, "resolveSubmitMode")
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        resolve_mode
            .call3(
                &composer_face,
                &JsValue::TRUE,
                &JsValue::from_str("enter"),
                &JsValue::TRUE,
            )?
            .as_string()
            .as_deref(),
        Some("queue")
    );
    let settings_entry = conversationApplyEntries(&bench, "settings.general.item").get(0);
    let settings_face =
        conversationApplyCallInject(&settings_entry, &JsValue::UNDEFINED, &JsValue::UNDEFINED);
    property(&settings_face, "setBusyEnter")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&settings_face, &JsValue::from_str("steer"))?;
    assert_eq!(
        resolve_mode
            .call3(
                &composer_face,
                &JsValue::TRUE,
                &JsValue::from_str("enter"),
                &JsValue::TRUE,
            )?
            .as_string()
            .as_deref(),
        Some("steer")
    );
    assert_eq!(conversationApplyArray(&bench, "settingsCalls").length(), 1);
    property(&composer_face, "stop")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&composer_face)
        .unwrap();
    let command = property(&composer_face, "command")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&composer_face, &JsValue::from_str("/help"))?;
    assert_eq!(
        JsFuture::from(Promise::resolve(&command)).await?.as_bool(),
        Some(true)
    );

    let root = conversationApplyEntries(&bench, "conversation").get(0);
    let root_face =
        conversationApplyCallInject(&root, &JsValue::from_str("s1"), &JsValue::UNDEFINED);
    let selected = property(&root_face, "selectWorkspace")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&root_face, &JsValue::from_str("workspace-1"))?;
    JsFuture::from(Promise::resolve(&selected)).await?;

    let chat = conversationApplyEntries(&bench, "conversation.view").get(0);
    let chat_face = conversationApplyCallInject(
        &chat,
        &JsValue::from_str("s1"),
        &property(&bench, "actions"),
    );
    property(&chat_face, "openDetails")
        .dyn_into::<Function>()
        .unwrap()
        .call1(
            &chat_face,
            &js_sys::JSON::parse(r#"{"turnSeq":2,"callId":"c1"}"#)?,
        )?;
    property(&chat_face, "openFile")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&chat_face, &JsValue::from_str("src/a.ts"))?;
    property(&chat_face, "loadOlder")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&chat_face)?;
    property(&chat_face, "forkAt")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&chat_face, &JsValue::from_f64(17.0))?;

    let session = conversationApplyEntries(&bench, "conversation.session").get(0);
    let session_face = conversationApplyCallInject(
        &session,
        &JsValue::from_str("s1"),
        &property(&bench, "actions"),
    );
    let tabs = property(&property(&session_face, "views"), "list")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)?;
    assert_eq!(Array::from(&tabs).length(), 1);

    let details = conversationApplyEntries(&bench, "details").get(0);
    let details_face = conversationApplyCallInject(
        &details,
        &JsValue::from_str("s1"),
        &property(&bench, "actions"),
    );
    property(&details_face, "closeDetails")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&details_face)?;

    for _ in 0..3 {
        JsFuture::from(Promise::resolve(&JsValue::UNDEFINED)).await?;
    }

    assert!(conversationApplyArray(&bench, "sessionCalls").length() >= 4);
    assert_eq!(conversationApplyArray(&bench, "layoutCalls").length(), 2);
    assert_eq!(conversationApplyArray(&bench, "workspaceCalls").length(), 2);
    assert_eq!(conversationApplyArray(&bench, "actionCalls").length(), 1);
    conversationApplyDisposeAll(&bench);
    Ok::<(), JsValue>(())
}
