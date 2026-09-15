//! Live WASM registry, settings, media, slot, Store, and Appearance parity.

#![cfg(target_arch = "wasm32")]
#![allow(clippy::float_cmp)] // Source-owned order and revision values are exact integers.

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_theme::{apply_client_ui_theme, configure_client_ui_theme, theme_inject};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function themeProduce(state, recipe) {
  const draft = JSON.parse(JSON.stringify(state))
  const replacement = recipe(draft)
  return replacement === undefined ? draft : replacement
}
export function themeProduceFunction() { return themeProduce }

export function themeBench(initialSystemDark = false) {
  const styleNodes = []
  const document = {
    head: { appendChild(node) { styleNodes.push(node); return node } },
    createElement(tag) {
      return { tag, attrs: {}, textContent: '', setAttribute(name, value) { this.attrs[name] = value } }
    },
  }
  globalThis.document = document
  const mediaListeners = new Set()
  const media = {
    matches: initialSystemDark,
    addEventListener(name, listener) { if (name === 'change') mediaListeners.add(listener) },
    removeEventListener(name, listener) { if (name === 'change') mediaListeners.delete(listener) },
    flip() {
      this.matches = !this.matches
      for (const listener of [...mediaListeners]) listener()
    },
  }
  globalThis.matchMedia = query => {
    if (query !== '(prefers-color-scheme: dark)') throw new Error(`unexpected media ${query}`)
    return media
  }

  const React = {
    createElement(kind, props, ...children) {
      props ||= {}
      if (typeof kind === 'function') return kind(props)
      return { kind, props, children }
    },
  }
  const primitives = Object.fromEntries([
    'IconLightOutline16', 'IconDarkOutline16', 'IconFollowsystemOutline16',
  ].map(name => [name, name]))

  let hostSnapshot = { status: 'ready', value: undefined, revision: undefined, writable: true }
  const hostListeners = new Set()
  const hostWrites = []
  const host = {
    getSnapshot() { return hostSnapshot },
    subscribe(listener) { hostListeners.add(listener); return () => hostListeners.delete(listener) },
    set(field, value) {
      hostWrites.push([field, value])
      hostSnapshot = {
        ...hostSnapshot,
        value: { ...(hostSnapshot.value ?? {}), [field]: value },
        revision: (hostSnapshot.revision ?? 0) + 1,
      }
      for (const listener of [...hostListeners]) listener()
      return Promise.resolve()
    },
    publish(preference, revision = 1) {
      hostSnapshot = { ...hostSnapshot, value: preference === undefined ? undefined : { preference }, revision }
      for (const listener of [...hostListeners]) listener()
    },
  }

  const dictionaries = new Map()
  const locale = {
    register(namespace, rows) {
      dictionaries.set(namespace, rows)
      return () => { if (dictionaries.get(namespace) === rows) dictionaries.delete(namespace) }
    },
  }
  const registrations = []
  const declarations = new Map()
  const slots = {
    register(options, component) {
      const row = { options, component }
      registrations.push(row)
      return () => {
        const index = registrations.indexOf(row)
        if (index >= 0) registrations.splice(index, 1)
      }
    },
    inject(name, callback) {
      const row = { callback, dispose: undefined }
      declarations.set(name, row)
      return () => {
        row.dispose?.()
        if (declarations.get(name) === row) declarations.delete(name)
      }
    },
    declare(name) {
      const row = declarations.get(name)
      if (row && row.dispose === undefined) row.dispose = row.callback()
    },
    collapse(name) {
      const row = declarations.get(name)
      row?.dispose?.()
      if (row) row.dispose = undefined
    },
  }
  const settingsScope = { bind(spec) { if (spec.namespace !== 'ui-theme') throw new Error('wrong namespace'); return host } }
  const services = {
    slots, locale, connection: {}, remote: {}, settingsScope,
  }
  const explicitEffects = []
  const owned = []
  const eventListeners = new Map()
  const emitted = []
  const ctx = {
    get(name) { return services[name] },
    provide(name, value) {
      services[name] = value
      owned.push(() => { if (services[name] === value) delete services[name] })
    },
    effect(install, label) {
      const dispose = install()
      explicitEffects.push({ label, dispose })
      return dispose
    },
    on(name, listener) {
      let listeners = eventListeners.get(name)
      if (!listeners) eventListeners.set(name, listeners = new Set())
      listeners.add(listener)
      const off = () => listeners.delete(listener)
      owned.push(off)
      return off
    },
    emit(name, value) {
      emitted.push({ name, value })
      for (const listener of [...eventListeners.get(name) ?? []]) listener(value)
    },
  }
  const declare = name => slots.declare(name)
  const disposeAll = () => {
    for (const dispose of [...owned].reverse()) dispose()
    owned.length = 0
    for (const effect of [...explicitEffects].reverse()) effect.dispose?.()
    explicitEffects.length = 0
    for (const name of [...declarations.keys()]) {
      slots.collapse(name)
      declarations.delete(name)
    }
  }
  let instance
  const ensureAppearance = () => {
    const registration = registrations.find(row => row.options.id === 'appearance')
    if (!registration) return undefined
    if (!instance) {
      instance = registration.options.store.create()
      const injected = registration.options.inject(instance.actions)
      instance.injected = injected
      instance.registration = registration
    }
    return instance
  }
  const copy = {
    'appearance.title': 'Appearance', 'appearance.light': 'Light',
    'appearance.dark': 'Dark', 'appearance.system': 'System',
  }
  const renderAppearance = () => {
    const appearance = ensureAppearance()
    return appearance.registration.component({
      useStore: selector => selector(appearance.getSnapshot()),
      t: key => copy[key] ?? key,
      ...appearance.injected,
    })
  }
  return {
    React, primitives, ctx, services, host, hostWrites, hostListeners, dictionaries,
    registrations, declarations, explicitEffects, emitted, eventListeners, media,
    mediaListeners, styleNodes, declare, disposeAll, ensureAppearance, renderAppearance,
  }
}

