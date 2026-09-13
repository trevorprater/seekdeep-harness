//! Live model-settings stores, components, writes, and plugin assembly.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use seekdeep_client_ui_settings_models::{
    apply_client_ui_settings_models, configure_client_ui_settings_models,
    create_models_settings_controller, create_welcome_notice_controller,
    custom_provider_card_component, deepseek_models_editor_component, model_list_editor_component,
    models_section_component, provider_editor_component, refresh_models_if_loaded,
    settings_models_inject, welcome_notice_component,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
const flatten = values => values.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
function engine() {
  const states = [], refs = [], effects = [], memos = []
  let si = 0, ri = 0, ei = 0, mi = 0
  const Fragment = Symbol('Fragment')
  const React = {
    Fragment,
    createElement(kind, supplied, ...children) {
      const flat = flatten(children)
      const props = { ...(supplied ?? {}) }
      if (flat.length === 1) props.children = flat[0]
      else if (flat.length > 1) props.children = flat
      const node = { kind, props, children: flat, focused: false, focus() { this.focused = true } }
      if (props.ref && typeof props.ref === 'object') props.ref.current = node
      return node
    },
    useState(initial) { const at = si++; if (!(at in states)) states[at] = typeof initial === 'function' ? initial() : initial; return [states[at], value => { states[at] = typeof value === 'function' ? value(states[at]) : value }] },
    useRef(initial) { const at = ri++; if (!(at in refs)) refs[at] = { current: initial }; return refs[at] },
    useMemo(factory, deps) { const at = mi++; const old = memos[at]; const same = old && old.deps.length === deps.length && deps.every((v, i) => Object.is(v, old.deps[i])); if (!same) memos[at] = { value: factory(), deps: [...deps] }; return memos[at].value },
    useCallback(callback, deps) { return React.useMemo(() => callback, deps) },
    useEffect(run, deps) { const at = ei++; const old = effects[at]; const same = old && old.deps.length === deps.length && deps.every((v, i) => Object.is(v, old.deps[i])); if (!same) { old?.cleanup?.(); const cleanup = run(); effects[at] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined } } },
  }
  function resolve(value) {
    if (Array.isArray(value)) return flatten(value.map(resolve))
    if (value === null || value === undefined || value === false || typeof value !== 'object') return value
    if (!('kind' in value)) return value
    if (typeof value.kind === 'function') return resolve(value.kind(value.props))
    if (value.kind === Fragment) return { kind: 'Fragment', props: value.props, children: flatten(value.children.map(resolve)) }
    return { ...value, children: flatten(value.children.map(resolve)) }
  }
  return {
    React,
    render(component, props) { si = 0; ri = 0; ei = 0; mi = 0; return resolve(React.createElement(component, props)) },
    dispose() { for (const effect of effects.reverse()) effect?.cleanup?.() },
    reset() { for (const effect of effects.reverse()) effect?.cleanup?.(); states.length=0; refs.length=0; effects.length=0; memos.length=0 },
    stateAt(index) { return states[index] },
  }
}

function getPath(value, path) { let at = value; for (const key of path) { if (at === null || typeof at !== 'object' || !(key in at)) return undefined; at = at[key] } return at }
function setPath(value, path, next) { const root = structuredClone(value ?? {}); if (path.length === 0) return next; let at = root; for (const key of path.slice(0, -1)) at = at[key] = typeof at[key] === 'object' && at[key] !== null ? at[key] : {}; at[path.at(-1)] = next; return root }
function deletePath(value, path) { const root = structuredClone(value ?? {}); if (path.length === 0) return {}; let at = root; for (const key of path.slice(0, -1)) { if (!at[key] || typeof at[key] !== 'object') return root; at = at[key] } delete at[path.at(-1)]; return root }
function nodeAtPath(root, path) {
  if (path.length === 0) return root
  if (path.at(-1) === 'models' && Array.isArray(root?.modelDefaults)) return { type: 'array', meta: { default: root.modelDefaults } }
  if (path.at(-1) === 'api' && Array.isArray(root?.protocolChoices)) return { type: 'union', list: root.protocolChoices.map(value => ({ value })), meta: {} }
  if (path[0] === 'providers') return { type: 'object', meta: { default: [] } }
  return { type: 'object', meta: {} }
}

export function makeSettingsModelsBench() {
  const e = engine()
  const styles = [], appRoot = { inert: false }
  globalThis.document = {
    head: { appendChild(node) { styles.push(node); return node } },
    createElement(kind) { return { kind, attrs: {}, setAttribute(k, v) { this.attrs[k] = v }, textContent: '' } },
    querySelector(selector) { const m = selector.match(/data-plugin-css="([^"]+)"/); return m ? styles.find(row => row.attrs['data-plugin-css'] === m[1]) ?? null : null },
    getElementById(id) { return id === 'root' ? appRoot : null },
  }
  const primitive = name => props => name === 'Modal' && props.open === false ? null : e.React.createElement(name, props, name === 'Modal' ? props.title : null, props.children, name === 'Modal' ? props.footer : null)
  const primitives = Object.fromEntries(['Button','IconChevronDownOutline14','IconChevronRightOutline14','IconPlusOutline16','IconTrashOutline16','Modal'].map(name => [name, primitive(name)]))
  const schema = { getPath, setPath, deletePath, hasPath: (value, path) => getPath(value, path) !== undefined, rehydrateSchema: value => value, nodeAtPath, validateDraft: () => undefined }
  const bind = source => selector => selector(source.getSnapshot())
  const copy = { en: {
    nav: 'Models', title: 'Models', intro: 'Configure an API key or provider sign-in to use its models.',
    model: 'Model', models: 'Models', modelsCustomized: 'Customized model catalog', modelsInherited: 'Using the adapter defaults',
    modelsEmpty: 'No models', modelId: 'Model ID', modelName: 'Display name', addModel: 'Add model', resetModels: 'Restore defaults',
    keyInput: 'API key', keyPlaceholder: 'Enter your API key', keyPlaceholderNative: 'Enter an API key', keyStored: 'Configured', keyEnvLocked: 'Read-only',
    customized: 'Customized settings', baseUrl: 'Base URL', baseUrlDefault: 'Provider default', apply: 'Apply', applying: 'Applying…', cancel: 'Cancel',
    welcomeTitle: 'Internal Testing Notice', welcomeBody: 'First paragraph.\n\nSecond paragraph.', welcomeContinue: 'Continue', welcomeError: 'Could not save.',
    onboardingTitle: 'Add an API key to get started', onboardingDescription: 'Configure DeepSeek.', onboardingLater: 'Configure later', onboardingSave: 'Save and continue', onboardingSaving: 'Saving…',
    customTitle: 'Custom provider', customRoute: 'Provider ID', customRouteHint: 'Route hint', customRouteInvalid: 'Invalid route', customRouteTaken: 'Taken',
    customDisplayName: 'Display name', customApi: 'API protocol', create: 'Create provider', creating: 'Creating…', add: 'Add provider',
    credentialConfigured: 'API key configured', credentialMissing: 'API key missing', edit: 'Edit', editProvider: 'Edit {provider}', customTag: 'Custom',
    remove: 'Delete', removeProvider: 'Delete {provider}', savedProvider: 'Saved {provider}.', deleteTitle: 'Delete {provider}?', deleteDescription: 'Delete {provider} and keep credential.',
    deleteDescriptionWithCredential: 'Delete {provider} and stored key.', deleteConfirm: 'Delete {provider}', deleting: 'Deleting {provider}…', close: 'Close', provider: 'Provider', customAdd: 'Add a custom provider',
    readOnly: 'Read-only', loadFailed: 'Load failed', retry: 'Retry', advancedHint: 'Other fields', conflict: 'Conflict',
    modelAdvanced: 'Capacities', removeModel: 'Delete model', contextWindow: 'Context window', maxTokens: 'Max output tokens', modelContextWindow: 'Route context window', modelMaxTokens: 'Route max output tokens', fetchModels: 'Fetch available models', fetching: 'Asking the provider…', fetchNeedsBaseUrl: 'Enter base URL', fetchEmpty: 'No models', fetchTitle: 'Choose models to add', fetchDescription: 'Choose candidates.', fetchAdopt: 'Add selected',
  } }
  const t = (key, values = {}) => String(copy.en[key] ?? key).replace(/\{([^}]+)\}/g, (match, name) => name in values ? String(values[name]) : match)
  return { ...e, primitives, schema, bind, styles, appRoot, t }
}

