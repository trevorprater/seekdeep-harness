//! Live Rust/WASM permission controller, command decoration, and Settings row parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_permission_presets::{
    WasmPermissionPresetSettingsController, apply_client_ui_permission_presets,
    configure_client_ui_permission_presets, exported_permission_row_component,
    permission_default_of_js, permission_presets_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const styles = []
if (typeof globalThis.document === 'undefined') {
  globalThis.document = {
    currentScript: null,
    querySelector(selector) {
      const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
      if (match === null) return null
      const plugin = JSON.parse(match[1])
      return styles.find(node => node.attributes['data-plugin'] === plugin) ?? null
    },
    querySelectorAll(selector) { const node = this.querySelector(selector); return node === null ? [] : [node] },
    createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(name, value) { this.attributes[name] = value } } },
    head: { appendChild(node) { styles.push(node); return node } },
  }
}

const SETTINGS_EN = {
  title: 'Permission', description: 'Choose the default permission mode for new sessions',
  loading: 'Loading', unavailable: 'Unavailable', 'confirm.title': 'Enable Full access?',
  'confirm.description': 'Full access lets new sessions reduce confirmation steps and perform more actions directly, including sensitive operations, file changes, or external commands. Only use it when you trust subsequent tasks.',
  'confirm.acknowledge': 'I understand the risks and want to continue', 'confirm.cancel': 'Cancel',
  'confirm.enable': 'Enable Full access',
}

function schema() {
  return { uid: 5, refs: {
    1: { type: 'const', value: 'read-only' },
    2: { type: 'const', value: 'workspace-write' },
    3: { type: 'const', value: 'danger-full-access' },
    4: { type: 'union', list: [1, 2, 3] },
    5: { type: 'object', dict: { defaultPreset: 4 } },
  } }
}
function view(defaultPreset, revision = 0) {
  return { ns: 'permission', schema: schema(), value: { defaultPreset }, base: { defaultPreset: 'read-only' }, applies: 'live', secrets: [], revision }
}
function ok(value) { return { rpcId: 'test', result: { ok: true, value } } }

export function makePermissionApiBench() {
  const payloads = []
  let description = { writable: true, hasDocument: false, namespaces: [view('read-only', 4)] }
  let mutation = view('workspace-write', 5)
  let describeFailure, mutateFailure
  return {
    payloads,
    api: { settings: {
      describe(payload) {
        payloads.push({ method: 'describe', payload })
        return Promise.resolve(describeFailure === undefined ? ok(description) : { rpcId: 'test', result: { ok: false, error: { code: 'internal', message: describeFailure, details: {} } } })
      },
      mutate(payload) {
        payloads.push({ method: 'mutate', payload })
        return Promise.resolve(mutateFailure === undefined ? ok(mutation) : { rpcId: 'test', result: { ok: false, error: { code: 'settings-conflict', message: mutateFailure, details: {} } } })
      },
    } },
    setDescription(value) { description = value }, setMutation(value) { mutation = value },
    setDescribeFailure(value) { describeFailure = value }, setMutateFailure(value) { mutateFailure = value },
  }
}
export function permissionView(value, revision = 0) { return view(value, revision) }
export function permissionDescription(value, writable = true) { return { writable, hasDocument: false, namespaces: value === undefined ? [] : [value] } }
export function permissionPayloads(bench) { return bench.payloads }
export function permissionSetDescription(bench, value) { bench.setDescription(value) }
export function permissionSetMutation(bench, value) { bench.setMutation(value) }
export function permissionSetMutateFailure(bench, value) { bench.setMutateFailure(value) }
export function permissionStoreSnapshot(store) { return store.getSnapshot() }

function hookRuntime() {
  const state = [], effects = []
  let stateCursor = 0, effectCursor = 0
  const Fragment = Symbol('Fragment')
  const React = {
    Fragment,
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false) } },
    useState(initial) {
      const index = stateCursor++
      if (!(index in state)) state[index] = typeof initial === 'function' ? initial() : initial
      const set = value => { state[index] = typeof value === 'function' ? value(state[index]) : value }
      return [state[index], set]
    },
    useEffect(run, deps) {
      const index = effectCursor++
      const previous = effects[index]
      const changed = previous === undefined || deps.length !== previous.deps.length || deps.some((value, i) => !Object.is(value, previous.deps[i]))
      if (changed) { previous?.cleanup?.(); const cleanup = run(); effects[index] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined } }
    },
  }
  return { React, render(component, props) { stateCursor = 0; effectCursor = 0; return component(props) }, dispose() { for (const effect of effects.reverse()) effect?.cleanup?.() } }
}
function textOf(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  return (node.children ?? []).map(textOf).join('')
}
function all(node, predicate, rows = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return rows
  if (!Array.isArray(node) && predicate(node)) rows.push(node)
  for (const child of Array.isArray(node) ? node : node.children ?? []) all(child, predicate, rows)
  return rows
}
function one(node, predicate) { return all(node, predicate)[0] }

