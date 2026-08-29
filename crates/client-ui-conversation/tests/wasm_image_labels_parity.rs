//! Live WASM coverage for image copy and attachment-label factories.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    attachment_error_text_browser, attachment_rail_labels_browser, drop_overlay_labels_browser,
    image_size_text_browser, lightbox_labels_browser, message_image_labels_browser,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let calls = []
export function installImageLabelsBench() { calls = [] }
export function imageLabelsObject(entries) { return Object.fromEntries(entries) }
export function makeImageTranslate() {
  return (key, vars) => {
    calls.push({ key, vars })
    if (vars === undefined) return key
    return `${key}:${Object.entries(vars).map(([name, value]) => `${name}=${value}`).join(',')}`
  }
}
export function imageLabelCalls() { return calls }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installImageLabelsBench)]
    fn install_image_labels_bench();
    #[wasm_bindgen(js_name = imageLabelsObject)]
    fn image_labels_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = makeImageTranslate)]
    fn make_image_translate() -> Function;
    #[wasm_bindgen(js_name = imageLabelCalls)]
    fn image_label_calls() -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    image_labels_object(&array).unchecked_into()
}

#[wasm_bindgen_test]
fn size_text_uses_integer_or_one_decimal_javascript_formatting() {
    assert_eq!(
        image_size_text_browser(10.0 * 1024.0 * 1024.0).unwrap(),
        "10MB"
    );
    assert_eq!(
        image_size_text_browser(2.5 * 1024.0 * 1024.0).unwrap(),
        "2.5MB"
    );
    assert_eq!(
        image_size_text_browser(2.54 * 1024.0 * 1024.0).unwrap(),
        "2.5MB"
    );
    assert_eq!(
        image_size_text_browser(f64::INFINITY).unwrap(),
        "InfinityMB"
    );
}

#[wasm_bindgen_test]
fn attachment_rejection_routes_all_specific_and_limit_aware_copy() {
    install_image_labels_bench();
    let translate = make_image_translate();
    let limits = object(&[
        ("maxImagesPerMessage", JsValue::from_f64(3.0)),
        ("maxImageBytes", JsValue::from_f64(2.5 * 1024.0 * 1024.0)),
        (
            "maxMessageImageBytes",
            JsValue::from_f64(10.0 * 1024.0 * 1024.0),
        ),
    ]);
    for (reason, expected) in [
        ("MODEL_DOES_NOT_SUPPORT_IMAGES", "image.modelUnsupported"),
        ("SUBAGENT_IMAGE_UNSUPPORTED", "image.subagentUnsupported"),
        ("IMAGE_TOO_MANY_PIXELS", "image.tooManyPixels"),
        ("INVALID_IMAGE", "image.unsupportedType"),
        ("IMAGE_TYPE_MISMATCH", "image.unsupportedType"),
    ] {
        assert_eq!(
            attachment_error_text_browser(
                translate.clone(),
                reason.to_owned(),
                limits.clone().into(),
            )
            .unwrap()
            .as_string()
            .as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        attachment_error_text_browser(
            translate.clone(),
            "TOO_MANY_IMAGES".to_owned(),
            limits.clone().into(),
        )
        .unwrap()
        .as_string()
        .as_deref(),
        Some("image.tooMany:count=3")
    );
    assert_eq!(
        attachment_error_text_browser(
            translate.clone(),
            "IMAGE_TOO_LARGE".to_owned(),
            limits.clone().into(),
        )
        .unwrap()
        .as_string()
        .as_deref(),
        Some("image.fileTooLarge:size=2.5MB")
    );
    assert_eq!(
        attachment_error_text_browser(
            translate.clone(),
            "IMAGES_TOO_LARGE".to_owned(),
            limits.into(),
        )
        .unwrap()
        .as_string()
        .as_deref(),
        Some("image.totalTooLarge:size=10MB")
    );
    assert_eq!(
        attachment_error_text_browser(translate, "FUTURE_REASON".to_owned(), JsValue::UNDEFINED,)
            .unwrap()
            .as_string()
            .as_deref(),
        Some("image.sendFailed:reason=FUTURE_REASON")
    );
}

#[wasm_bindgen_test]
fn message_and_lightbox_factories_pin_exact_keys_and_deferred_named_callback() {
    install_image_labels_bench();
    let translate = make_image_translate();
    let labels = message_image_labels_browser(translate.clone()).unwrap();
    assert_eq!(
        property(&labels, "image").as_string().as_deref(),
        Some("image.label")
    );
    assert_eq!(
        property(&labels, "open").as_string().as_deref(),
        Some("image.openOriginal")
    );
    assert_eq!(
        property(&labels, "loading").as_string().as_deref(),
        Some("image.loading")
    );
    assert_eq!(
        property(&labels, "loadFailed").as_string().as_deref(),
        Some("image.loadFailed")
    );
    let lightbox = property(&labels, "lightbox");
    assert_eq!(
        property(&lightbox, "dialog").as_string().as_deref(),
        Some("image.preview")
    );
    assert_eq!(
        property(&lightbox, "close").as_string().as_deref(),
        Some("image.closePreview")
    );
    let before = image_label_calls().length();
    let named = property(&labels, "openNamed")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("diagram"))
        .unwrap();
    assert_eq!(
        named.as_string().as_deref(),
        Some("image.openOriginalLabel:label=diagram")
    );
    assert_eq!(image_label_calls().length(), before + 1);
    let direct = lightbox_labels_browser(translate).unwrap();
    assert_eq!(
        property(&direct, "dialog").as_string().as_deref(),
        Some("image.preview")
    );
}

#[wasm_bindgen_test]
fn overlay_and_rail_factories_preserve_blocked_and_optional_desc_shapes() {
    install_image_labels_bench();
    let translate = make_image_translate();
    let blocked =
        drop_overlay_labels_browser(translate.clone(), false, JsValue::UNDEFINED).unwrap();
    assert_eq!(
        property(&blocked, "title").as_string().as_deref(),
        Some("image.dropBlocked")
    );
    assert!(
        Reflect::get(&blocked, &JsValue::from_str("desc"))
            .unwrap()
            .is_undefined()
    );
    assert!(!Reflect::has(&blocked, &JsValue::from_str("desc")).unwrap());
    let accepting =
        drop_overlay_labels_browser(translate.clone(), true, JsValue::UNDEFINED).unwrap();
    assert!(Reflect::has(&accepting, &JsValue::from_str("desc")).unwrap());
    assert!(property(&accepting, "desc").is_undefined());
    let limited = drop_overlay_labels_browser(
        translate.clone(),
        true,
        object(&[
            ("count", JsValue::from_f64(4.0)),
            ("size", JsValue::from_str("5MB")),
        ])
        .into(),
    )
    .unwrap();
    assert_eq!(
        property(&limited, "desc").as_string().as_deref(),
        Some("image.dropDesc:count=4,size=5MB")
    );
    let rail = attachment_rail_labels_browser(translate).unwrap();
    assert_eq!(
        property(&rail, "group").as_string().as_deref(),
        Some("image.pending")
    );
    assert_eq!(
        property(&rail, "open").as_string().as_deref(),
        Some("image.openOriginal")
    );
    assert_eq!(
        property(&rail, "scrollLeft").as_string().as_deref(),
        Some("image.scrollLeft")
    );
    assert_eq!(
        property(&rail, "scrollRight").as_string().as_deref(),
        Some("image.scrollRight")
    );
}