const ok = value => ({ result: { ok: true, value } })
const fail = (message, code = 'failed') => ({ result: { ok: false, error: { message, code } } })
export function makeModelsApi() {
  const calls = []
  let credentialSetFailures = 0
  const namespace = {
    ns: 'llm-deepseek', schema: { type: 'object' }, value: { apiKeyEnv: 'DEEPSEEK_API_KEY', models: [{ id: 'deepseek-chat' }] },
    base: { models: [{ id: 'deepseek-chat' }] }, user: {}, applies: 'live', secrets: [], revision: 4,
  }
  const api = {
    llm: {
      providers(request) { calls.push(['providers', request]); return Promise.resolve(ok({ providers: [{ provider: 'deepseek-official', displayName: 'DeepSeek', settingsNs: 'llm-deepseek', settingsPath: [], authentication: 'api-key', active: true }] })) },
      discoverModels(request) { calls.push(['discoverModels', request]); return Promise.resolve(ok({ models: [{ id: 'deepseek-chat', name: 'Chat', contextWindow: 128000, maxTokens: 8192, ignored: 'wire-only' }] })) },
    },
    settings: {
      describe(request) { calls.push(['settings.describe', request]); return Promise.resolve(ok({ writable: true, hasDocument: true, namespaces: [namespace] })) },
      mutate(request) { calls.push(['settings.mutate', structuredClone(request)]); namespace.user = request.ops.reduce((value, op) => op.op === 'set' ? setPath(value, op.path, op.value) : deletePath(value, op.path), namespace.user); namespace.value = { ...namespace.value, ...namespace.user }; namespace.revision++; return Promise.resolve(ok(structuredClone(namespace))) },
    },
    credentials: {
      describe(request) { calls.push(['credentials.describe', request]); return Promise.resolve(ok({ credentials: Object.fromEntries(request.refs.map(ref => [ref, { configured: false, writable: true }])) })) },
      set(request) { calls.push(['credentials.set', { ...request }]); if (credentialSetFailures > 0) { credentialSetFailures--; return Promise.resolve(fail('credential refused')) } return Promise.resolve(ok({ stored: true })) },
      unset(request) { calls.push(['credentials.unset', { ...request }]); return Promise.resolve(ok({ removed: true })) },
    },
  }
  return { api, calls, namespace, fail, failCredentialSets(count) { credentialSetFailures = count } }
}
export function failCredentialSets(value, count) { value.failCredentialSets(count) }
export function deferCredentialSet(value) {
  let release
  value.api.credentials.set = request => {
    value.calls.push(['credentials.set', { ...request }])
    return new Promise(resolve => { release = () => resolve(ok({ stored: true })) })
  }
  return { release() { release?.() } }
}

export function makeWelcomeApi(version) {
  const calls = []
  const view = { ns: 'ui-onboarding', schema: {}, value: version === undefined ? {} : { welcomeNoticeVersion: version }, base: {}, user: {}, applies: 'live', secrets: [], revision: 1 }
  const api = { settings: {
    describe(request) { calls.push(['describe', request]); return Promise.resolve(ok({ writable: true, namespaces: [view] })) },
    mutate(request) { calls.push(['mutate', structuredClone(request)]); view.value = setPath(view.value, request.ops[0].path, request.ops[0].value); return Promise.resolve(ok(view)) },
  } }
  return { api, calls, view }
}

export function makeSectionProps(bench) {
  const calls = []
  const pi = { ns: 'llm-pi-ai', schema: { type: 'object' }, value: { providers: { acme: { apiKeyEnv: 'ACME_API_KEY' } } }, base: { providers: {} }, user: { providers: { acme: { apiKeyEnv: 'ACME_API_KEY' } } }, applies: 'live', secrets: [], revision: 8 }
  const rows = [
    { entry: { provider: 'acme', displayName: 'Acme', settingsNs: 'llm-pi-ai', settingsPath: ['providers','acme'], authentication: 'api-key', active: true, declared: true }, configured: true, removable: true, apiKeyEnv: 'ACME_API_KEY', credential: { configured: true, writable: true } },
    { entry: { provider: 'codex', displayName: 'Codex', settingsNs: 'llm-pi-ai', settingsPath: ['providers','codex'], authentication: 'codex-oauth', active: false }, configured: false, removable: false },
  ]
  const state = { status: 'ready', error: null, credentialError: null, writable: true, rows, namespaces: new Map([['llm-pi-ai', pi]]) }
  const controller = { calls, store: { getSnapshot: () => state }, load() { calls.push(['load']); return Promise.resolve() } }
  const api = {
    credentials: { describe: request => Promise.resolve(ok({ credentials: Object.fromEntries(request.refs.map(ref => [ref,{ configured: true, writable: true }])) })), unset(request) { calls.push(['credentials.unset', request]); return Promise.resolve(ok({ removed: true })) }, set(request) { calls.push(['credentials.set',request]); return Promise.resolve(ok({ stored: true })) } },
    settings: { mutate(request) { calls.push(['settings.mutate', structuredClone(request)]); return Promise.resolve(ok(pi)) } },
    llm: { discoverModels: request => Promise.resolve(ok({ models: [] })) },
  }
  return { controller, api, t: bench.t, useSnapshot: selector => selector(state), calls, state, pi }
}

function walk(root, out = []) { if (!root || typeof root !== 'object') return out; if (Array.isArray(root)) { root.forEach(v => walk(v, out)); return out } if ('kind' in root) out.push(root); (root.children ?? []).forEach(v => walk(v, out)); return out }
export function smRender(bench, component, props) { try { return bench.render(component, props) } catch (error) { throw error instanceof Error ? error : new Error(`render threw ${String(error)}`) } }
export function smFind(root, key, value) { return walk(root).find(node => value === undefined ? key in node.props : Object.is(node.props[key], value)) }
export function smFindKind(root, kind) { return walk(root).find(node => node.kind === kind) }
export function smFindText(root, text) { return walk(root).find(node => { const parts=[]; const visit=v=>{ if(typeof v==='string'||typeof v==='number')parts.push(String(v)); else if(Array.isArray(v))v.forEach(visit); else if(v&&typeof v==='object')(v.children??[]).forEach(visit)}; visit(node); return parts.join('') === text }) }
export function smText(root) { const parts=[]; const visit=v=>{ if(typeof v==='string'||typeof v==='number')parts.push(String(v)); else if(Array.isArray(v))v.forEach(visit); else if(v&&typeof v==='object')(v.children??[]).forEach(visit)}; visit(root); return parts.join('') }
export function smClick(node) { return node.props.onClick?.({ target: node, preventDefault() {}, stopPropagation() {} }) }
export function smChange(node, value) { return node.props.onChange?.({ target: { value } }) }
export function smBlur(node, value) { return node.props.onBlur?.({ target: { value } }) }
export function smProp(node, key) { return node?.props?.[key] }
export function smProps(root, key) { return walk(root).map(node => node.props?.[key]).filter(value => value !== undefined) }
export function smProperty(value, key) { return value?.[key] }
export function smCalls(value) { return value.calls }
export function smState(bench, index) { return bench.stateAt(index) }
export function smTick() { return new Promise(resolve => setTimeout(resolve, 0)) }
export function smReset(bench) { bench.reset() }

