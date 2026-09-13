//! Live JavaScript-boundary coverage for the compiled settings shell components.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_settings_general::{
    apply_client_ui_settings_general, configure_client_ui_settings_general, settings_general_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function makeSettingsGeneralBench(loopback) {
  const registrations = []
  const slotCalls = []
  const effects = []
  const state = {
    cursor: 0,
    overrides: [],
    describeCalls: 0,
    openCalls: 0,
    rows: [
      { id: 'general', order: 0, label: 'General' },
      { id: 'models', order: 10, label: 'Models' },
    ],
    steps: [{ id: 'welcome', order: -100 }, { id: 'credential', order: 0 }],
    sessions: { phase: 'ready', current: undefined, byId: {} },
  }
  const documentListeners = new Map()
  globalThis.document = {
    head: { appendChild() {} },
    createElement() { return { setAttribute() {}, textContent: '' } },
    addEventListener(type, listener) { documentListeners.set(type, listener) },
    removeEventListener(type, listener) {
      if (documentListeners.get(type) === listener) documentListeners.delete(type)
    },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = state.cursor++
      const value = index < state.overrides.length ? state.overrides[index] : initial
      return [value, update => {
        state.overrides[index] = typeof update === 'function' ? update(value) : update
      }]
    },
    useCallback(callback) { return callback },
    useEffect(effect) { effects.push(effect); return undefined },
    useId() { return 'settings-title-id' },
    useRef(value) { return { current: value } },
  }
  const primitives = Object.fromEntries([
    'Button', 'IconSettingsOutline14', 'IconSettingsOutline16', 'IconDataOutline16',
    'IconAgentPresetOutline16', 'IconPersonalizationOutline16', 'IconCloseOutline16',
  ].map(name => [name, name]))
  const webReact = {
    bindSnapshotSelector(store) {
      return selector => selector(store.getSnapshot())
    },
  }
  const slots = {
    inject(_name, register) { return register() },
    register(options, component) {
      registrations.push({ options, component })
      return () => {}
    },
    getVersion() { return 1 },
    entries(name) { return registrations.filter(entry => entry.options.name === name) },
    subscribe() { return () => {} },
  }
  const dictionary = {
    trigger: 'Settings', title: 'Settings', close: 'Close',
    openDocument: 'Open configuration file',
    'openDocument.error': 'Could not open configuration file',
    'general.nav': 'General',
  }
  const locale = {
    register() { return () => {} },
    bind() { return key => dictionary[key] ?? key },
    getSnapshot() { return { revision: 1 } },
    subscribe() { return () => {} },
  }
  const settings = {
    describe() {
      state.describeCalls += 1
      return Promise.resolve({ result: { ok: true, value: { hasDocument: true } } })
    },
    openDocument() {
      state.openCalls += 1
      return Promise.resolve({ result: { ok: true, value: { opened: true } } })
    },
  }
  const connection = { isLoopback: loopback, api: { settings } }
  const services = { slots, locale, connection }
  const ctx = {
    get(name) { return services[name] },
    effect(install) { const dispose = install(); effects.push(dispose); return dispose },
    on() { return () => {} },
  }
  const rootProps = wide => ({
    wide,
    useSections: selector => selector(state.rows),
    useOnboardingSteps: selector => selector(state.steps),
    useSessions: selector => selector(state.sessions),
    renderSlot(name, share, options) {
      const node = { kind: 'slot', props: { name, share, options }, children: [] }
      slotCalls.push(node.props)
      return node
    },
  })
  return {
    ctx, React, primitives, webReact, registrations, slotCalls, effects, state, rootProps,
    documentListeners,
  }
}

export function settingsGeneralRegistration(bench, name) {
  return bench.registrations.find(entry => entry.options.name === name)
}

export function invokeSettingsGeneral(component, props) {
  return component(props)
}

export function renderSettingsGeneralNode(node) {
  return typeof node?.kind === 'function' ? node.kind(node.props) : node
}

