//! Live WASM coverage for empty-session hero surfaces.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_empty_hero, hero_glow_component, hero_shell_component,
    workspace_chip_component, workspace_label_browser,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let clicks = 0
export function installHeroBench() {
  clicks = 0
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useId() { return ':hero:1:' },
  }
  const uiPrimitives = {
    FishLogo: 'FishLogo', IconChevronDownOutline14: 'IconChevronDownOutline14',
    IconFolderClose16: 'IconFolderClose16', IconFolderOpen16: 'IconFolderOpen16',
  }
  return { React, uiPrimitives }
}
export function heroObject(entries) { return Object.fromEntries(entries) }
export function makeHeroTranslate() {
  const copy = { 'hero.chooseWorkspace': 'Choose workspace', 'hero.headline': 'Into the Unknown', 'hero.preview': 'Preview' }
  return key => copy[key] ?? key
}
export function makeHeroClick() { return () => { clicks += 1 } }
export function heroClicks() { return clicks }
export function heroRender(component, props) { return component(props) }
export function heroMarker() { return { kind: 'overlay', props: {}, children: ['overlay'] } }
export function heroFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = heroFindKind(child, kind); if (found) return found }
  return undefined
}
export function heroFindClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.props?.className === className) return value
  for (const child of value.children ?? []) { const found = heroFindClass(child, className); if (found) return found }
  return undefined
}
export function heroText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(heroText).join('')
  return heroText(value.children)
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installHeroBench)]
    fn install_hero_bench() -> JsValue;
    #[wasm_bindgen(js_name = heroObject)]
    fn hero_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = makeHeroTranslate)]
    fn make_hero_translate() -> Function;
    #[wasm_bindgen(js_name = makeHeroClick)]
    fn make_hero_click() -> Function;
    #[wasm_bindgen(js_name = heroClicks)]
    fn hero_clicks() -> u32;
    #[wasm_bindgen(js_name = heroRender)]
    fn hero_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = heroMarker)]
    fn hero_marker() -> JsValue;
    #[wasm_bindgen(js_name = heroFindKind)]
    fn hero_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = heroFindClass)]
    fn hero_find_class(value: &JsValue, class_name: &str) -> JsValue;
    #[wasm_bindgen(js_name = heroText)]
    fn hero_text(value: &JsValue) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn child(value: &JsValue, index: u32) -> JsValue {
    property(value, "children")
        .unchecked_into::<Array>()
        .get(index)
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    hero_object(&array).unchecked_into()
}

fn setup() -> (JsValue, JsValue, JsValue) {
    let bench = install_hero_bench();
    configure_client_ui_conversation_empty_hero(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    (
        workspace_chip_component().unwrap(),
        hero_glow_component().unwrap(),
        hero_shell_component().unwrap(),
    )
}

#[wasm_bindgen_test]
fn workspace_label_accepts_both_separators_and_echoes_separator_only_paths() {
    assert_eq!(workspace_label_browser("/work/project/"), "project");
    assert_eq!(workspace_label_browser("C:\\work\\project\\"), "project");
    assert_eq!(workspace_label_browser("mixed/path\\name"), "name");
    assert_eq!(workspace_label_browser("///"), "///");
    assert_eq!(workspace_label_browser(""), "");
}

#[wasm_bindgen_test]
fn workspace_chip_pins_placeholder_label_icons_ref_toggle_and_open_echo() {
    let (component, _, _) = setup();
    let button_ref = Object::new();
    let on_click = make_hero_click();
    let placeholder = hero_render(
        &component,
        object(&[
            ("buttonRef", button_ref.clone().into()),
            ("onClick", on_click.into()),
            ("t", make_hero_translate().into()),
        ])
        .as_ref(),
    );
    assert_eq!(
        property(&property(&placeholder, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("Choose workspace")
    );
    assert_eq!(
        property(&property(&placeholder, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    assert!(Object::is(
        &property(&property(&placeholder, "props"), "ref"),
        button_ref.as_ref()
    ));
    assert_eq!(
        property(&child(&placeholder, 0), "kind")
            .as_string()
            .as_deref(),
        Some("IconFolderClose16")
    );
    assert_eq!(hero_text(&child(&placeholder, 1)), "Choose workspace");
    property(&property(&placeholder, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(hero_clicks(), 1);

    let labeled = hero_render(
        &component,
        object(&[
            ("label", JsValue::from_str("project")),
            ("menuOpen", JsValue::TRUE),
            ("t", make_hero_translate().into()),
        ])
        .as_ref(),
    );
    assert_eq!(
        property(&child(&labeled, 0), "kind").as_string().as_deref(),
        Some("IconFolderOpen16")
    );
    assert_eq!(hero_text(&child(&labeled, 1)), "project");
    assert_eq!(
        property(&property(&labeled, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(&property(&child(&labeled, 2), "props"), "size").as_f64(),
        Some(12.0)
    );
}

#[wasm_bindgen_test]
fn hero_glow_sanitizes_stable_filter_id_and_wires_exact_svg_reference() {
    let (_, component, _) = setup();
    let tree = hero_render(
        &component,
        object(&[("className", JsValue::from_str("positioned-glow"))]).as_ref(),
    );
    assert_eq!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .as_deref(),
        Some("positioned-glow")
    );
    assert_eq!(
        property(&property(&tree, "props"), "viewBox")
            .as_string()
            .as_deref(),
        Some("0 0 1051 468")
    );
    let filter = hero_find_kind(&tree, "filter");
    assert_eq!(
        property(&property(&filter, "props"), "id")
            .as_string()
            .as_deref(),
        Some("empty-glow-hero1")
    );
    let group = hero_find_kind(&tree, "g");
    assert_eq!(
        property(&property(&group, "props"), "filter")
            .as_string()
            .as_deref(),
        Some("url(#empty-glow-hero1)")
    );
    let ellipse = hero_find_kind(&tree, "ellipse");
    assert_eq!(
        property(&property(&ellipse, "props"), "fill")
            .as_string()
            .as_deref(),
        Some("#6187D8")
    );
}

#[wasm_bindgen_test]
fn hero_shell_forwards_locale_fish_geometry_and_overlay_children() {
    let (_, _, component) = setup();
    let overlay = hero_marker();
    let tree = hero_render(
        &component,
        object(&[
            ("t", make_hero_translate().into()),
            ("children", overlay.clone()),
        ])
        .as_ref(),
    );
    assert!(hero_text(&tree).contains("Into the Unknown"));
    assert!(hero_text(&tree).contains("Preview"));
    let fish = hero_find_kind(&tree, "FishLogo");
    assert_eq!(
        property(&property(&fish, "props"), "size").as_f64(),
        Some(34.0)
    );
    assert!(Object::is(&child(&tree, 1), &overlay));
    let body = hero_find_class(&tree, "seekdeep-conversation-hero-body");
    assert_eq!(
        property(&body, "children")
            .unchecked_into::<Array>()
            .length(),
        0
    );
}