export function makeApplyBench(bench, api) {
  const entries = [], localeCalls = [], effects = [], remoteHandlers = new Map(), ctxHandlers = new Map(), disposeCalls = []
  const declared = new Set(['settings.section','settings.onboarding'])
  const own = value => { if (typeof value === 'function') effects.push(value); return value }
  const slots = {
    inject(name, install) { if (!declared.has(name)) throw new Error(`undeclared ${name}`); const result = install(); return own(typeof result === 'function' ? result : () => {}) },
    register(options, component) { const row={options,component}; entries.push(row); return () => entries.splice(entries.indexOf(row),1) },
  }
  const locale = { register(ns, dictionaries) { localeCalls.push([ns,dictionaries]); return () => {} }, bind(ns) { return (key, values = {}) => { const dict=localeCalls.find(row=>row[0]===ns)?.[1]?.en ?? {}; return String(dict[key] ?? key).replace(/\{([^}]+)\}/g,(match,name)=>name in values ? String(values[name]) : match) } } }
  const remote = { $on(name, fn) { remoteHandlers.set(name, fn); return () => { disposeCalls.push(['remote', name]); remoteHandlers.delete(name) } } }
  const connection = { api, isLoopback: true }
  const ctx = { slots, locale, remote, get(name) { return name === 'connection' ? connection : undefined }, effect(setup) { return own(setup()) }, on(name, fn) { ctxHandlers.set(name, fn); return () => { disposeCalls.push(['ctx', name]); ctxHandlers.delete(name) } } }
  return { ctx, entries, localeCalls, effects, remoteHandlers, ctxHandlers, disposeCalls }
}
export function smEntries(bench, name) { return bench.entries.filter(row => row.options.name === name) }
export function smEmit(map, name, ...args) { return map.get(name)?.(...args) }
export function smDisposeApply(bench) { for (const effect of bench.effects) effect() }
"#)]
extern "C" {
    fn makeSettingsModelsBench() -> JsValue;
    fn makeModelsApi() -> JsValue;
    fn failCredentialSets(value: &JsValue, count: u32);
    fn deferCredentialSet(value: &JsValue) -> JsValue;
    fn makeWelcomeApi(version: &JsValue) -> JsValue;
    fn makeSectionProps(bench: &JsValue) -> JsValue;
    fn makeApplyBench(bench: &JsValue, api: &JsValue) -> JsValue;
    fn smRender(bench: &JsValue, component: &JsValue, props: &JsValue) -> JsValue;
    fn smFind(root: &JsValue, key: &str, value: &JsValue) -> JsValue;
    fn smFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn smFindText(root: &JsValue, text: &str) -> JsValue;
    fn smText(root: &JsValue) -> String;
    fn smClick(node: &JsValue) -> JsValue;
    fn smChange(node: &JsValue, value: &str) -> JsValue;
    fn smBlur(node: &JsValue, value: &str) -> JsValue;
    fn smProp(node: &JsValue, key: &str) -> JsValue;
    fn smProps(root: &JsValue, key: &str) -> Array;
    fn smProperty(value: &JsValue, key: &str) -> JsValue;
    fn smCalls(value: &JsValue) -> Array;
    fn smState(bench: &JsValue, index: u32) -> JsValue;
    fn smTick() -> Promise;
    fn smReset(bench: &JsValue);
    fn smEntries(bench: &JsValue, name: &str) -> Array;
    fn smDisposeApply(bench: &JsValue);
    #[wasm_bindgen(variadic)]
    fn smEmit(map: &JsValue, arguments: &Array) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

fn configure() -> JsValue {
    let bench = makeSettingsModelsBench();
    configure_client_ui_settings_models(
        property(&bench, "React"),
        property(&bench, "primitives"),
        property(&bench, "schema"),
        property(&bench, "bind").dyn_into().unwrap(),
    )
    .unwrap();
    bench
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // One controller graph pins both observable state faces and persistence modes.
async fn models_and_welcome_controllers_preserve_rpc_join_and_snapshot_contracts() {
    let models = makeModelsApi();
    Reflect::delete_property(
        &Object::from(property(&models, "namespace")),
        &JsValue::from_str("base"),
    )
    .unwrap();
    Reflect::delete_property(
        &Object::from(property(&models, "namespace")),
        &JsValue::from_str("user"),
    )
    .unwrap();
    let controller = create_models_settings_controller(property(&models, "api")).unwrap();
    assert!(!Reflect::has(&controller, &JsValue::from_str("readiness")).unwrap());
    let store = property(&controller, "store");
    let before = property(&store, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&store)
        .unwrap();
    assert_eq!(
        property(&before, "status").as_string().as_deref(),
        Some("idle")
    );
    refresh_models_if_loaded(controller.clone()).unwrap();
    assert_eq!(smCalls(&models).length(), 0);
    JsFuture::from(Promise::resolve(
        &property(&controller, "load")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&controller)
            .unwrap(),
    ))
    .await
    .unwrap();
    let after = property(&store, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&store)
        .unwrap();
    assert_eq!(
        property(&after, "status").as_string().as_deref(),
        Some("ready")
    );
    assert_eq!(
        property(&after, "rows")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );
    let namespace = property(&after, "namespaces")
        .dyn_into::<Map>()
        .unwrap()
        .get(&JsValue::from_str("llm-deepseek"));
    assert!(!Reflect::has(&namespace, &JsValue::from_str("base")).unwrap());
    assert!(!Reflect::has(&namespace, &JsValue::from_str("user")).unwrap());
    assert!(Object::is(
        &after,
        &property(&store, "getSnapshot")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&store)
            .unwrap()
    ));
    assert_eq!(smCalls(&models).length(), 3);

    let welcome = makeWelcomeApi(&JsValue::UNDEFINED);
    let notice =
        create_welcome_notice_controller(property(&welcome, "api"), "host".to_owned()).unwrap();
    JsFuture::from(Promise::resolve(
        &property(&notice, "load")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&notice)
            .unwrap(),
    ))
    .await
    .unwrap();
    let accepted = JsFuture::from(Promise::resolve(
        &property(&notice, "acknowledge")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&notice)
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(accepted, JsValue::TRUE);
    assert_eq!(smCalls(&welcome).length(), 2);
    let memory =
        create_welcome_notice_controller(property(&welcome, "api"), "memory".to_owned()).unwrap();
    JsFuture::from(Promise::resolve(
        &property(&memory, "load")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&memory)
            .unwrap(),
    ))
    .await
    .unwrap();
    JsFuture::from(Promise::resolve(
        &property(&memory, "acknowledge")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&memory)
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(smCalls(&welcome).length(), 2);
}