export function settingsGeneralText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(settingsGeneralText).join('')
}

export function settingsGeneralKinds(node, out = []) {
  if (node === null || node === undefined || node === false) return out
  if (typeof node === 'object') {
    if (typeof node.kind === 'string') out.push(node.kind)
    for (const child of node.children ?? []) settingsGeneralKinds(child, out)
  }
  return out
}

export function settingsGeneralFindKind(node, kind) {
  if (node === null || node === undefined) return undefined
  if (node.kind === kind) return node
  for (const child of node.children ?? []) {
    const found = settingsGeneralFindKind(child, kind)
    if (found !== undefined) return found
  }
  return undefined
}

export function settingsGeneralFindButtonText(node, text) {
  if (node === null || node === undefined) return undefined
  if (node.kind === 'button' && settingsGeneralText(node).includes(text)) return node
  for (const child of node.children ?? []) {
    const found = settingsGeneralFindButtonText(child, text)
    if (found !== undefined) return found
  }
  return undefined
}

export function settingsGeneralActionProps(bench) {
  const entry = settingsGeneralRegistration(bench, 'settings.action')
  return { ...entry.options.inject(), t: key => ({
    openDocument: 'Open configuration file',
    'openDocument.error': 'Could not open configuration file',
  })[key] ?? key }
}

export function settingsGeneralRootProps(bench, wide) { return bench.rootProps(wide) }
export function settingsGeneralOpenPanel(bench) {
  bench.state.cursor = 0
  bench.state.overrides = [true, 'models', new Set()]
}
export function settingsGeneralResetHooks(bench) { bench.state.cursor = 0 }
export function settingsGeneralSlotCalls(bench) { return bench.slotCalls }
export function settingsGeneralState(bench) { return bench.state }
export function settingsGeneralRunEffects(bench) {
  const pending = bench.effects.splice(0)
  for (const effect of pending) effect()
}
export function settingsGeneralDispatchEscape(bench) {
  bench.documentListeners.get('keydown')?.({ key: 'Escape' })
}
export function settingsGeneralTick() { return new Promise(resolve => setTimeout(resolve, 0)) }
"#)]
extern "C" {
    fn makeSettingsGeneralBench(loopback: bool) -> JsValue;
    fn settingsGeneralRegistration(bench: &JsValue, name: &str) -> JsValue;
    fn invokeSettingsGeneral(component: &JsValue, props: &JsValue) -> JsValue;
    fn renderSettingsGeneralNode(node: &JsValue) -> JsValue;
    fn settingsGeneralText(node: &JsValue) -> String;
    fn settingsGeneralKinds(node: &JsValue) -> Array;
    fn settingsGeneralFindKind(node: &JsValue, kind: &str) -> JsValue;
    fn settingsGeneralFindButtonText(node: &JsValue, text: &str) -> JsValue;
    fn settingsGeneralActionProps(bench: &JsValue) -> JsValue;
    fn settingsGeneralRootProps(bench: &JsValue, wide: bool) -> JsValue;
    fn settingsGeneralOpenPanel(bench: &JsValue);
    fn settingsGeneralResetHooks(bench: &JsValue);
    fn settingsGeneralSlotCalls(bench: &JsValue) -> Array;
    fn settingsGeneralState(bench: &JsValue) -> JsValue;
    fn settingsGeneralRunEffects(bench: &JsValue);
    fn settingsGeneralDispatchEscape(bench: &JsValue);
    fn settingsGeneralTick() -> js_sys::Promise;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).expect("property")
}

fn registration_component(bench: &JsValue, name: &str) -> JsValue {
    property(&settingsGeneralRegistration(bench, name), "component")
}