export function themeService(bench) { return bench.services.theme }
export function themeSnapshot(bench) { return bench.services.theme.getTheme() }
export function themeSet(bench, id) { return bench.services.theme.setTheme(id) }
export function themeRegister(bench, definition) { return bench.services.theme.register(definition) }
export function themeOverride(bench, source, tokens) { return bench.services.theme.overrideTokens(source, tokens) }
export function themeInspect(bench) { return bench.services.theme.exportInspectTokens() }
export function themePublishHost(bench, preference, revision) { bench.host.publish(preference, revision) }
export function themeFlipMedia(bench) { bench.media.flip() }
export function themeHostWrites(bench) { return bench.hostWrites }
export function themeEvents(bench) { return bench.emitted.filter(row => row.name === 'theme/change').map(row => row.value) }
export function themeEffectLabels(bench) { return bench.explicitEffects.map(row => row.label) }
export function themeDictionary(bench, locale) { return bench.dictionaries.get('settings.theme')?.[locale] }
export function themeDeclareItems(bench) { bench.declare('settings.general.item') }
export function themeRegistration(bench) { return bench.registrations.find(row => row.options.id === 'appearance') }
export function themeAppearanceState(bench) { return bench.ensureAppearance()?.getSnapshot() }
export function themeRenderAppearance(bench) { return bench.renderAppearance() }
export function themeFind(node, property, value) {
  if (!node) return undefined
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = themeFind(child, property, value)
    if (found) return found
  }
  return undefined
}
export function themeFindAll(node, property, value, out = []) {
  if (!node) return out
  if (node.props?.[property] === value) out.push(node)
  for (const child of node.children ?? []) themeFindAll(child, property, value, out)
  return out
}
export function themeClick(node) { return node.props.onClick() }
export function themeDispose(bench) { bench.disposeAll() }
export function themeMediaListenerCount(bench) { return bench.mediaListeners.size }
export function themeHostListenerCount(bench) { return bench.hostListeners.size }
export function themeStylesheetCount(bench) {
  return bench.styleNodes.filter(node => node.attrs?.['data-plugin'] === '@seekdeep-ai/seekdeep-client-ui-theme').length
}
"#)]
extern "C" {
    fn themeProduceFunction() -> Function;
    fn themeBench(initial_system_dark: bool) -> JsValue;
    fn themeService(bench: &JsValue) -> JsValue;
    fn themeSnapshot(bench: &JsValue) -> JsValue;
    fn themeSet(bench: &JsValue, id: &str);
    fn themeRegister(bench: &JsValue, definition: JsValue) -> Function;
    fn themeOverride(bench: &JsValue, source: &str, tokens: JsValue) -> Function;
    fn themeInspect(bench: &JsValue) -> Array;
    fn themePublishHost(bench: &JsValue, preference: JsValue, revision: f64);
    fn themeFlipMedia(bench: &JsValue);
    fn themeHostWrites(bench: &JsValue) -> Array;
    fn themeEvents(bench: &JsValue) -> Array;
    fn themeEffectLabels(bench: &JsValue) -> Array;
    fn themeDictionary(bench: &JsValue, locale: &str) -> JsValue;
    fn themeDeclareItems(bench: &JsValue);
    fn themeRegistration(bench: &JsValue) -> JsValue;
    fn themeAppearanceState(bench: &JsValue) -> JsValue;
    fn themeRenderAppearance(bench: &JsValue) -> JsValue;
    fn themeFind(node: &JsValue, property: &str, value: &str) -> JsValue;
    fn themeFindAll(node: &JsValue, property: &str, value: &str) -> Array;
    fn themeClick(node: &JsValue) -> JsValue;
    fn themeDispose(bench: &JsValue);
    fn themeMediaListenerCount(bench: &JsValue) -> u32;
    fn themeHostListenerCount(bench: &JsValue) -> u32;
    fn themeStylesheetCount(bench: &JsValue) -> u32;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn actual_runtime_module() -> JsValue {
    seekdeep_client_runtime::install_store_produce(themeProduceFunction());
    let module = Object::new();
    let define = Closure::wrap(Box::new(|declaration: JsValue| {
        seekdeep_client_runtime::define_store(declaration)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    Reflect::set(
        &module,
        &JsValue::from_str("defineStore"),
        &define.into_js_value(),
    )
    .unwrap();
    module.into()
}

fn configure(bench: &JsValue) {
    configure_client_ui_theme(
        property(bench, "React"),
        property(bench, "primitives"),
        actual_runtime_module(),
    )
    .unwrap();
}

fn string(value: &JsValue, name: &str) -> String {
    property(value, name).as_string().unwrap()
}

fn number(value: &JsValue, name: &str) -> f64 {
    property(value, name).as_f64().unwrap()
}

fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = property(value, name).dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args)
}

fn definition(id: &str, scheme: &str, token: Option<(&str, &str)>) -> JsValue {
    let tokens = Object::new();
    if let Some((name, value)) = token {
        Reflect::set(&tokens, &JsValue::from_str(name), &JsValue::from_str(value)).unwrap();
    }
    let output = Object::new();
    Reflect::set(&output, &"id".into(), &id.into()).unwrap();
    Reflect::set(&output, &"colorScheme".into(), &scheme.into()).unwrap();
    Reflect::set(&output, &"tokens".into(), &tokens).unwrap();
    output.into()
}

fn override_rows(rows: &[(&str, &str, &str)]) -> JsValue {
    let output = Object::new();
    for (name, light, dark) in rows {
        let pair = Object::new();
        Reflect::set(&pair, &"light".into(), &(*light).into()).unwrap();
        Reflect::set(&pair, &"dark".into(), &(*dark).into()).unwrap();
        Reflect::set(&output, &(*name).into(), &pair).unwrap();
    }
    output.into()
}

#[wasm_bindgen_test]
fn registry_scope_media_custom_themes_and_overrides_publish_exactly() {
    let bench = themeBench(true);
    configure(&bench);
    assert_eq!(themeStylesheetCount(&bench), 1);
    apply_client_ui_theme(property(&bench, "ctx")).unwrap();
    assert_eq!(
        theme_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["slots", "locale", "connection", "remote", "settingsScope"]
    );
    let initial = themeSnapshot(&bench);
    assert_eq!(string(&initial, "preference"), "system");
    assert_eq!(string(&property(&initial, "active"), "id"), "dark");
    assert!(Object::is(&initial, &themeSnapshot(&bench)));
    assert!(Object::is_frozen(&Object::from(initial.clone())));

    themeSet(&bench, "light");
    assert_eq!(string(&themeSnapshot(&bench), "preference"), "light");
    assert_eq!(themeHostWrites(&bench).length(), 1);
    let before_flip = themeEvents(&bench).length();
    themeFlipMedia(&bench);
    assert_eq!(themeEvents(&bench).length(), before_flip);
    themeSet(&bench, "system");
    themeFlipMedia(&bench);
    assert_eq!(
        string(&property(&themeSnapshot(&bench), "active"), "id"),
        "dark"
    );

    let writes = themeHostWrites(&bench).length();
    themePublishHost(&bench, JsValue::from_str("dark"), 7.0);
    assert_eq!(string(&themeSnapshot(&bench), "preference"), "dark");
    assert_eq!(themeHostWrites(&bench).length(), writes);

    let dispose = themeRegister(
        &bench,
        definition("sepia", "light", Some(("--dsw-alias-bg-base", "red"))),
    );
    themeSet(&bench, "sepia");
    assert_eq!(string(&themeSnapshot(&bench), "preference"), "sepia");
    assert_eq!(themeHostWrites(&bench).length(), writes);
    let active = property(&themeSnapshot(&bench), "active");
    assert_eq!(
        property(&property(&active, "tokens"), "--dsw-alias-bg-base")
            .as_string()
            .as_deref(),
        Some("red")
    );
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(string(&themeSnapshot(&bench), "preference"), "system");
    dispose.call0(&JsValue::UNDEFINED).unwrap();

    let first = themeOverride(
        &bench,
        "first",
        override_rows(&[
            ("--shared", "first-light", "first-dark"),
            ("--first", "only-light", "only-dark"),
        ]),
    );
    let second = themeOverride(
        &bench,
        "second",
        override_rows(&[("--shared", "second-light", "second-dark")]),
    );
    themeSet(&bench, "dark");
    let active_tokens = property(&property(&themeSnapshot(&bench), "active"), "tokens");
    assert_eq!(
        property(&active_tokens, "--shared").as_string().as_deref(),
        Some("second-dark")
    );
    second.call0(&JsValue::UNDEFINED).unwrap();
    let active_tokens = property(&property(&themeSnapshot(&bench), "active"), "tokens");
    assert_eq!(
        property(&active_tokens, "--shared").as_string().as_deref(),
        Some("first-dark")
    );
    first.call0(&JsValue::UNDEFINED).unwrap();
}

#[wasm_bindgen_test]
fn malformed_overrides_inspection_and_exact_generation_guards_are_live() {
    let bench = themeBench(false);
    configure(&bench);
    apply_client_ui_theme(property(&bench, "ctx")).unwrap();
    let bare = Object::new();
    Reflect::set(&bare, &"--bad".into(), &"red".into()).unwrap();
    let error = call(
        &themeService(&bench),
        "overrideTokens",
        &[JsValue::from_str("package"), bare.into()],
    )
    .unwrap_err();
    assert!(string(&error, "message").contains("bare string"));
    assert_eq!(string(&error, "name"), "TypeError");
    for invalid in [JsValue::from_f64(1.0), JsValue::NULL, Object::new().into()] {
        let rows = Object::new();
        Reflect::set(&rows, &"--bad".into(), &invalid).unwrap();
        let error = call(
            &themeService(&bench),
            "overrideTokens",
            &[JsValue::from_str("package"), rows.into()],
        )
        .unwrap_err();
        assert!(string(&error, "message").contains("{ light, dark } pair"));
    }

    let stale = themeOverride(&bench, "package", override_rows(&[("--old", "old", "old")]));
    let current = themeOverride(&bench, "package", override_rows(&[("--new", "new", "new")]));
    let before = themeEvents(&bench).length();
    stale.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(themeEvents(&bench).length(), before);
    current.call0(&JsValue::UNDEFINED).unwrap();
    current.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(themeEvents(&bench).length(), before + 1);

    themeRegister(
        &bench,
        definition("custom", "light", Some(("--registered", "pink"))),
    );
    themeOverride(
        &bench,
        "inspect",
        override_rows(&[("semanticAccent", "pink", "red")]),
    );
    let inspected = themeInspect(&bench);
    let names = inspected
        .iter()
        .map(|row| string(&row, "name"))
        .collect::<Vec<_>>();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    let semantic = inspected
        .iter()
        .find(|row| string(row, "name") == "semanticAccent")
        .unwrap();
    assert!(property(&semantic, "cssVariable").is_undefined());
    Reflect::set(
        &inspected.get(0),
        &"description".into(),
        &"caller mutation".into(),
    )
    .unwrap();
    assert_ne!(
        string(&themeInspect(&bench).get(0), "description"),
        "caller mutation"
    );
}

#[wasm_bindgen_test]
fn apply_is_declaration_aware_projects_the_real_store_and_unwinds_every_effect() {
    let bench = themeBench(false);
    configure(&bench);
    apply_client_ui_theme(property(&bench, "ctx")).unwrap();
    assert!(themeRegistration(&bench).is_undefined());
    assert_eq!(
        themeEffectLabels(&bench)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        [
            "ui-theme: prefers-color-scheme listener",
            "ui-theme: settings scope adoption",
            "ui-theme: settings row dictionaries"
        ]
    );
    assert_eq!(
        property(&themeDictionary(&bench, "zh"), "appearance.title")
            .as_string()
            .as_deref(),
        Some("外观")
    );
    themeDeclareItems(&bench);
    let registration = themeRegistration(&bench);
    let options = property(&registration, "options");
    assert_eq!(string(&options, "name"), "settings.general.item");
    assert_eq!(string(&options, "id"), "appearance");
    assert_eq!(number(&options, "order"), 10.0);
    assert_eq!(string(&options, "locale"), "settings.theme");

    let state = themeAppearanceState(&bench);
    assert_eq!(string(&state, "preference"), "system");
    assert_eq!(number(&state, "revision"), 0.0);
    let tree = themeRenderAppearance(&bench);
    assert_eq!(
        themeFindAll(&tree, "className", "seekdeep-theme-cube").length(),
        2
    );
    let system = themeFind(
        &tree,
        "className",
        "seekdeep-theme-cube seekdeep-theme-selected",
    );
    assert!(!system.is_undefined());
    let light = themeFind(&tree, "className", "seekdeep-theme-cube");
    themeClick(&light);
    assert_eq!(string(&themeAppearanceState(&bench), "preference"), "light");
    let rerendered = themeRenderAppearance(&bench);
    let selected = themeFind(
        &rerendered,
        "className",
        "seekdeep-theme-cube seekdeep-theme-selected",
    );
    assert_eq!(
        Array::from(&property(&selected, "children"))
            .get(1)
            .as_string()
            .as_deref(),
        Some("Light")
    );

    assert_eq!(themeMediaListenerCount(&bench), 1);
    assert_eq!(themeHostListenerCount(&bench), 1);
    themeDispose(&bench);
    assert!(themeService(&bench).is_undefined());
    assert!(themeRegistration(&bench).is_undefined());
    assert!(themeDictionary(&bench, "en").is_undefined());
    assert_eq!(themeMediaListenerCount(&bench), 0);
    assert_eq!(themeHostListenerCount(&bench), 0);
}