export function makePermissionRowBench() {
  const hooks = hookRuntime(), loads = [], selections = []
  const snapshot = { status: 'idle', error: null, writable: false, currentValue: '', options: [], revision: 0 }
  const props = {
    load() { snapshot.status = 'loading'; loads.push(true); return Promise.resolve() },
    select(value) { selections.push(value); return Promise.resolve() },
    usePermission(select) { return select(snapshot) },
    t(key) { return SETTINGS_EN[key] ?? key },
  }
  return { hooks, React: hooks.React, primitives: { IconChevronDownOutline14: 'IconChevronDownOutline14', Menu: 'Menu', RiskConfirmation: 'RiskConfirmation' }, snapshot, props, loads, selections }
}
export function permissionRowRender(bench, component) { return bench.hooks.render(component, bench.props) }
export function permissionRowSnapshot(bench) { return bench.snapshot }
export function permissionRowLoads(bench) { return bench.loads }
export function permissionRowSelections(bench) { return bench.selections }
export function permissionRowMenu(tree) { return one(tree, node => node.kind === 'Menu') }
export function permissionRowRisk(tree) { return one(tree, node => node.kind === 'RiskConfirmation') }
export function permissionRowButton(tree) { return permissionRowMenu(tree)?.props?.anchor }
export function permissionRowAlert(tree) { return one(tree, node => node.props?.role === 'alert') }
export function permissionRowText(tree) { return textOf(tree) }
export function permissionRowCall(node, name, value) { return node.props[name](value) }

