//! Live WASM `AppRoot`, title, platform seed, and app-shell assembly parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_web::{
    app_root_component, app_shell_id, app_shell_inject, app_shell_name, apply_app_shell,
    build_render_app, configure_client_web, create_loader_status_store, create_signal,
    document_title_component, get_static_modules, platform_modules,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function shellBench() {
  const effects = []
  let refCursor = 0, effectCursor = 0
  const refs = []
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) {
      props ||= {}
      if (typeof kind === 'function') return kind({ ...props, children: children[0] })
      return { kind, props, children }
    },
    useSyncExternalStore(_subscribe, getSnapshot) { return getSnapshot() },
    useRef(initial) {
      const seat = refCursor++
      if (!(seat in refs)) refs[seat] = { current: initial }
      return refs[seat]
    },
    useEffect(effect, deps) {
      const seat = effectCursor++
      const previous = effects[seat]
      const changed = !previous || deps.some((value, index) => !Object.is(value, previous.deps[index]))
      if (!changed) return
      previous?.cleanup?.()
      effects[seat] = { deps: [...deps], cleanup: effect() }
    },
    render(component, props) { refCursor = 0; effectCursor = 0; return component(props) },
    unmount() { for (const effect of effects) effect?.cleanup?.(); effects.length = 0 },
  }
  const document = {
    title: 'SeekDeep Harness',
    head: { styles: [], appendChild(node) { this.styles.push(node); return node } },
    createElement(tag) { return { tag, attrs: {}, textContent: '', setAttribute(name, value) { this.attrs[name] = value } } },
  }
  globalThis.document = document
  const staticModules = {
    react: React, 'react/jsx-runtime': {}, 'react-dom': {}, 'react-dom/client': {},
    '@seekdeep-ai/cordis': {}, '@seekdeep-ai/seekdeep-client-ui-slots': {},
    '@seekdeep-ai/seekdeep-client-web-react': {}, '@seekdeep-ai/seekdeep-client-ui-primitives': {},
    '@seekdeep-ai/seekdeep-client-ui-attachment': {}, '@seekdeep-ai/seekdeep-client-schema-form': {},
  }
  const installs = [], services = {}
  const sessionState = { current: 's1', byId: { s1: { title: 'Session One' } } }
  const sessions = { list: { getSnapshot: () => sessionState, subscribe: () => () => {} } }
  const slots = {
    install(renderer) { installs.push(renderer) },
    renderSlot(name) { if (name !== 'root') throw new Error(name); return 'ROOT' },
  }
  services.sessions = sessions; services.slots = slots; services.layout = {}
  const ctx = {
    get(name) { return services[name] },
    reflect: { provide(name, value) { services[name] = value; return () => { delete services[name] } } },
  }
  const webReact = {
    bindSnapshotSelector(source) { return selector => selector(source.getSnapshot()) },
    createSlotRenderer() { return { renderRoot() {} } },
  }
  return { React, document, staticModules, installs, services, ctx, webReact,
    render: (component, props) => React.render(component, props) }
}
export function shellText(node) {
  if (node === undefined || node === null || node === false) return ''
  if (Array.isArray(node)) return node.map(shellText).join('')
  if (typeof node !== 'object') return String(node)
  return (node.children ?? []).map(shellText).join('')
}
export function shellRender(bench, component, props) { return bench.render(component, props) }
export function shellUnmount(bench) { bench.React.unmount() }
export function shellService(bench, name) { return bench.services[name] }
export function shellInstalls(bench) { return bench.installs }
export function shellDocumentTitle(bench) { return bench.document.title }
export function shellStyleCount(bench) { return bench.document.head.styles.length }
"#)]
extern "C" {
    fn shellBench() -> JsValue;
    fn shellText(node: &JsValue) -> String;
    fn shellRender(bench: &JsValue, component: &Function, props: &JsValue) -> JsValue;
    fn shellUnmount(bench: &JsValue);
    fn shellService(bench: &JsValue, name: &str) -> JsValue;
    fn shellInstalls(bench: &JsValue) -> Array;
    fn shellDocumentTitle(bench: &JsValue) -> String;
    fn shellStyleCount(bench: &JsValue) -> u32;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn configure(bench: &JsValue) {
    configure_client_web(
        property(bench, "React"),
        Object::new().into(),
        Object::new().into(),
        Object::new().into(),
        Object::new().into(),
        Object::new().into(),
        property(bench, "webReact"),
        property(bench, "staticModules"),
    )
    .unwrap();
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let output = Object::new();
    for (key, value) in entries {
        Reflect::set(&output, &JsValue::from_str(key), value).unwrap();
    }
    output.into()
}

#[wasm_bindgen_test]
fn app_root_renders_loading_failure_and_settled_real_ui_without_partial_shell() {
    let bench = shellBench();
    configure(&bench);
    assert_eq!(shellStyleCount(&bench), 1);
    let settled = create_signal(JsValue::FALSE).unwrap();
    let status = create_loader_status_store().unwrap();
    let error = create_signal(JsValue::UNDEFINED).unwrap();
    let calls = Array::new();
    let render_calls = calls.clone();
    let render_app = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
        render_calls.push(&JsValue::from_str("render"));
        JsValue::from_str("REAL")
    }) as Box<dyn FnMut() -> JsValue>);
    let props = object(&[
        ("settled", settled.clone()),
        ("status", status.clone()),
        ("error", error.clone()),
        ("renderApp", render_app.into_js_value()),
    ]);
    let component = app_root_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let loading = shellRender(&bench, &component, &props);
    assert_eq!(shellText(&loading), "HARNESSLoading plugins…");
    assert_eq!(calls.length(), 0);

    property(&status, "set")
        .dyn_into::<Function>()
        .unwrap()
        .call2(
            &status,
            &JsValue::from_str("broken"),
            &JsValue::from_str("failed"),
        )
        .unwrap();
    property(&error, "set")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&error, &JsValue::from_str("sweep failed"))
        .unwrap();
    let failed = shellRender(&bench, &component, &props);
    assert_eq!(
        shellText(&failed),
        "HARNESSFailed to load pluginsbrokensweep failed"
    );
    assert_eq!(calls.length(), 0);

    property(&settled, "set")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&settled, &JsValue::TRUE)
        .unwrap();
    assert_eq!(shellText(&shellRender(&bench, &component, &props)), "REAL");
    assert_eq!(calls.length(), 1);
}

