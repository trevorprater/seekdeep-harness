//! Live Rust/WASM Skill row, catalog cache, input source, and plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_skill::{
    apply_client_ui_skill, configure_client_ui_skill, exported_skill_row_component, skill_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
if (typeof globalThis.document === 'undefined') {
  const nodes = []
  const selected = selector => {
    const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
    if (match === null) return []
    const plugin = JSON.parse(match[1])
    return nodes.filter(node => node.kind === 'style' && node.attributes['data-plugin'] === plugin)
  }
  globalThis.document = {
    querySelector(selector) { return selected(selector)[0] ?? null },
    querySelectorAll(selector) { return selected(selector) },
    createElement(kind) {
      return {
        kind, attributes: {}, textContent: '',
        setAttribute(name, value) { this.attributes[name] = value },
      }
    },
    head: { appendChild(node) { nodes.push(node); return node } },
  }
}

function hooks() {
  const slots = []
  let cursor = 0
  const Fragment = Symbol('Fragment')
  return {
    React: {
      Fragment,
      createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
      useState(initial) {
        const index = cursor++
        if (slots[index] === undefined) {
          const slot = { value: initial }
          slot.set = value => { slot.value = typeof value === 'function' ? value(slot.value) : value }
          slots[index] = slot
        }
        return [slots[index].value, slots[index].set]
      },
    },
    reset() { cursor = 0 },
  }
}
function textOf(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  return (node.children ?? []).map(textOf).join('')
}
function find(node, predicate) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (!Array.isArray(node) && predicate(node)) return node
  for (const child of Array.isArray(node) ? node : node.children ?? []) {
    const found = find(child, predicate)
    if (found !== undefined) return found
  }
  return undefined
}
const zh = {
  'row.running': '正在加载 skill', 'row.failed': 'skill 加载失败',
  'row.stopped': 'skill 加载已中止', 'row.instructions': '说明', 'menu.userOnly': '仅用户',
}

export function makeSkillRowBench(block) {
  const state = hooks()
  const calls = []
  return {
    hooks: state, React: state.React,
    primitives: {
      IconChevronDownOutline14: 'IconChevronDownOutline14', IconInspectOutline12: 'IconInspectOutline12',
      IconSkillOutline16: 'IconSkillOutline16', StateDot: 'StateDot',
    },
    props: {
      block, inspect() { calls.push('inspect') }, t(key) { return zh[key] ?? key },
    }, calls,
  }
}
export function skillRowRender(bench, component) { bench.hooks.reset(); return component(bench.props) }
export function skillRowText(tree) { return textOf(tree) }
export function skillRowDisclosure(tree) { return find(tree, node => node.props?.role === 'button' && node.kind === 'div') }
export function skillRowButton(tree, text) { return find(tree, node => node.kind === 'button' && textOf(node) === text) }
export function skillRowPre(tree) { return find(tree, node => node.kind === 'pre') }
export function skillRowCard(tree) { return find(tree, node => node.props?.['data-tool'] === 'skill') }
export function skillRowClick(node) { return node.props.onClick() }
export function skillRowKey(node, key) { return node.props.onKeyDown({ key, preventDefault() {} }) }
export function skillRowCalls(bench) { return bench.calls }
export function skillStyleCount() {
  return document.querySelectorAll('style[data-plugin="@seekdeep-ai/seekdeep-client-ui-skill"]').length
}