#[wasm_bindgen_test]
fn apply_registers_copy_section_and_ordered_onboarding_entries() {
    let bench = configure();
    let api = makeModelsApi();
    let apply = makeApplyBench(&bench, &property(&api, "api"));
    apply_client_ui_settings_models(property(&apply, "ctx")).unwrap();
    let inject = settings_models_inject();
    assert_eq!(inject.length(), 4);
    assert_eq!(smEntries(&apply, "settings.section").length(), 1);
    let onboarding = smEntries(&apply, "settings.onboarding");
    assert_eq!(onboarding.length(), 2);
    assert_eq!(
        property(&property(&onboarding.get(0), "options"), "id")
            .as_string()
            .as_deref(),
        Some("welcome-notice")
    );
    assert_eq!(
        property(&property(&onboarding.get(1), "options"), "id")
            .as_string()
            .as_deref(),
        Some("deepseek-official")
    );
    assert_eq!(
        property(&apply, "localeCalls")
            .dyn_into::<Array>()
            .unwrap()
            .length(),
        1
    );
    smDisposeApply(&apply);
    let disposed = property(&apply, "disposeCalls")
        .dyn_into::<Array>()
        .unwrap();
    let names = (0..disposed.length())
        .map(|index| property(&disposed.get(index), "1").as_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "settings/document-updated",
            "credentials/updated",
            "llm/adapters-updated",
            "connection/reset"
        ]
    );
}