#[wasm_bindgen_test]
fn document_title_projects_selected_title_and_restores_original_on_change_and_unmount() {
    let bench = shellBench();
    configure(&bench);
    let component = document_title_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    shellRender(
        &bench,
        &component,
        &object(&[("title", JsValue::from_str("Session"))]),
    );
    assert_eq!(shellDocumentTitle(&bench), "Session — SeekDeep Harness");
    shellRender(&bench, &component, &Object::new().into());
    assert_eq!(shellDocumentTitle(&bench), "SeekDeep Harness");
    shellRender(
        &bench,
        &component,
        &object(&[("title", JsValue::from_str("Again"))]),
    );
    shellUnmount(&bench);
    assert_eq!(shellDocumentTitle(&bench), "SeekDeep Harness");
}

#[wasm_bindgen_test]
fn app_shell_installs_renderer_provides_lazy_service_and_renders_title_plus_root() {
    let bench = shellBench();
    configure(&bench);
    assert_eq!(app_shell_id(), "@seekdeep-ai/seekdeep-client-app-shell");
    assert_eq!(app_shell_name(), "app-shell");
    assert_eq!(
        app_shell_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["slots", "sessions", "layout"]
    );
    apply_app_shell(property(&bench, "ctx")).unwrap();
    assert_eq!(shellInstalls(&bench).length(), 1);
    let service = shellService(&bench, "appShell");
    let render = property(&service, "renderApp")
        .dyn_into::<Function>()
        .unwrap();
    let first = render.call0(&service).unwrap();
    assert_eq!(shellText(&first), "ROOT");
    assert!(Object::is(
        &property(&service, "renderApp"),
        &property(&service, "renderApp")
    ));
    assert!(Object::is(
        &get_static_modules().unwrap(),
        &property(&bench, "staticModules")
    ));
    assert_eq!(platform_modules().length(), 11);
    assert!(build_render_app(Object::new().into()).is_err());
}