fn translated_props(wide: bool) -> JsValue {
    let props = js_sys::Object::new();
    Reflect::set(&props, &"wide".into(), &wide.into()).unwrap();
    let translate =
        wasm_bindgen::closure::Closure::wrap(Box::new(|key: String| match key.as_str() {
            "trigger" | "title" => "Settings".to_owned(),
            "close" => "Close".to_owned(),
            _ => key,
        }) as Box<dyn FnMut(String) -> String>);
    Reflect::set(&props, &"t".into(), &translate.into_js_value()).unwrap();
    props.into()
}

#[wasm_bindgen_test]
fn apply_registers_exact_seats_localized_chrome_and_general_body() {
    let bench = makeSettingsGeneralBench(true);
    configure_client_ui_settings_general(
        property(&bench, "React"),
        property(&bench, "primitives"),
        property(&bench, "webReact"),
    )
    .unwrap();
    apply_client_ui_settings_general(property(&bench, "ctx")).unwrap();
    assert_eq!(
        settings_general_inject().to_vec(),
        ["slots", "locale", "connection"]
            .into_iter()
            .map(JsValue::from_str)
            .collect::<Vec<_>>()
    );
    for name in [
        "sidebar.settings",
        "settings.trigger",
        "settings.header",
        "settings.action",
        "settings.close",
        "settings.section",
    ] {
        assert!(
            !settingsGeneralRegistration(&bench, name).is_undefined(),
            "{name}"
        );
    }

    let wide = invokeSettingsGeneral(
        &registration_component(&bench, "settings.trigger"),
        &translated_props(true),
    );
    assert_eq!(settingsGeneralText(&wide), "Settings");
    assert!(
        settingsGeneralKinds(&wide)
            .to_vec()
            .iter()
            .any(|kind| kind.as_string().as_deref() == Some("IconSettingsOutline16"))
    );
    let rail = invokeSettingsGeneral(
        &registration_component(&bench, "settings.trigger"),
        &translated_props(false),
    );
    assert!(settingsGeneralText(&rail).is_empty());
    assert!(
        settingsGeneralKinds(&rail)
            .to_vec()
            .iter()
            .any(|kind| kind.as_string().as_deref() == Some("IconSettingsOutline14"))
    );
    assert_eq!(
        settingsGeneralText(&invokeSettingsGeneral(
            &registration_component(&bench, "settings.header"),
            &translated_props(true),
        )),
        "Settings"
    );
    assert_eq!(
        settingsGeneralText(&invokeSettingsGeneral(
            &registration_component(&bench, "settings.close"),
            &translated_props(true),
        )),
        "Close"
    );

    let general_props = js_sys::Object::new();
    let render = wasm_bindgen::closure::Closure::wrap(Box::new(|name: String, _share: JsValue| {
        JsValue::from_str(&format!("slot:{name}"))
    })
        as Box<dyn FnMut(String, JsValue) -> JsValue>);
    Reflect::set(
        &general_props,
        &"renderSlot".into(),
        &render.into_js_value(),
    )
    .unwrap();
    let general = invokeSettingsGeneral(
        &registration_component(&bench, "settings.section"),
        &general_props.into(),
    );
    assert_eq!(settingsGeneralText(&general), "slot:settings.general.item");
}

#[wasm_bindgen_test(async)]
async fn document_action_loads_metadata_renders_and_opens_the_host_document() {
    let bench = makeSettingsGeneralBench(true);
    configure_client_ui_settings_general(
        property(&bench, "React"),
        property(&bench, "primitives"),
        property(&bench, "webReact"),
    )
    .unwrap();
    apply_client_ui_settings_general(property(&bench, "ctx")).unwrap();
    let component = registration_component(&bench, "settings.action");
    let props = settingsGeneralActionProps(&bench);
    assert!(invokeSettingsGeneral(&component, &props).is_null());
    settingsGeneralRunEffects(&bench);
    JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    settingsGeneralResetHooks(&bench);
    let action = invokeSettingsGeneral(&component, &props);
    assert_eq!(settingsGeneralText(&action), "Open configuration file");
    let button = settingsGeneralFindKind(&action, "Button");
    assert!(!button.is_undefined());
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    JsFuture::from(settingsGeneralTick()).await.unwrap();
    assert_eq!(
        property(&settingsGeneralState(&bench), "describeCalls").as_f64(),
        Some(1.0)
    );
    assert_eq!(
        property(&settingsGeneralState(&bench), "openCalls").as_f64(),
        Some(1.0)
    );
}