#[wasm_bindgen_test(async)]
async fn compiled_welcome_and_onboarding_modal_follow_acknowledgement_and_inert_lifecycle() {
    let bench = configure();
    let welcome_api = makeWelcomeApi(&JsValue::UNDEFINED);
    let controller =
        create_welcome_notice_controller(property(&welcome_api, "api"), "host".to_owned()).unwrap();
    JsFuture::from(Promise::resolve(
        &property(&controller, "load")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&controller)
            .unwrap(),
    ))
    .await
    .unwrap();
    let store = property(&controller, "store");
    let hook_store = store.clone();
    let use_welcome = Closure::wrap(Box::new(move |selector: Function| {
        let snapshot = property(&hook_store, "getSnapshot")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&hook_store)
            .unwrap();
        selector.call1(&JsValue::UNDEFINED, &snapshot).unwrap()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    let completes = Array::new();
    let complete_calls = completes.clone();
    let complete = Closure::wrap(Box::new(move || {
        complete_calls.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let props = object(&[
        ("complete", complete.into_js_value()),
        ("controller", controller.clone()),
        ("useWelcome", use_welcome.into_js_value()),
        ("t", property(&bench, "t")),
    ]);
    let tree = smRender(&bench, &welcome_notice_component().unwrap(), &props);
    assert!(smText(&tree).contains("Internal Testing Notice"));
    assert_eq!(
        property(&property(&bench, "appRoot"), "inert"),
        JsValue::TRUE
    );
    smClick(&smFindKind(&tree, "Button"));
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    assert_eq!(completes.length(), 1);
}

#[wasm_bindgen_test]
fn compiled_model_rows_and_provider_editor_emit_curated_controls() {
    let bench = configure();
    let styles = property(&bench, "styles").dyn_into::<Array>().unwrap();
    let models_css = property(&styles.get(0), "textContent").as_string().unwrap();
    assert!(models_css.contains("http://www.w3.org/2000/svg"));
    assert!(!models_css.contains("seekdeep-settings-models-org"));
    let changes = Array::new();
    let change_calls = changes.clone();
    let change = Closure::wrap(Box::new(move |models: JsValue| {
        change_calls.push(&models);
    }) as Box<dyn FnMut(JsValue)>);
    let models = js_sys::JSON::parse(r#"[{"id":"a","contextWindow":256000},{"id":"b"}]"#).unwrap();
    let source_models = models.clone().dyn_into::<Array>().unwrap();
    let props = object(&[
        ("models", models),
        ("overridden", JsValue::TRUE),
        ("defaultContextWindow", JsValue::from_f64(131_072.0)),
        ("defaultMaxTokens", JsValue::from_f64(8192.0)),
        ("t", property(&bench, "t")),
        ("disabled", JsValue::FALSE),
        ("onChange", change.into_js_value()),
        ("onReset", Function::new_no_args("").into()),
    ]);
    let tree = smRender(&bench, &deepseek_models_editor_component().unwrap(), &props);
    assert!(smText(&tree).contains("Models"));
    assert!(
        !smFind(
            &tree,
            "className",
            &JsValue::from_str("seekdeep-settings-models-modelList")
        )
        .is_undefined()
    );
    let add = smFind(
        &tree,
        "className",
        &JsValue::from_str("seekdeep-settings-models-addModelButton"),
    );
    smClick(&add);
    assert_eq!(changes.length(), 1);
    let added = changes.get(0).dyn_into::<Array>().unwrap();
    assert!(!Object::is(&added.get(0), &source_models.get(0)));
    assert!(!smFindKind(&tree, "IconPlusOutline16").is_undefined());

    smReset(&bench);
    let api = makeModelsApi();
    let namespace = property(&api, "namespace");
    let closes = Array::new();
    let close_calls = closes.clone();
    let close = Closure::wrap(Box::new(move |changed: bool| {
        close_calls.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let editor_props = object(&[
        ("provider", JsValue::from_str("deepseek-official")),
        ("displayName", JsValue::from_str("DeepSeek")),
        ("namespace", namespace),
        ("settingsPath", Array::new().into()),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", close.into_js_value()),
    ]);
    let editor = smRender(&bench, &provider_editor_component().unwrap(), &editor_props);
    assert!(smText(&editor).contains("DeepSeek"));
    assert!(smText(&editor).contains("API key"));
    assert!(smText(&editor).contains("Customized settings"));
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // One rendered editor pins inherited rows and its complete discovery probe.
async fn provider_editor_inherits_schema_models_and_sends_the_live_trimmed_probe() {
    let bench = configure();
    let api = makeModelsApi();
    let namespace = js_sys::JSON::parse(
        r#"{
          "ns":"llm-pi-ai",
          "schema":{"modelDefaults":[42,{"id":"fallback-model"}]},
          "value":{"providers":{"acme":{"baseURL":"https://fallback.test/v1","api":"openai-responses"}}},
          "base":{"providers":{"acme":{}}},
          "user":{"providers":{"acme":{"baseURL":"\uFEFF \uFEFF","api":"\uFEFF"}}},
          "applies":"live",
          "secrets":[],
          "revision":9
        }"#,
    )
    .unwrap();
    let props = object(&[
        ("provider", JsValue::from_str("acme")),
        ("displayName", JsValue::from_str("Acme")),
        ("declared", JsValue::TRUE),
        ("authentication", JsValue::from_str("api-key")),
        ("namespace", namespace),
        (
            "settingsPath",
            Array::of2(&JsValue::from_str("providers"), &JsValue::from_str("acme")).into(),
        ),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", Function::new_no_args("").into()),
    ]);
    let editor = provider_editor_component().unwrap();
    let initial = smRender(&bench, &editor, &props);
    assert_eq!(
        smProp(
            &smFind(&initial, "aria-label", &JsValue::from_str("API key")),
            "aria-invalid"
        ),
        JsValue::FALSE
    );
    assert_eq!(
        smProp(
            &smFind(&initial, "aria-label", &JsValue::from_str("Model ID 1")),
            "value"
        )
        .as_string()
        .as_deref(),
        Some("")
    );
    assert_eq!(
        smProp(
            &smFind(&initial, "aria-label", &JsValue::from_str("Model ID 2")),
            "value"
        )
        .as_string()
        .as_deref(),
        Some("fallback-model")
    );

    smChange(
        &smFind(&initial, "aria-label", &JsValue::from_str("API key")),
        "\u{feff}  sk-live  \u{feff}",
    );
    let ready = smRender(&bench, &editor, &props);
    let fetch = smFindText(&ready, "Fetch available models");
    assert_eq!(smProp(&fetch, "disabled"), JsValue::FALSE);
    smClick(&fetch);
    JsFuture::from(smTick()).await.unwrap();
    let calls = smCalls(&api);
    let discovery = (0..calls.length())
        .map(|index| calls.get(index))
        .find(|call| property(call, "0").as_string().as_deref() == Some("discoverModels"))
        .unwrap();
    let request = property(&discovery, "1");
    assert_eq!(
        property(&request, "baseURL").as_string().as_deref(),
        Some("https://fallback.test/v1")
    );
    assert_eq!(
        property(&request, "api").as_string().as_deref(),
        Some("openai-responses")
    );
    assert_eq!(
        property(&request, "apiKey").as_string().as_deref(),
        Some("sk-live")
    );

    smChange(
        &smFind(&ready, "aria-label", &JsValue::from_str("API key")),
        "NAME=value",
    );
    let blocked = smRender(&bench, &editor, &props);
    let fetch = smFindText(&blocked, "Fetch available models");
    assert_eq!(smProp(&fetch, "disabled"), JsValue::TRUE);
    assert_eq!(
        smProp(&fetch, "title").as_string().as_deref(),
        Some("keyIllegalCharacters")
    );

    smReset(&bench);
    let malformed_user =
        js_sys::JSON::parse(r#"{"providers":{"acme":{"models":{"not":"an array"}}}}"#).unwrap();
    Reflect::set(
        &property(&props, "namespace"),
        &JsValue::from_str("user"),
        &malformed_user,
    )
    .unwrap();
    let malformed = smRender(&bench, &editor, &props);
    assert!(smText(&malformed).contains("No models"));
    assert!(smFind(&malformed, "aria-label", &JsValue::from_str("Model ID 1")).is_undefined());

    smReset(&bench);
    let list_models = js_sys::JSON::parse(r#"[{"id":"first"},{"id":"second"}]"#)
        .unwrap()
        .dyn_into::<Array>()
        .unwrap();
    let list_changes = Array::new();
    let recorded = list_changes.clone();
    let change = Closure::wrap(Box::new(move |models: JsValue| {
        recorded.push(&models);
    }) as Box<dyn FnMut(JsValue)>);
    let api = makeModelsApi();
    let list_props = object(&[
        ("models", list_models.clone().into()),
        ("onChange", change.into_js_value()),
        (
            "probe",
            object(&[
                ("settingsNs", JsValue::from_str("llm-pi-ai")),
                ("provider", JsValue::from_str("existing")),
            ]),
        ),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("disabled", JsValue::FALSE),
    ]);
    let list = smRender(&bench, &model_list_editor_component().unwrap(), &list_props);
    assert!(
        smFind(
            &list,
            "className",
            &JsValue::from_str("seekdeep-settings-models-modelList")
        )
        .is_undefined()
    );
    let add = smFind(
        &list,
        "className",
        &JsValue::from_str("seekdeep-settings-models-addModelButton"),
    );
    assert!(smFindKind(&add, "IconPlusOutline16").is_undefined());
    smChange(
        &smFind(&list, "aria-label", &JsValue::from_str("Model ID 1")),
        "changed",
    );
    let edited = list_changes.get(0).dyn_into::<Array>().unwrap();
    assert!(Object::is(&edited.get(1), &list_models.get(1)));
    assert!(!Object::is(&edited.get(0), &list_models.get(0)));
    smClick(&smFind(
        &list,
        "aria-label",
        &JsValue::from_str("Delete model 1"),
    ));
    let remaining = list_changes.get(1).dyn_into::<Array>().unwrap();
    assert!(Object::is(&remaining.get(0), &list_models.get(1)));
}

#[wasm_bindgen_test(async)]
async fn provider_native_empty_profile_is_materialized_without_a_credential_write() {
    let bench = configure();
    let api = makeModelsApi();
    let namespace = js_sys::JSON::parse(
        r#"{
          "ns":"llm-pi-ai",
          "schema":{},
          "value":{"providers":{}},
          "base":{"providers":{}},
          "user":{"providers":{}},
          "applies":"live",
          "secrets":[],
          "revision":4
        }"#,
    )
    .unwrap();
    let closes = Array::new();
    let close_calls = closes.clone();
    let close = Closure::wrap(Box::new(move |changed: bool| {
        close_calls.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let props = object(&[
        ("provider", JsValue::from_str("bedrock")),
        ("displayName", JsValue::from_str("Bedrock")),
        ("authentication", JsValue::from_str("provider-native")),
        ("namespace", namespace),
        (
            "settingsPath",
            Array::of2(
                &JsValue::from_str("providers"),
                &JsValue::from_str("bedrock"),
            )
            .into(),
        ),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", close.into_js_value()),
    ]);
    let tree = smRender(&bench, &provider_editor_component().unwrap(), &props);
    let submit = smFind(
        &tree,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    );
    assert_eq!(smProp(&submit, "disabled"), JsValue::FALSE);
    smClick(&submit);
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    let calls = smCalls(&api);
    let mutation = (0..calls.length())
        .map(|index| calls.get(index))
        .find(|call| property(call, "0").as_string().as_deref() == Some("settings.mutate"))
        .unwrap();
    let operation = property(&property(&property(&mutation, "1"), "ops"), "0");
    assert_eq!(
        property(&operation, "op").as_string().as_deref(),
        Some("set")
    );
    assert_eq!(
        Object::keys(&Object::from(property(&operation, "value"))).length(),
        0
    );
    assert!(!(0..calls.length()).any(|index| {
        property(&calls.get(index), "0").as_string().as_deref() == Some("credentials.set")
    }));
    assert_eq!(closes.get(0), JsValue::TRUE);
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // One ledger proves both source write-order variants.
async fn provider_and_custom_cards_write_trimmed_credentials_after_settings() {
    let bench = configure();
    let api = makeModelsApi();
    let namespace = property(&api, "namespace");
    let closes = Array::new();
    let close_calls = closes.clone();
    let close = Closure::wrap(Box::new(move |changed: bool| {
        close_calls.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let props = object(&[
        ("provider", JsValue::from_str("deepseek-official")),
        ("displayName", JsValue::from_str("DeepSeek")),
        ("namespace", namespace),
        ("settingsPath", Array::new().into()),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("credentialOnly", JsValue::TRUE),
        ("credentialRequired", JsValue::TRUE),
        ("onClose", close.into_js_value()),
    ]);
    let editor = provider_editor_component().unwrap();
    let first = smRender(&bench, &editor, &props);
    let key = smFind(&first, "aria-label", &JsValue::from_str("API key"));
    smChange(&key, "  sk-live  ");
    let ready = smRender(&bench, &editor, &props);
    let submit = smFind(
        &ready,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    );
    assert_eq!(smProp(&submit, "disabled"), JsValue::FALSE);
    smClick(&submit);
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    let calls = smCalls(&api);
    let set_call = (0..calls.length())
        .map(|index| calls.get(index))
        .find(|call| property(call, "0").as_string().as_deref() == Some("credentials.set"))
        .unwrap();
    assert_eq!(
        property(&property(&set_call, "1"), "value")
            .as_string()
            .as_deref(),
        Some("sk-live")
    );
    assert!(!(0..calls.length()).any(|index| {
        property(&calls.get(index), "0").as_string().as_deref() == Some("settings.mutate")
    }));
    assert_eq!(closes.get(0), JsValue::TRUE);

    smReset(&bench);
    let custom_api = makeModelsApi();
    let custom_closes = Array::new();
    let recorded = custom_closes.clone();
    let custom_close = Closure::wrap(Box::new(move |changed: bool| {
        recorded.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let protocols = Array::of1(&JsValue::from_str("openai-completions"));
    let custom_props = object(&[
        ("taken", Array::new().into()),
        ("protocols", protocols.into()),
        ("revision", JsValue::from_f64(4.0)),
        ("api", property(&custom_api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", custom_close.into_js_value()),
    ]);
    let custom = custom_provider_card_component().unwrap();
    let form = smRender(&bench, &custom, &custom_props);
    smChange(
        &smFind(&form, "aria-label", &JsValue::from_str("Provider ID")),
        "acme-gateway",
    );
    smChange(
        &smFind(&form, "aria-label", &JsValue::from_str("Base URL")),
        "https://gateway.example/v1",
    );
    smChange(
        &smFind(&form, "aria-label", &JsValue::from_str("API key")),
        " key-1 ",
    );
    let with_fields = smRender(&bench, &custom, &custom_props);
    smClick(&smFind(
        &with_fields,
        "className",
        &JsValue::from_str("seekdeep-settings-models-addModelButton"),
    ));
    let with_row = smRender(&bench, &custom, &custom_props);
    smChange(
        &smFind(&with_row, "aria-label", &JsValue::from_str("Model ID 1")),
        "acme-chat",
    );
    let complete = smRender(&bench, &custom, &custom_props);
    let create = smFind(
        &complete,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    );
    assert_eq!(smProp(&create, "disabled"), JsValue::FALSE);
    smClick(&create);
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    let calls = smCalls(&custom_api);
    let names = (0..calls.length())
        .map(|index| property(&calls.get(index), "0").as_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["settings.mutate", "credentials.set"]);
    let mutate = property(&calls.get(0), "1");
    let profile = property(&property(&property(&mutate, "ops"), "0"), "value");
    assert_eq!(
        property(&profile, "apiKeyEnv").as_string().as_deref(),
        Some("ACME_GATEWAY_API_KEY")
    );
    let draft_models = smState(&bench, 6).dyn_into::<Array>().unwrap();
    let written_models = property(&profile, "models").dyn_into::<Array>().unwrap();
    assert!(!Object::is(&draft_models.get(0), &written_models.get(0)));
    assert_eq!(custom_closes.get(0), JsValue::TRUE);
}

#[wasm_bindgen_test(async)]
async fn models_section_adopts_dormant_routes_and_deletes_managed_profiles_in_order() {
    let bench = configure();
    let props = makeSectionProps(&bench);
    let section = models_section_component().unwrap();
    let first = smRender(&bench, &section, &props);
    assert!(smText(&first).contains("Acme"));
    let remove = smFind(
        &first,
        "aria-label",
        &JsValue::from_str("Delete Acme (acme)"),
    );
    assert!(
        !remove.is_undefined(),
        "aria labels: {:?}",
        smProps(&first, "aria-label")
    );
    smClick(&remove);
    let confirming = smRender(&bench, &section, &props);
    let modal = smFindKind(&confirming, "Modal");
    assert_eq!(smProp(&modal, "open"), JsValue::TRUE);
    smClick(&smFindText(&confirming, "Delete Acme (acme)"));
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    let calls = property(&props, "calls").dyn_into::<Array>().unwrap();
    let names = (0..calls.length())
        .map(|index| property(&calls.get(index), "0").as_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["credentials.unset", "settings.mutate", "load"]);
    let mutation = property(&calls.get(1), "1");
    assert_eq!(
        property(&property(&property(&mutation, "ops"), "0"), "op")
            .as_string()
            .as_deref(),
        Some("unset")
    );

    smReset(&bench);
    let add_props = makeSectionProps(&bench);
    let page = smRender(&bench, &section, &add_props);
    let add = smFindText(&page, "Add provider");
    assert_eq!(smProp(&add, "disabled"), JsValue::FALSE);
    smClick(&add);
    let add_card = smRender(&bench, &section, &add_props);
    assert!(!smFind(&add_card, "aria-label", &JsValue::from_str("Provider")).is_undefined());
    assert!(smText(&add_card).contains("Codex"));
}

#[wasm_bindgen_test(async)]
async fn models_section_announces_refreshed_saves_and_requires_schema_protocols_to_declare() {
    let bench = configure();
    let props = makeSectionProps(&bench);
    let section = models_section_component().unwrap();
    let first = smRender(&bench, &section, &props);
    let custom = smFindText(&first, "Add a custom provider");
    assert_eq!(smProp(&custom, "disabled"), JsValue::TRUE);

    Reflect::set(
        &property(&property(&props, "pi"), "schema"),
        &JsValue::from_str("protocolChoices"),
        &Array::of1(&JsValue::from_str("openai-responses")),
    )
    .unwrap();
    let enabled = smRender(&bench, &section, &props);
    assert_eq!(
        smProp(&smFindText(&enabled, "Add a custom provider"), "disabled"),
        JsValue::FALSE
    );

    smClick(&smFind(
        &enabled,
        "aria-label",
        &JsValue::from_str("Edit Acme (acme)"),
    ));
    let editor = smRender(&bench, &section, &props);
    smClick(&smFind(
        &editor,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    ));
    Reflect::set(
        &property(
            &property(
                &property(&props, "state").dyn_into::<Object>().unwrap(),
                "rows",
            )
            .dyn_into::<Array>()
            .unwrap()
            .get(0),
            "entry",
        ),
        &JsValue::from_str("displayName"),
        &JsValue::from_str("Renamed Acme"),
    )
    .unwrap();
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    let saved = smRender(&bench, &section, &props);
    assert!(smText(&saved).contains("Saved Renamed Acme (acme)."));
    let notice = smFind(&saved, "role", &JsValue::from_str("status"));
    assert_eq!(
        smProp(&notice, "aria-live").as_string().as_deref(),
        Some("polite")
    );

    smClick(&smFindText(&saved, "Add provider"));
    let adding = smRender(&bench, &section, &props);
    assert!(!smText(&adding).contains("Saved Renamed Acme (acme)."));
    let state_rows = property(&property(&props, "state"), "rows")
        .dyn_into::<Array>()
        .unwrap();
    Reflect::set(
        &Object::from(state_rows.get(1)),
        &JsValue::from_str("configured"),
        &JsValue::TRUE,
    )
    .unwrap();
    let settled_elsewhere = smRender(&bench, &section, &props);
    assert!(
        !smFind(
            &settled_elsewhere,
            "aria-label",
            &JsValue::from_str("Provider")
        )
        .is_undefined()
    );
    assert!(smText(&settled_elsewhere).contains("Codex"));
}

#[wasm_bindgen_test]
fn dismissing_a_setup_card_preserves_an_independent_open_editor() {
    let bench = configure();
    let props = makeSectionProps(&bench);
    let state = property(&props, "state");
    let rows = js_sys::JSON::parse(
        r#"[
          {
            "entry":{"provider":"deepseek-official","displayName":"DeepSeek","settingsNs":"llm-deepseek","settingsPath":[],"authentication":"api-key","active":true},
            "configured":true,"removable":false,"apiKeyEnv":"DEEPSEEK_API_KEY",
            "credential":{"configured":false,"writable":true}
          },
          {
            "entry":{"provider":"other","displayName":"Other","settingsNs":"llm-pi-ai","settingsPath":["providers","other"],"authentication":"api-key","active":false},
            "configured":true,"removable":false
          }
        ]"#,
    )
    .unwrap();
    Reflect::set(&state, &JsValue::from_str("rows"), &rows).unwrap();
    let namespaces = property(&state, "namespaces").dyn_into::<Map>().unwrap();
    let deepseek = js_sys::JSON::parse(
        r#"{"ns":"llm-deepseek","schema":{},"value":{"apiKeyEnv":"DEEPSEEK_API_KEY"},"base":{},"user":{},"applies":"live","secrets":[],"revision":1}"#,
    )
    .unwrap();
    let other = js_sys::JSON::parse(
        r#"{"ns":"llm-pi-ai","schema":{},"value":{"providers":{"other":{}}},"base":{"providers":{}},"user":{"providers":{"other":{}}},"applies":"live","secrets":[],"revision":1}"#,
    )
    .unwrap();
    namespaces.set(&JsValue::from_str("llm-deepseek"), &deepseek);
    namespaces.set(&JsValue::from_str("llm-pi-ai"), &other);

    let section = models_section_component().unwrap();
    let first = smRender(&bench, &section, &props);
    let setup = smFind(
        &first,
        "className",
        &JsValue::from_str("seekdeep-settings-models-setupCard"),
    );
    assert!(!setup.is_undefined());
    assert!(!smText(&setup).contains("Edit"));
    let edit = smFind(
        &first,
        "aria-label",
        &JsValue::from_str("Edit Other (other)"),
    );
    assert!(
        !edit.is_undefined(),
        "labels: {:?}",
        smProps(&first, "aria-label")
    );
    smClick(&edit);
    let both = smRender(&bench, &section, &props);
    assert!(smText(&both).contains("Customized settings"));
    let cancel = smFindText(&both, "Cancel");
    assert!(!cancel.is_undefined(), "tree: {}", smText(&both));
    smClick(&cancel);
    assert_eq!(
        property(&smState(&bench, 0), "provider")
            .as_string()
            .as_deref(),
        Some("other")
    );
    assert!(
        smState(&bench, 7)
            .dyn_into::<js_sys::Set>()
            .unwrap()
            .has(&JsValue::from_str("deepseek-official"))
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One hook instance proves buffer reindex plus discovery adoption.
fn model_editors_preserve_invalid_text_and_reindex_state() {
    let bench = configure();
    let changes = Array::new();
    let recorded = changes.clone();
    let change = Closure::wrap(Box::new(move |models: JsValue| {
        recorded.push(&models);
    }) as Box<dyn FnMut(JsValue)>);
    let props = object(&[
        (
            "models",
            js_sys::JSON::parse(r#"[{"id":"a"},{"id":"b"}]"#).unwrap(),
        ),
        ("overridden", JsValue::TRUE),
        ("defaultContextWindow", JsValue::from_f64(131_072.0)),
        ("defaultMaxTokens", JsValue::from_f64(8_192.0)),
        ("t", property(&bench, "t")),
        ("disabled", JsValue::FALSE),
        ("onChange", change.into_js_value()),
        ("onReset", Function::new_no_args("").into()),
    ]);
    let editor = deepseek_models_editor_component().unwrap();
    let collapsed = smRender(&bench, &editor, &props);
    smClick(&smFind(
        &collapsed,
        "aria-label",
        &JsValue::from_str("Capacities 1"),
    ));
    let expanded = smRender(&bench, &editor, &props);
    let context = smFind(
        &expanded,
        "aria-label",
        &JsValue::from_str("Context window 1"),
    );
    assert_eq!(
        smProp(&context, "placeholder").as_string().as_deref(),
        Some("131072")
    );
    smChange(&context, "abc");
    let invalid = smRender(&bench, &editor, &props);
    let context = smFind(
        &invalid,
        "aria-label",
        &JsValue::from_str("Context window 1"),
    );
    assert_eq!(
        smProp(&context, "value").as_string().as_deref(),
        Some("abc")
    );
    smBlur(&context, "abc");
    let retained = smRender(&bench, &editor, &props);
    assert_eq!(
        smProp(
            &smFind(
                &retained,
                "aria-label",
                &JsValue::from_str("Context window 1")
            ),
            "value"
        )
        .as_string()
        .as_deref(),
        Some("abc")
    );
    smClick(&smFind(
        &retained,
        "aria-label",
        &JsValue::from_str("Delete model 1"),
    ));
    let latest = changes.get(changes.length() - 1);
    Reflect::set(&props, &JsValue::from_str("models"), &latest).unwrap();
    let reindexed = smRender(&bench, &editor, &props);
    assert!(
        smFind(
            &reindexed,
            "aria-label",
            &JsValue::from_str("Context window 1")
        )
        .is_undefined()
    );
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)] // One picker lifecycle pins request, modal, selection, and adopted row shape.
async fn model_discovery_picker_adopts_default_candidates() {
    let bench = configure();
    let api = makeModelsApi();
    let discovered = Array::new();
    let discovered_calls = discovered.clone();
    let discovered_change = Closure::wrap(Box::new(move |models: JsValue| {
        discovered_calls.push(&models);
    }) as Box<dyn FnMut(JsValue)>);
    let list_props = object(&[
        (
            "models",
            js_sys::JSON::parse(r#"[{"id":"existing"}]"#).unwrap(),
        ),
        ("onChange", discovered_change.into_js_value()),
        (
            "probe",
            object(&[
                ("settingsNs", JsValue::from_str("llm-pi-ai")),
                ("baseURL", JsValue::from_str("https://example.test/v1")),
                ("ignored", JsValue::TRUE),
            ]),
        ),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("disabled", JsValue::FALSE),
    ]);
    let list = model_list_editor_component().unwrap();
    let initial = smRender(&bench, &list, &list_props);
    smClick(&smFind(
        &initial,
        "aria-label",
        &JsValue::from_str("Capacities 1"),
    ));
    let expanded = smRender(&bench, &list, &list_props);
    let chevron = smFindKind(&expanded, "svg");
    assert_eq!(
        property(&smProp(&chevron, "style"), "transform")
            .as_string()
            .as_deref(),
        Some("rotate(90deg)")
    );
    assert!(
        !smFind(
            &expanded,
            "aria-label",
            &JsValue::from_str("Route context window 1")
        )
        .is_undefined()
    );
    assert!(
        !smFind(
            &expanded,
            "aria-label",
            &JsValue::from_str("Route max output tokens 1")
        )
        .is_undefined()
    );
    assert!(
        smProp(
            &smFind(
                &expanded,
                "aria-label",
                &JsValue::from_str("Route context window 1")
            ),
            "onBlur"
        )
        .is_undefined()
    );
    smClick(&smFindText(&expanded, "Fetch available models"));
    JsFuture::from(smTick()).await.unwrap();
    let calls = smCalls(&api);
    let request = property(&calls.get(0), "1");
    assert_eq!(Object::keys(&Object::from(request)).length(), 2);
    let picker = smRender(&bench, &list, &list_props);
    assert!(smText(&picker).contains("Choose models to add"));
    let modal = smFindKind(&picker, "Modal");
    assert_eq!(
        smProp(&modal, "closeLabel").as_string().as_deref(),
        Some("Close")
    );
    assert_eq!(
        smProp(&modal, "description").as_string().as_deref(),
        Some("Choose candidates.")
    );
    assert_eq!(
        smProp(&modal, "className").as_string().as_deref(),
        Some("seekdeep-settings-models-fetchDialog")
    );
    assert!(
        !smFind(
            &picker,
            "className",
            &JsValue::from_str("seekdeep-settings-models-candidateList")
        )
        .is_undefined()
    );
    let checkbox = smFind(&picker, "type", &JsValue::from_str("checkbox"));
    assert_eq!(smProp(&checkbox, "checked"), JsValue::TRUE);
    smClick(&smFindText(&picker, "Add selected"));
    assert_eq!(discovered.length(), 1);
    let adopted = discovered.get(0).dyn_into::<Array>().unwrap();
    assert_eq!(adopted.length(), 2);
    assert_eq!(
        property(&adopted.get(0), "id").as_string().as_deref(),
        Some("existing")
    );
    assert_eq!(
        property(&adopted.get(1), "id").as_string().as_deref(),
        Some("deepseek-chat")
    );
    assert_eq!(
        property(&adopted.get(1), "name").as_string().as_deref(),
        Some("Chat")
    );
    assert_eq!(
        property(&adopted.get(1), "contextWindow").as_f64(),
        Some(128_000.0)
    );
    assert_eq!(
        property(&adopted.get(1), "maxTokens").as_f64(),
        Some(8192.0)
    );
    assert!(!Reflect::has(&adopted.get(1), &JsValue::from_str("ignored")).unwrap());
}

#[wasm_bindgen_test(async)]
async fn provider_retry_advances_settings_baseline_before_retrying_only_the_key() {
    let bench = configure();
    let api = makeModelsApi();
    failCredentialSets(&api, 1);
    let closes = Array::new();
    let close_calls = closes.clone();
    let close = Closure::wrap(Box::new(move |changed: bool| {
        close_calls.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let props = object(&[
        ("provider", JsValue::from_str("deepseek-official")),
        ("displayName", JsValue::from_str("DeepSeek")),
        ("namespace", property(&api, "namespace")),
        ("settingsPath", Array::new().into()),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", close.into_js_value()),
    ]);
    let editor = provider_editor_component().unwrap();
    let first = smRender(&bench, &editor, &props);
    smChange(
        &smFind(&first, "aria-label", &JsValue::from_str("API key")),
        "retry-key",
    );
    let customized = smFindText(&first, "Customized settings");
    assert!(!customized.is_undefined());
    let ready = smRender(&bench, &editor, &props);
    smChange(
        &smFind(&ready, "aria-label", &JsValue::from_str("Base URL")),
        "https://mirror.example/v1",
    );
    let staged = smRender(&bench, &editor, &props);
    smClick(&smFind(
        &staged,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    ));
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    assert_eq!(closes.length(), 0);
    let calls = smCalls(&api);
    let first_names = (0..calls.length())
        .map(|index| property(&calls.get(index), "0").as_string().unwrap())
        .filter(|name| name == "settings.mutate" || name == "credentials.set")
        .collect::<Vec<_>>();
    assert_eq!(first_names, ["settings.mutate", "credentials.set"]);

    let retry = smRender(&bench, &editor, &props);
    smClick(&smFind(
        &retry,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    ));
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    let calls = smCalls(&api);
    let names = (0..calls.length())
        .map(|index| property(&calls.get(index), "0").as_string().unwrap())
        .filter(|name| name == "settings.mutate" || name == "credentials.set")
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["settings.mutate", "credentials.set", "credentials.set"]
    );
    assert_eq!(closes.get(0), JsValue::TRUE);
}

#[wasm_bindgen_test(async)]
async fn provider_settings_baseline_advances_while_the_credential_write_is_still_pending() {
    let bench = configure();
    let api = makeModelsApi();
    let gate = deferCredentialSet(&api);
    let closes = Array::new();
    let close_calls = closes.clone();
    let close = Closure::wrap(Box::new(move |changed: bool| {
        close_calls.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let props = object(&[
        ("provider", JsValue::from_str("deepseek-official")),
        ("displayName", JsValue::from_str("DeepSeek")),
        ("namespace", property(&api, "namespace")),
        ("settingsPath", Array::new().into()),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", close.into_js_value()),
    ]);
    let editor = provider_editor_component().unwrap();
    let first = smRender(&bench, &editor, &props);
    smChange(
        &smFind(&first, "aria-label", &JsValue::from_str("API key")),
        "pending-key",
    );
    let with_key = smRender(&bench, &editor, &props);
    smChange(
        &smFind(&with_key, "aria-label", &JsValue::from_str("Base URL")),
        "https://pending.example/v1",
    );
    let staged = smRender(&bench, &editor, &props);
    smClick(&smFind(
        &staged,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    ));
    JsFuture::from(smTick()).await.unwrap();
    assert_eq!(closes.length(), 0);
    assert_eq!(smState(&bench, 3), JsValue::TRUE);
    assert_eq!(
        property(&smState(&bench, 5), "baseURL")
            .as_string()
            .as_deref(),
        Some("https://pending.example/v1")
    );
    assert_eq!(smState(&bench, 6).as_f64(), Some(5.0));

    property(&gate, "release")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&gate)
        .unwrap();
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    assert_eq!(closes.get(0), JsValue::TRUE);
}

#[wasm_bindgen_test(async)]
async fn custom_profile_is_committed_while_its_credential_write_is_still_pending() {
    let bench = configure();
    let api = makeModelsApi();
    let gate = deferCredentialSet(&api);
    let closes = Array::new();
    let close_calls = closes.clone();
    let close = Closure::wrap(Box::new(move |changed: bool| {
        close_calls.push(&JsValue::from_bool(changed));
    }) as Box<dyn FnMut(bool)>);
    let props = object(&[
        ("taken", Array::new().into()),
        (
            "protocols",
            Array::of1(&JsValue::from_str("openai-responses")).into(),
        ),
        ("revision", JsValue::from_f64(4.0)),
        ("api", property(&api, "api")),
        ("t", property(&bench, "t")),
        ("readOnly", JsValue::FALSE),
        ("onClose", close.into_js_value()),
    ]);
    let custom = custom_provider_card_component().unwrap();
    let first = smRender(&bench, &custom, &props);
    smChange(
        &smFind(&first, "aria-label", &JsValue::from_str("Provider ID")),
        "pending-gateway",
    );
    smChange(
        &smFind(&first, "aria-label", &JsValue::from_str("Base URL")),
        "https://pending.example/v1",
    );
    smChange(
        &smFind(&first, "aria-label", &JsValue::from_str("API key")),
        "pending-key",
    );
    let fields = smRender(&bench, &custom, &props);
    smClick(&smFind(
        &fields,
        "className",
        &JsValue::from_str("seekdeep-settings-models-addModelButton"),
    ));
    let row = smRender(&bench, &custom, &props);
    smChange(
        &smFind(&row, "aria-label", &JsValue::from_str("Model ID 1")),
        "pending-model",
    );
    let ready = smRender(&bench, &custom, &props);
    smClick(&smFind(
        &ready,
        "className",
        &JsValue::from_str("seekdeep-settings-models-primaryButton"),
    ));
    JsFuture::from(smTick()).await.unwrap();
    assert_eq!(closes.length(), 0);
    assert_eq!(smState(&bench, 9), JsValue::TRUE);

    property(&gate, "release")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&gate)
        .unwrap();
    JsFuture::from(smTick()).await.unwrap();
    JsFuture::from(smTick()).await.unwrap();
    assert_eq!(closes.get(0), JsValue::TRUE);
}