const CATALOG = [
  { name: 'commit-helper', description: 'commit flow', modelInvocable: true },
  { name: 'code-review', description: 'review flow', whenToUse: 'reviews', modelInvocable: true },
  { name: 'deploy', description: 'deploy flow', modelInvocable: true },
]
export function makeSkillPluginBench() {
  const row = makeSkillRowBench({ callId: 'call-skill', argsRaw: '{"name":"x"}' })
  const effects = [], entries = [], payloads = [], signals = [], eventListeners = new Map()
  const localeRegistrations = []
  const lexiconNotifications = []
  let source
  let addressed
  let mode = 'success'
  let catalog = CATALOG
  let pendingResolve
  const list = (payload, signal) => {
    payloads.push(payload); signals.push(signal)
    if (mode === 'pending') return new Promise(resolve => { pendingResolve = resolve })
    if (mode === 'failure') return Promise.resolve({ result: { ok: false, error: { code: 'internal', message: 'boom', details: {} } } })
    return Promise.resolve({ result: { ok: true, value: { skills: catalog } } })
  }
  const own = dispose => { effects.push(dispose); return dispose }
  const ctx = {
    inputTriggers: { registerSource(value) { if (source !== undefined) throw new Error('already registered'); source = value; return () => { source = undefined } } },
    connection: { api: { skills: { list } } },
    sessions: { subagentAddress(id) { return id === addressed ? { childSessionId: id } : undefined } },
    locale: {
      register(namespace, dictionaries) {
        const entry = { namespace, dictionaries }; localeRegistrations.push(entry)
        return () => localeRegistrations.splice(localeRegistrations.indexOf(entry), 1)
      },
      bind() { return key => zh[key] ?? key },
    },
    remote: {
      $on(name, listener) { eventListeners.set('remote:' + name, listener); return () => eventListeners.delete('remote:' + name) },
    },
    on(name, listener) { eventListeners.set(name, listener); return () => eventListeners.delete(name) },
    effect(setup) { return own(setup()) },
  }
  ctx.slots = {
    inject(name, install) { return own(install()) },
    register(options, component) { const entry = { options, component }; entries.push(entry); return () => entries.splice(entries.indexOf(entry), 1) },
  }
  ctx['remote'] = ctx.remote
  return {
    ...row, ctx, effects, entries, payloads, signals, lexiconNotifications, localeRegistrations,
    get source() { return source },
    setAddressed(value) { addressed = value }, setMode(value) { mode = value },
    setCatalog(value) { catalog = value }, resolvePending() {
      pendingResolve?.({ result: { ok: true, value: { skills: catalog } } })
    },
    dispatch(name, ...args) { eventListeners.get(name)?.(...args) },
  }
}
export function skillSource(bench) { return bench.source }
export function skillEntries(bench) { return bench.entries }
export function skillSetAddressed(bench, id) { bench.setAddressed(id) }
export function skillSetMode(bench, mode) { bench.setMode(mode) }
export function skillSetCatalog(bench, catalog) { bench.setCatalog(catalog) }
export function skillResolvePending(bench) { bench.resolvePending() }
export function skillPayloads(bench) { return bench.payloads }
export function skillSignals(bench) { return bench.signals }
export function skillLocaleRegistrations(bench) { return bench.localeRegistrations }
export function skillDispatch(bench, name, first, second) { bench.dispatch(name, first, second) }
export function skillDispose(bench) { for (const dispose of bench.effects.splice(0).reverse()) dispose() }
export function skillSession(id) { return { sessionId: id } }
export function skillRequest(query, signal = new AbortController().signal) { return { query, position: 'leading', signal } }
export function skillAbortSignal() { const controller = new AbortController(); controller.abort(); return controller.signal }
export function skillTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function skillCatalog() { return CATALOG }
"#)]
extern "C" {
    fn makeSkillRowBench(block: &JsValue) -> JsValue;
    fn skillRowRender(bench: &JsValue, component: &Function) -> JsValue;
    fn skillRowText(tree: &JsValue) -> String;
    fn skillRowDisclosure(tree: &JsValue) -> JsValue;
    fn skillRowButton(tree: &JsValue, text: &str) -> JsValue;
    fn skillRowPre(tree: &JsValue) -> JsValue;
    fn skillRowCard(tree: &JsValue) -> JsValue;
    fn skillRowClick(node: &JsValue) -> JsValue;
    fn skillRowKey(node: &JsValue, key: &str) -> JsValue;
    fn skillRowCalls(bench: &JsValue) -> Array;
    fn skillStyleCount() -> u32;
    fn makeSkillPluginBench() -> JsValue;
    fn skillSource(bench: &JsValue) -> JsValue;
    fn skillEntries(bench: &JsValue) -> Array;
    fn skillSetAddressed(bench: &JsValue, id: &str);
    fn skillSetMode(bench: &JsValue, mode: &str);
    fn skillSetCatalog(bench: &JsValue, catalog: &JsValue);
    fn skillResolvePending(bench: &JsValue);
    fn skillPayloads(bench: &JsValue) -> Array;
    fn skillSignals(bench: &JsValue) -> Array;
    fn skillLocaleRegistrations(bench: &JsValue) -> Array;
    fn skillDispatch(bench: &JsValue, name: &str, first: &JsValue, second: &JsValue);
    fn skillDispose(bench: &JsValue);
    fn skillSession(id: &str) -> JsValue;
    fn skillRequest(query: &str, signal: &JsValue) -> JsValue;
    fn skillAbortSignal() -> JsValue;
    fn skillTick() -> Promise;
    fn skillCatalog() -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    let direct = Reflect::get(value, &JsValue::from_str(key)).unwrap();
    if !direct.is_undefined() {
        return direct;
    }
    let props = Reflect::get(value, &JsValue::from_str("props")).unwrap_or(JsValue::UNDEFINED);
    Reflect::get(&props, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn row_component(bench: &JsValue) -> Function {
    configure_client_ui_skill(property(bench, "React"), property(bench, "primitives")).unwrap();
    exported_skill_row_component().unwrap().dyn_into().unwrap()
}

fn json(value: &str) -> JsValue {
    js_sys::JSON::parse(value).unwrap()
}

#[wasm_bindgen_test]
fn row_disclosure_keyboard_states_output_and_inspect_are_live() {
    let settled = json(
        r#"{"kind":"tool-result","callId":"call-skill","call":{"argsRaw":"{\"name\":\"dsh-manage-issues\"}"},"content":[{"type":"text","text":"Follow the issue workflow.\nKeep project fields in sync."}],"isError":false}"#,
    );
    let bench = makeSkillRowBench(&settled);
    let component = row_component(&bench);
    let compact = skillRowRender(&bench, &component);
    let disclosure = skillRowDisclosure(&compact);
    assert_eq!(
        property(&disclosure, "aria-expanded").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&skillRowCard(&compact), "data-state")
            .as_string()
            .as_deref(),
        Some("ok")
    );
    assert_eq!(skillStyleCount(), 1);
    skillRowKey(&disclosure, "Escape");
    assert_eq!(
        property(
            &skillRowDisclosure(&skillRowRender(&bench, &component)),
            "aria-expanded"
        )
        .as_bool(),
        Some(false)
    );
    skillRowKey(&disclosure, "Enter");
    let open = skillRowRender(&bench, &component);
    assert_eq!(
        skillRowText(&skillRowPre(&open)),
        "Follow the issue workflow.\nKeep project fields in sync."
    );
    assert!(!skillRowText(&open).contains("{\"name\""));
    skillRowClick(&skillRowButton(&open, "Inspect"));
    assert_eq!(skillRowCalls(&bench).length(), 1);
    let open_disclosure = skillRowDisclosure(&open);
    skillRowKey(&open_disclosure, " ");
    assert_eq!(
        property(
            &skillRowDisclosure(&skillRowRender(&bench, &component)),
            "aria-expanded"
        )
        .as_bool(),
        Some(false)
    );

    let failed = json(
        r#"{"kind":"tool-result","callId":"call-skill","call":{"argsRaw":"{\"name\":\"broken\"}"},"content":[{"type":"text","text":"SkillError: missing resource\nCheck SKILL.md."}],"isError":true,"error":{"name":"SkillError","code":"missing"}}"#,
    );
    let failed_bench = makeSkillRowBench(&failed);
    let failed_component = row_component(&failed_bench);
    let failed_tree = skillRowRender(&failed_bench, &failed_component);
    assert!(skillRowText(&failed_tree).contains("SkillError: missing resource"));
    assert!(!skillRowText(&failed_tree).contains("Check SKILL.md."));
    skillRowClick(&skillRowDisclosure(&failed_tree));
    let failed_open = skillRowRender(&failed_bench, &failed_component);
    assert!(skillRowText(&skillRowPre(&failed_open)).contains("Check SKILL.md."));
    assert_eq!(
        property(&skillRowPre(&failed_open), "data-error").as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test]
fn running_stopped_structured_and_name_fallbacks_are_live() {
    for (block, expected, state) in [
        (
            json(r#"{"callId":"call-skill","argsRaw":"{\"name\":\"dsh-manage-issues\"}"}"#),
            "dsh-manage-issues",
            "running",
        ),
        (
            json(
                r#"{"kind":"tool-result","callId":"call-skill","call":{"argsRaw":"{\"name\":\"stopped\"}"},"content":[],"isError":false,"error":{"name":"InterruptedError","code":"interrupted"}}"#,
            ),
            "skill 加载已中止",
            "stopped",
        ),
        (
            json(r#"{"callId":"call-skill","argsRaw":"{\"name\":"}"#),
            "{\"name\":",
            "running",
        ),
        (
            json(
                r#"{"kind":"tool-result","callId":"call-skill","call":null,"content":[],"isError":false}"#,
            ),
            "call-skill",
            "ok",
        ),
    ] {
        let bench = makeSkillRowBench(&block);
        let component = row_component(&bench);
        let tree = skillRowRender(&bench, &component);
        assert!(skillRowText(&tree).contains(expected));
        assert_eq!(
            property(&skillRowCard(&tree), "data-state")
                .as_string()
                .as_deref(),
            Some(state)
        );
    }
    let structured = json(
        r#"{"kind":"tool-result","callId":"call-skill","call":{"argsRaw":"{\"name\":\"structured\"}"},"content":[{"type":"reasoning","text":"note"}],"isError":false}"#,
    );
    let bench = makeSkillRowBench(&structured);
    let component = row_component(&bench);
    let compact = skillRowRender(&bench, &component);
    skillRowClick(&skillRowDisclosure(&compact));
    assert!(skillRowText(&skillRowRender(&bench, &component)).contains("\"type\": \"reasoning\""));
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn candidates_cache_addressing_failure_abort_and_pick_are_live() {
    let bench = makeSkillPluginBench();
    configure_client_ui_skill(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_skill(property(&bench, "ctx")).unwrap();
    assert_eq!(
        skill_inject()
            .iter()
            .map(|value| value.as_string().unwrap())
            .collect::<Vec<_>>(),
        [
            "inputTriggers",
            "connection",
            "sessions",
            "slots",
            "locale",
            "remote"
        ]
    );
    assert_eq!(skillEntries(&bench).length(), 1);
    let entry = skillEntries(&bench).get(0);
    assert_eq!(
        property(&property(&entry, "options"), "key")
            .as_string()
            .as_deref(),
        Some("skill")
    );
    assert_eq!(skillLocaleRegistrations(&bench).length(), 1);
    let source = skillSource(&bench);
    let candidates = property(&source, "candidates")
        .dyn_into::<Function>()
        .unwrap();
    let request = skillRequest("co", &JsValue::UNDEFINED);
    let first = JsFuture::from(Promise::resolve(
        &candidates
            .call2(&source, &skillSession("s1"), &request)
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&first).length(), 2);
    assert_eq!(skillPayloads(&bench).length(), 1);
    assert_eq!(
        property(&skillPayloads(&bench).get(0), "sessionId")
            .as_string()
            .as_deref(),
        Some("s1")
    );
    let second = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("s1"),
                &skillRequest("dep", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&second).length(), 1);
    assert_eq!(skillPayloads(&bench).length(), 1);
    skillSetAddressed(&bench, "child");
    let addressed = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("child"),
                &skillRequest("", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&addressed).length(), 0);
    assert_eq!(skillPayloads(&bench).length(), 1);

    skillSetMode(&bench, "failure");
    let failure = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("failed"),
                &skillRequest("", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap_err();
    assert!(
        property(&failure, "message")
            .as_string()
            .is_some_and(|message| message.contains("skill.list failed: internal: boom"))
    );
    skillSetMode(&bench, "success");
    let retried = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("failed"),
                &skillRequest("", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&retried).length(), 3);

    let aborted = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("aborted"),
                &skillRequest("co", &skillAbortSignal()),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&aborted).length(), 0);
    let warmed = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("aborted"),
                &skillRequest("co", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&warmed).length(), 2);

    let pick = property(&source, "onPick").dyn_into::<Function>().unwrap();
    let outcome = pick
        .call1(
            &source,
            &json(r#"{"candidate":{"name":"commit-helper","description":"commit flow"}}"#),
        )
        .unwrap();
    assert_eq!(
        property(&outcome, "text").as_string().as_deref(),
        Some("/commit-helper ")
    );
    assert!(property(&source, "codec").is_undefined());
    skillDispose(&bench);
    assert_eq!(skillEntries(&bench).length(), 0);
    assert_eq!(skillLocaleRegistrations(&bench).length(), 0);
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn single_flight_warm_lexicon_listeners_and_invalidation_are_live() {
    let bench = makeSkillPluginBench();
    configure_client_ui_skill(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_skill(property(&bench, "ctx")).unwrap();
    let source = skillSource(&bench);
    let candidates = property(&source, "candidates")
        .dyn_into::<Function>()
        .unwrap();
    skillSetMode(&bench, "pending");
    let first = candidates
        .call2(
            &source,
            &skillSession("s1"),
            &skillRequest("co", &JsValue::UNDEFINED),
        )
        .unwrap();
    let second = candidates
        .call2(
            &source,
            &skillSession("s1"),
            &skillRequest("dep", &JsValue::UNDEFINED),
        )
        .unwrap();
    JsFuture::from(skillTick()).await.unwrap();
    assert_eq!(skillPayloads(&bench).length(), 1);
    let lexicon = property(&source, "lexicon").dyn_into::<Function>().unwrap();
    assert!(
        lexicon
            .call1(&source, &skillSession("s1"))
            .unwrap()
            .is_undefined()
    );
    let notifications = Array::new();
    let notify_values = notifications.clone();
    let listener = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
        notify_values.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let subscribe = property(&source, "subscribeLexicon")
        .dyn_into::<Function>()
        .unwrap();
    let off = subscribe
        .call2(&source, &skillSession("s1"), &listener.into_js_value())
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    skillResolvePending(&bench);
    JsFuture::from(Promise::resolve(&first)).await.unwrap();
    JsFuture::from(Promise::resolve(&second)).await.unwrap();
    assert_eq!(notifications.length(), 1);
    assert_eq!(
        Array::from(&lexicon.call1(&source, &skillSession("s1")).unwrap()).length(),
        3
    );
    skillSetMode(&bench, "success");
    JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("s2"),
                &skillRequest("", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(skillPayloads(&bench).length(), 2);
    skillDispatch(
        &bench,
        "remote:agent-preset/selected",
        &JsValue::from_str("s1"),
        &JsValue::from_str("minimal"),
    );
    assert_eq!(notifications.length(), 2);
    assert_eq!(
        property(&skillSignals(&bench).get(0), "aborted").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&skillSignals(&bench).get(1), "aborted").as_bool(),
        Some(false)
    );
    off.call0(&JsValue::UNDEFINED).unwrap();
    JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("s2"),
                &skillRequest("co", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(skillPayloads(&bench).length(), 2);
    JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("s1"),
                &skillRequest("", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(skillPayloads(&bench).length(), 3);
    assert_eq!(notifications.length(), 2);
    let warm = property(&source, "warm").dyn_into::<Function>().unwrap();
    warm.call1(&source, &skillSession("s3")).unwrap();
    JsFuture::from(skillTick()).await.unwrap();
    assert_eq!(skillPayloads(&bench).length(), 4);
    assert_eq!(
        Array::from(&lexicon.call1(&source, &skillSession("s3")).unwrap()).length(),
        3
    );
    skillDispatch(
        &bench,
        "connection/reset",
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    );
    for index in 1..=3 {
        assert_eq!(
            property(&skillSignals(&bench).get(index), "aborted").as_bool(),
            Some(true)
        );
    }
    skillDispose(&bench);
}

#[wasm_bindgen_test(async)]
async fn user_only_description_and_reapply_lifecycle_are_live() {
    let bench = makeSkillPluginBench();
    configure_client_ui_skill(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_skill(property(&bench, "ctx")).unwrap();
    skillSetCatalog(
        &bench,
        &json(
            r#"[{"name":"shared-skill","description":"both surfaces","modelInvocable":true},{"name":"user-only-skill","description":"user surface only","modelInvocable":false}]"#,
        ),
    );
    let source = skillSource(&bench);
    let candidates = property(&source, "candidates")
        .dyn_into::<Function>()
        .unwrap();
    let rows = JsFuture::from(Promise::resolve(
        &candidates
            .call2(
                &source,
                &skillSession("s1"),
                &skillRequest("", &JsValue::UNDEFINED),
            )
            .unwrap(),
    ))
    .await
    .unwrap();
    let rows = Array::from(&rows);
    assert_eq!(
        property(&rows.get(1), "description").as_string().as_deref(),
        Some("仅用户 · user surface only")
    );
    skillDispose(&bench);
    apply_client_ui_skill(property(&bench, "ctx")).unwrap();
    assert!(!skillSource(&bench).is_undefined());
    skillDispose(&bench);
}