export function makePermissionPluginBench() {
  const row = makePermissionRowBench(), effects = [], entries = [], localeEntries = [], events = new Map(), commands = [], values = new Map()
  let decoration
  let commandMode = 'success'
  const apiBench = makePermissionApiBench()
  const own = dispose => { effects.push(dispose); return dispose }
  const locale = {
    register(namespace, languageOrDictionaries, dictionary) {
      const entry = dictionary === undefined
        ? { namespace, dictionaries: languageOrDictionaries }
        : { namespace, language: languageOrDictionaries, dictionary }
      localeEntries.push(entry); return () => localeEntries.splice(localeEntries.indexOf(entry), 1)
    },
    bind(namespace) {
      return key => {
        for (let index = localeEntries.length - 1; index >= 0; index--) {
          const entry = localeEntries[index]
          if (entry.namespace !== namespace) continue
          const dictionary = entry.language === 'en' ? entry.dictionary : entry.dictionaries?.en
          if (dictionary?.[key] !== undefined) return dictionary[key]
        }
        return key
      }
    },
  }
  const session = id => ({
    projections: { faceOf(key) { return { getSnapshot() { return key === 'permissions' ? values.get(id) : undefined } } } },
    command(line) {
      commands.push(line)
      if (commandMode === 'failure') return Promise.resolve({ ok: false, error: { code: 'internal', message: 'boom' } })
      return Promise.resolve({ ok: true, value: { matched: commandMode !== 'unmatched' } })
    },
  })
  const ctx = {
    commandUi: { decorate(value) { decoration = value; return () => { decoration = undefined } } },
    sessions: { binding(id) { return values.has(id) ? { sessionId: id, session: session(id) } : undefined } },
    slots: { inject(name, install) { return own(install()) }, register(options, component) { const entry = { options, component }; entries.push(entry); return () => entries.splice(entries.indexOf(entry), 1) } },
    locale,
    connection: { api: apiBench.api },
    remote: { $on(name, listener) { events.set('remote:' + name, listener); return () => events.delete('remote:' + name) } },
    on(name, listener) { events.set(name, listener); return () => events.delete(name) },
    effect(setup) { return own(setup()) },
  }
  return { ...row, ctx, effects, entries, localeEntries, events, commands, values, apiBench,
    get decoration() { return decoration }, setCommandMode(value) { commandMode = value },
    dispatch(name, ...args) { events.get(name)?.(...args) },
  }
}
export function permissionPluginDecoration(bench) { return bench.decoration }
export function permissionPluginEntries(bench) { return bench.entries }
export function permissionPluginLocales(bench) { return bench.localeEntries }
export function permissionPluginCommands(bench) { return bench.commands }
export function permissionPluginSetValue(bench, id, value) { bench.values.set(id, value) }
export function permissionPluginSetCommandMode(bench, mode) { bench.setCommandMode(mode) }
export function permissionPluginDispatch(bench, name, first, second) { bench.dispatch(name, first, second) }
export function permissionPluginMethodCount(bench, method) { return bench.apiBench.payloads.filter(row => row.method === method).length }
export function permissionPluginDispose(bench) { bench.hooks.dispose(); for (const dispose of bench.effects.splice(0).reverse()) dispose() }
export function permissionSession(id) { return { sessionId: id } }
export function permissionSelectValue(current = 'workspace-write') { return { options: [
  { value: 'read-only', name: 'read-only', description: 'Reads only.' },
  { value: 'workspace-write', name: 'workspace-write' },
  { value: 'danger-full-access', name: 'danger-full-access' },
  { value: 'custom', name: 'Custom' },
], currentValue: current } }
export function permissionStyleCount() { return styles.filter(node => node.attributes['data-plugin'] === '@seekdeep-ai/seekdeep-client-ui-permission-presets').length }
export function permissionTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
"#)]
extern "C" {
    fn makePermissionApiBench() -> JsValue;
    fn permissionView(value: &str, revision: u32) -> JsValue;
    fn permissionDescription(value: &JsValue, writable: bool) -> JsValue;
    fn permissionPayloads(bench: &JsValue) -> Array;
    fn permissionSetDescription(bench: &JsValue, value: &JsValue);
    fn permissionSetMutation(bench: &JsValue, value: &JsValue);
    fn permissionSetMutateFailure(bench: &JsValue, value: &str);
    fn permissionStoreSnapshot(store: &JsValue) -> JsValue;
    fn makePermissionRowBench() -> JsValue;
    fn permissionRowRender(bench: &JsValue, component: &Function) -> JsValue;
    fn permissionRowSnapshot(bench: &JsValue) -> JsValue;
    fn permissionRowLoads(bench: &JsValue) -> Array;
    fn permissionRowSelections(bench: &JsValue) -> Array;
    fn permissionRowMenu(tree: &JsValue) -> JsValue;
    fn permissionRowRisk(tree: &JsValue) -> JsValue;
    fn permissionRowButton(tree: &JsValue) -> JsValue;
    fn permissionRowAlert(tree: &JsValue) -> JsValue;
    fn permissionRowText(tree: &JsValue) -> String;
    fn permissionRowCall(node: &JsValue, name: &str, value: &JsValue) -> JsValue;
    fn makePermissionPluginBench() -> JsValue;
    fn permissionPluginDecoration(bench: &JsValue) -> JsValue;
    fn permissionPluginEntries(bench: &JsValue) -> Array;
    fn permissionPluginLocales(bench: &JsValue) -> Array;
    fn permissionPluginCommands(bench: &JsValue) -> Array;
    fn permissionPluginSetValue(bench: &JsValue, id: &str, value: &JsValue);
    fn permissionPluginSetCommandMode(bench: &JsValue, mode: &str);
    fn permissionPluginDispatch(bench: &JsValue, name: &str, first: &JsValue, second: &JsValue);
    fn permissionPluginMethodCount(bench: &JsValue, method: &str) -> u32;
    fn permissionPluginDispose(bench: &JsValue);
    fn permissionSession(id: &str) -> JsValue;
    fn permissionSelectValue(current: &str) -> JsValue;
    fn permissionStyleCount() -> u32;
    fn permissionTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    let direct = Reflect::get(value, &JsValue::from_str(key)).unwrap();
    if !direct.is_undefined() {
        return direct;
    }
    let props = Reflect::get(value, &JsValue::from_str("props")).unwrap_or(JsValue::UNDEFINED);
    Reflect::get(&props, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn call(value: &JsValue, key: &str) -> Function {
    property(value, key).dyn_into().unwrap()
}

fn configure(bench: &JsValue) {
    configure_client_ui_permission_presets(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
}

#[wasm_bindgen_test(async)]
async fn controller_store_schema_and_optimistic_mutation_are_live() {
    let bench = makePermissionApiBench();
    let controller = WasmPermissionPresetSettingsController::new(property(&bench, "api")).unwrap();
    let store = controller.store();
    let notifications = Array::new();
    let values = notifications.clone();
    let listener = Closure::wrap(Box::new(move || {
        values.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let off = call(&store, "subscribe")
        .call1(&store, &listener.into_js_value())
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    JsFuture::from(controller.load()).await.unwrap();
    let snapshot = permissionStoreSnapshot(&store);
    assert_eq!(
        property(&snapshot, "status").as_string().as_deref(),
        Some("ready")
    );
    assert_eq!(
        property(&snapshot, "currentValue").as_string().as_deref(),
        Some("read-only")
    );
    assert_eq!(property(&snapshot, "revision").as_f64(), Some(4.0));
    assert_eq!(Array::from(&property(&snapshot, "options")).length(), 3);
    permissionSetMutation(&bench, &permissionView("workspace-write", 5));
    JsFuture::from(controller.select("workspace-write".to_owned()))
        .await
        .unwrap();
    let payloads = permissionPayloads(&bench);
    let mutation = property(&payloads.get(1), "payload");
    assert_eq!(
        property(&mutation, "ns").as_string().as_deref(),
        Some("permission")
    );
    assert_eq!(property(&mutation, "expectedRevision").as_f64(), Some(4.0));
    let operation = Array::from(&property(&mutation, "ops")).get(0);
    assert_eq!(
        property(&operation, "value").as_string().as_deref(),
        Some("workspace-write")
    );
    assert_eq!(notifications.length(), 4);
    off.call0(&JsValue::UNDEFINED).unwrap();

    let resolved = permission_default_of_js(permissionView("danger-full-access", 7)).unwrap();
    assert_eq!(
        property(&resolved, "currentValue").as_string().as_deref(),
        Some("danger-full-access")
    );
    permissionSetMutateFailure(&bench, "stale");
    JsFuture::from(controller.select("read-only".to_owned()))
        .await
        .unwrap();
    assert_eq!(
        property(&permissionStoreSnapshot(&store), "error")
            .as_string()
            .as_deref(),
        Some("stale")
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn plugin_decoration_options_commands_slots_locales_and_cleanup_are_live() {
    let bench = makePermissionPluginBench();
    configure(&bench);
    apply_client_ui_permission_presets(property(&bench, "ctx")).unwrap();
    assert_eq!(
        permission_presets_inject()
            .iter()
            .map(|value| value.as_string().unwrap())
            .collect::<Vec<_>>(),
        [
            "commandUi",
            "sessions",
            "slots",
            "locale",
            "connection",
            "remote"
        ]
    );
    assert_eq!(permissionStyleCount(), 1);
    assert_eq!(permissionPluginLocales(&bench).length(), 3);
    assert_eq!(permissionPluginEntries(&bench).length(), 1);
    let row = permissionPluginEntries(&bench).get(0);
    let row_options = property(&row, "options");
    assert_eq!(
        property(&row_options, "id").as_string().as_deref(),
        Some("permission")
    );
    assert_eq!(property(&row_options, "order").as_f64(), Some(-20.0));
    let injected = call(&row_options, "inject").call0(&row_options).unwrap();
    assert!(!property(&property(&injected, "hooks"), "permission").is_undefined());
    JsFuture::from(Promise::resolve(
        &call(&injected, "load").call0(&injected).unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(permissionPluginMethodCount(&bench, "describe"), 1);
    permissionPluginDispatch(
        &bench,
        "remote:settings/document-updated",
        &JsValue::from_str("another"),
        &JsValue::from_f64(1.0),
    );
    JsFuture::from(permissionTick()).await.unwrap();
    assert_eq!(permissionPluginMethodCount(&bench, "describe"), 1);
    permissionPluginDispatch(
        &bench,
        "remote:settings/document-updated",
        &JsValue::from_str("permission"),
        &JsValue::from_f64(2.0),
    );
    JsFuture::from(permissionTick()).await.unwrap();
    assert_eq!(permissionPluginMethodCount(&bench, "describe"), 2);
    permissionPluginDispatch(
        &bench,
        "connection/reset",
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    );
    JsFuture::from(permissionTick()).await.unwrap();
    assert_eq!(permissionPluginMethodCount(&bench, "describe"), 3);
    JsFuture::from(Promise::resolve(
        &call(&injected, "select")
            .call1(&injected, &JsValue::from_str("read-only"))
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(permissionPluginMethodCount(&bench, "mutate"), 1);

    let decoration = permissionPluginDecoration(&bench);
    assert_eq!(
        property(&decoration, "name").as_string().as_deref(),
        Some("permission")
    );
    let ui = property(&decoration, "ui");
    assert_eq!(
        property(&ui, "kind").as_string().as_deref(),
        Some("popupSelect")
    );
    let session = permissionSession("s1");
    assert!(
        call(&ui, "options")
            .call2(&ui, &permissionSession("ghost"), &JsValue::UNDEFINED)
            .is_err()
    );
    assert_eq!(
        call(&decoration, "available")
            .call1(&decoration, &session)
            .unwrap()
            .as_bool(),
        Some(false)
    );
    permissionPluginSetValue(&bench, "s1", &permissionSelectValue("workspace-write"));
    assert_eq!(
        call(&decoration, "available")
            .call1(&decoration, &session)
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let options = call(&ui, "options")
        .call2(&ui, &session, &JsValue::UNDEFINED)
        .unwrap();
    let options = Array::from(&JsFuture::from(Promise::resolve(&options)).await.unwrap());
    assert_eq!(options.length(), 3);
    assert_eq!(
        property(&options.get(0), "label").as_string().as_deref(),
        Some("Read Only")
    );
    assert_eq!(property(&options.get(1), "active").as_bool(), Some(true));
    assert_eq!(
        property(&options.get(0), "detail").as_string().as_deref(),
        Some("Reads only.")
    );
    let confirmation = property(&options.get(2), "confirmation");
    assert_eq!(
        property(&confirmation, "title").as_string().as_deref(),
        Some("Enable Full access?")
    );
    permissionPluginSetValue(&bench, "s1", &permissionSelectValue("custom"));
    let custom = call(&ui, "options")
        .call2(&ui, &session, &JsValue::UNDEFINED)
        .unwrap();
    let custom = Array::from(&JsFuture::from(Promise::resolve(&custom)).await.unwrap());
    assert!(
        custom
            .iter()
            .all(|option| property(&option, "active").is_undefined())
    );
    permissionPluginSetValue(&bench, "s1", &permissionSelectValue("workspace-write"));

    let selected = call(&ui, "onSelect")
        .call2(&ui, &options.get(2), &session)
        .unwrap();
    JsFuture::from(Promise::resolve(&selected)).await.unwrap();
    assert_eq!(
        permissionPluginCommands(&bench)
            .get(0)
            .as_string()
            .as_deref(),
        Some("/permission danger-full-access")
    );
    permissionPluginSetCommandMode(&bench, "failure");
    let failure = call(&ui, "onSelect")
        .call2(&ui, &options.get(0), &session)
        .unwrap();
    assert!(JsFuture::from(Promise::resolve(&failure)).await.is_err());
    permissionPluginSetCommandMode(&bench, "unmatched");
    let unmatched = call(&ui, "onSelect")
        .call2(&ui, &options.get(0), &session)
        .unwrap();
    assert!(JsFuture::from(Promise::resolve(&unmatched)).await.is_err());
    let ghost = call(&ui, "onSelect")
        .call2(&ui, &options.get(0), &permissionSession("ghost"))
        .unwrap();
    assert!(JsFuture::from(Promise::resolve(&ghost)).await.is_err());

    permissionPluginDispose(&bench);
    assert!(permissionPluginDecoration(&bench).is_undefined());
    assert_eq!(permissionPluginEntries(&bench).length(), 0);
    assert_eq!(permissionPluginLocales(&bench).length(), 0);
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn permission_row_menu_risk_gate_states_and_errors_are_live() {
    let bench = makePermissionRowBench();
    configure(&bench);
    let component = exported_permission_row_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let initial = permissionRowRender(&bench, &component);
    assert_eq!(permissionRowLoads(&bench).length(), 1);
    let loading = permissionRowRender(&bench, &component);
    assert_eq!(
        permissionRowText(&initial),
        "PermissionChoose the default permission mode for new sessions"
    );
    assert_eq!(permissionRowText(&permissionRowButton(&loading)), "Loading");
    assert_eq!(
        property(&permissionRowButton(&loading), "disabled").as_bool(),
        Some(true)
    );

    let snapshot = permissionRowSnapshot(&bench);
    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("ready"),
    )
    .unwrap();
    Reflect::set(&snapshot, &JsValue::from_str("writable"), &JsValue::TRUE).unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("currentValue"),
        &JsValue::from_str("read-only"),
    )
    .unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("options"),
        &js_sys::JSON::parse(r#"[{"id":"read-only","label":"Read Only"},{"id":"workspace-write","label":"Workspace Write"},{"id":"danger-full-access","label":"Full access"}]"#).unwrap(),
    )
    .unwrap();
    let mut tree = permissionRowRender(&bench, &component);
    let button = permissionRowButton(&tree);
    assert_eq!(permissionRowText(&button), "Read Only");
    permissionRowCall(&button, "onClick", &JsValue::UNDEFINED);
    tree = permissionRowRender(&bench, &component);
    let menu = permissionRowMenu(&tree);
    assert_eq!(property(&menu, "open").as_bool(), Some(true));
    permissionRowCall(&menu, "onSelect", &JsValue::from_str("read-only"));
    assert_eq!(permissionRowSelections(&bench).length(), 0);
    tree = permissionRowRender(&bench, &component);
    permissionRowCall(
        &permissionRowMenu(&tree),
        "onSelect",
        &JsValue::from_str("workspace-write"),
    );
    assert_eq!(
        permissionRowSelections(&bench)
            .get(0)
            .as_string()
            .as_deref(),
        Some("workspace-write")
    );

    tree = permissionRowRender(&bench, &component);
    permissionRowCall(&permissionRowButton(&tree), "onClick", &JsValue::UNDEFINED);
    tree = permissionRowRender(&bench, &component);
    permissionRowCall(
        &permissionRowMenu(&tree),
        "onSelect",
        &JsValue::from_str("danger-full-access"),
    );
    tree = permissionRowRender(&bench, &component);
    let risk = permissionRowRisk(&tree);
    assert_eq!(property(&risk, "open").as_bool(), Some(true));
    assert_eq!(property(&risk, "acknowledged").as_bool(), Some(false));
    permissionRowCall(&risk, "onCancel", &JsValue::UNDEFINED);
    tree = permissionRowRender(&bench, &component);
    assert_eq!(
        property(&permissionRowRisk(&tree), "open").as_bool(),
        Some(false)
    );
    permissionRowCall(&permissionRowButton(&tree), "onClick", &JsValue::UNDEFINED);
    tree = permissionRowRender(&bench, &component);
    permissionRowCall(
        &permissionRowMenu(&tree),
        "onSelect",
        &JsValue::from_str("danger-full-access"),
    );
    tree = permissionRowRender(&bench, &component);
    let risk = permissionRowRisk(&tree);
    permissionRowCall(&risk, "onAcknowledgedChange", &JsValue::TRUE);
    tree = permissionRowRender(&bench, &component);
    permissionRowCall(&permissionRowRisk(&tree), "onConfirm", &JsValue::UNDEFINED);
    assert_eq!(
        permissionRowSelections(&bench)
            .get(1)
            .as_string()
            .as_deref(),
        Some("danger-full-access")
    );
    tree = permissionRowRender(&bench, &component);
    assert_eq!(
        property(&permissionRowRisk(&tree), "open").as_bool(),
        Some(false)
    );

    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("error"),
    )
    .unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("error"),
        &JsValue::from_str("changed elsewhere"),
    )
    .unwrap();
    tree = permissionRowRender(&bench, &component);
    assert_eq!(
        permissionRowText(&permissionRowAlert(&tree)),
        "changed elsewhere"
    );
    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("ready"),
    )
    .unwrap();
    Reflect::set(&snapshot, &JsValue::from_str("error"), &JsValue::NULL).unwrap();
    Reflect::set(&snapshot, &JsValue::from_str("writable"), &JsValue::FALSE).unwrap();
    tree = permissionRowRender(&bench, &component);
    assert_eq!(
        property(&permissionRowButton(&tree), "disabled").as_bool(),
        Some(true)
    );
    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("unavailable"),
    )
    .unwrap();
    assert!(permissionRowRender(&bench, &component).is_null());
}
