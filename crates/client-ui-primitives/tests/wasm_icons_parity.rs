//! Live JavaScript coverage for the generated Rust/WASM SVG component catalog.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Object, Reflect};
use seekdeep_client_ui_primitives::{
    ICON_DEFINITIONS, configure_client_ui_primitive_icons, icon_components,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_test::wasm_bindgen_test;

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> JsValue {
    let output = Object::new();
    for (key, value) in entries {
        Reflect::set(&output, &JsValue::from_str(key), value).unwrap();
    }
    output.into()
}

fn render(component: &JsValue, props: &JsValue) -> JsValue {
    component
        .clone()
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, props)
        .unwrap()
}

fn component(components: &Object, name: &str) -> JsValue {
    Reflect::get(components, &JsValue::from_str(name)).unwrap()
}

#[wasm_bindgen_test]
fn complete_icon_catalog_renders_trusted_current_color_svg_components() {
    let react = Object::new();
    let create = Closure::wrap(Box::new(|kind: JsValue, props: JsValue| {
        let output = Object::new();
        Reflect::set(&output, &"kind".into(), &kind).unwrap();
        Reflect::set(&output, &"props".into(), &props).unwrap();
        output
    }) as Box<dyn FnMut(JsValue, JsValue) -> Object>);
    Reflect::set(&react, &"createElement".into(), &create.into_js_value()).unwrap();
    configure_client_ui_primitive_icons(react.into());
    let components = icon_components().unwrap();
    assert_eq!(Object::keys(&components).length(), 72);
    assert_eq!(
        ICON_DEFINITIONS
            .iter()
            .filter(|definition| definition.name.starts_with("Icon"))
            .count(),
        70
    );
    for definition in ICON_DEFINITIONS {
        let node = render(
            &component(&components, definition.name),
            &Object::new().into(),
        );
        assert_eq!(property(&node, "kind").as_string().as_deref(), Some("svg"));
        let props = property(&node, "props");
        assert_eq!(
            property(&props, "viewBox").as_string().as_deref(),
            Some(definition.view_box),
            "{}",
            definition.name
        );
        let html = property(&property(&props, "dangerouslySetInnerHTML"), "__html")
            .as_string()
            .unwrap();
        assert!(html.contains("currentColor"), "{}", definition.name);
        assert!(
            !has_hex_palette(&html),
            "{} contains a hardcoded palette",
            definition.name
        );
    }
}

#[wasm_bindgen_test]
fn size_class_ratio_brand_accessibility_and_goal_reuse_match_source_contracts() {
    let react = Object::new();
    let create = Closure::wrap(Box::new(|kind: JsValue, props: JsValue| {
        let output = Object::new();
        Reflect::set(&output, &"kind".into(), &kind).unwrap();
        Reflect::set(&output, &"props".into(), &props).unwrap();
        output
    }) as Box<dyn FnMut(JsValue, JsValue) -> Object>);
    Reflect::set(&react, &"createElement".into(), &create.into_js_value()).unwrap();
    configure_client_ui_primitive_icons(react.into());
    let components = icon_components().unwrap();

    let send = render(
        &component(&components, "IconSendOutline16"),
        &props(&[
            ("size", JsValue::from_f64(20.0)),
            ("className", JsValue::from_str("x")),
        ]),
    );
    let send = property(&send, "props");
    assert_eq!(property(&send, "width").as_f64(), Some(20.0));
    assert_eq!(property(&send, "height").as_f64(), Some(20.0));
    assert_eq!(
        property(&send, "className").as_string().as_deref(),
        Some("x")
    );

    for (name, size) in [
        ("IconApiOutline14", 14.0),
        ("IconFolderClose16", 16.0),
        ("IconArchiveOutline20", 20.0),
    ] {
        let node = render(&component(&components, name), &Object::new().into());
        assert_eq!(
            property(&property(&node, "props"), "width").as_f64(),
            Some(size)
        );
    }
    let tree = render(
        &component(&components, "IconTreeCorner8x10"),
        &props(&[("size", JsValue::from_f64(20.0))]),
    );
    let tree = property(&tree, "props");
    assert_eq!(property(&tree, "width").as_f64(), Some(16.0));
    assert_eq!(property(&tree, "height").as_f64(), Some(20.0));

    let fish = render(&component(&components, "FishLogo"), &Object::new().into());
    let fish = property(&fish, "props");
    assert_eq!(property(&fish, "width").as_f64(), Some(24.0));
    assert!((property(&fish, "height").as_f64().unwrap() - 17.658).abs() < 0.01);
    assert_eq!(property(&fish, "aria-hidden").as_bool(), Some(true));
    let brand = render(
        &component(&components, "BrandWordmark"),
        &Object::new().into(),
    );
    let brand = property(&brand, "props");
    assert_eq!(property(&brand, "width").as_f64(), Some(182.0));
    assert_eq!(property(&brand, "height").as_f64(), Some(24.0));
    assert_eq!(property(&brand, "aria-hidden").as_bool(), Some(true));
    let brand_html = property(&property(&brand, "dangerouslySetInnerHTML"), "__html")
        .as_string()
        .unwrap();
    assert!(brand_html.contains(">seekdeep</text>"));
    assert!(brand_html.contains(">HARNESS</text>"));
    assert!(!brand_html.contains("DeepSeek"));
    assert!(!brand_html.contains("deepseek"));
    assert!(!brand_html.contains("dsh-wordmark"));

    let goal = render(
        &component(&components, "IconGoalOutline16"),
        &Object::new().into(),
    );
    let html = property(
        &property(&property(&goal, "props"), "dangerouslySetInnerHTML"),
        "__html",
    )
    .as_string()
    .unwrap();
    assert!(!html.contains(" id="));
    assert!(!html.contains("clip-path"));
}

fn has_hex_palette(value: &str) -> bool {
    let bytes = value.as_bytes();
    for index in 0..bytes.len().saturating_sub(4) {
        if bytes[index] != b'#' {
            continue;
        }
        let count = bytes[index + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        if matches!(count, 3 | 4 | 6 | 8) {
            return true;
        }
    }
    false
}