#[wasm_bindgen_test]
fn root_renders_onboarding_and_the_open_modal_navigation_contract() {
    let bench = makeSettingsGeneralBench(true);
    configure_client_ui_settings_general(
        property(&bench, "React"),
        property(&bench, "primitives"),
        property(&bench, "webReact"),
    )
    .unwrap();
    apply_client_ui_settings_general(property(&bench, "ctx")).unwrap();
    let component = registration_component(&bench, "sidebar.settings");
    let root = invokeSettingsGeneral(&component, &settingsGeneralRootProps(&bench, true));
    assert_eq!(
        property(&root, "kind").as_string().as_deref(),
        Some("Fragment")
    );
    let calls = settingsGeneralSlotCalls(&bench);
    let first_onboarding = calls.to_vec().into_iter().find(|call| {
        property(call, "name").as_string().as_deref() == Some("settings.onboarding")
            && property(&property(call, "share"), "stepId")
                .as_string()
                .as_deref()
                == Some("welcome")
    });
    let first_onboarding = first_onboarding.expect("welcome onboarding call");
    property(&property(&first_onboarding, "share"), "complete")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    settingsGeneralResetHooks(&bench);
    let root = invokeSettingsGeneral(&component, &settingsGeneralRootProps(&bench, true));
    assert!(
        settingsGeneralSlotCalls(&bench)
            .to_vec()
            .iter()
            .any(|call| {
                property(call, "name").as_string().as_deref() == Some("settings.onboarding")
                    && property(&property(call, "share"), "stepId")
                        .as_string()
                        .as_deref()
                        == Some("credential")
            })
    );

    let trigger = settingsGeneralFindKind(&root, "button");
    property(&property(&trigger, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    settingsGeneralResetHooks(&bench);
    let root = invokeSettingsGeneral(&component, &settingsGeneralRootProps(&bench, false));
    let children = Array::from(&property(&root, "children"));
    let panel_element = children.get(1);
    let panel = renderSettingsGeneralNode(&panel_element);
    settingsGeneralRunEffects(&bench);
    let dialog = settingsGeneralFindKind(&panel, "div");
    assert!(!dialog.is_undefined());
    let text = settingsGeneralText(&panel);
    assert!(text.contains("General"), "{text}");
    assert!(text.contains("Models"), "{text}");
    let models = settingsGeneralFindButtonText(&panel, "Models");
    property(&property(&models, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    settingsGeneralResetHooks(&bench);
    let rerendered = invokeSettingsGeneral(&component, &settingsGeneralRootProps(&bench, false));
    let panel = renderSettingsGeneralNode(&Array::from(&property(&rerendered, "children")).get(1));
    assert!(
        property(
            &property(&settingsGeneralFindButtonText(&panel, "Models"), "props"),
            "aria-current"
        )
        .as_string()
        .as_deref()
            == Some("true")
    );
    let panel_calls = settingsGeneralSlotCalls(&bench);
    assert!(panel_calls.to_vec().iter().any(|call| {
        property(call, "name").as_string().as_deref() == Some("settings.section")
            && property(&property(call, "options"), "only")
                .as_string()
                .as_deref()
                == Some("models")
    }));

    settingsGeneralRunEffects(&bench);
    settingsGeneralDispatchEscape(&bench);
    settingsGeneralResetHooks(&bench);
    let closed = invokeSettingsGeneral(&component, &settingsGeneralRootProps(&bench, false));
    assert!(
        Array::from(&property(&closed, "children"))
            .iter()
            .all(|child| !child.is_function())
    );
}
