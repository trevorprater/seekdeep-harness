//! Compiled image copy and attachment-label factories.

use js_sys::{Array, Function, Number, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

/// Formats bytes as user-facing mebibytes with JavaScript number semantics.
///
/// # Errors
///
/// Returns if JavaScript number formatting fails.
#[wasm_bindgen(js_name = imageSizeText)]
pub fn image_size_text_browser(bytes: f64) -> Result<String, JsValue> {
    let mebibytes = bytes / (1024.0 * 1024.0);
    let number = JsValue::from_f64(mebibytes);
    let text = if Number::is_integer(&number) {
        number_string(mebibytes)
    } else {
        Number::from(mebibytes)
            .to_fixed(1)?
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("Number.toFixed() returned a non-string").into())
    }?;
    Ok(format!("{text}MB"))
}

/// Resolves product copy for one Host attachment rejection.
///
/// # Errors
///
/// Returns when the translate face throws or supplied limits are malformed.
#[wasm_bindgen(js_name = attachmentErrorText)]
#[allow(clippy::needless_pass_by_value)]
pub fn attachment_error_text_browser(
    translate: Function,
    reason: String,
    limits: JsValue,
) -> Result<JsValue, JsValue> {
    let known_limits = !limits.is_undefined();
    match reason.as_str() {
        "MODEL_DOES_NOT_SUPPORT_IMAGES" => translate_key(&translate, "image.modelUnsupported"),
        "SUBAGENT_IMAGE_UNSUPPORTED" => translate_key(&translate, "image.subagentUnsupported"),
        "IMAGE_TOO_MANY_PIXELS" => translate_key(&translate, "image.tooManyPixels"),
        "INVALID_IMAGE" | "IMAGE_TYPE_MISMATCH" => {
            translate_key(&translate, "image.unsupportedType")
        }
        "TOO_MANY_IMAGES" if known_limits => translate_params(
            &translate,
            "image.tooMany",
            &[(
                "count",
                required_property(&limits, "maxImagesPerMessage", "image limits")?,
            )],
        ),
        "IMAGE_TOO_LARGE" if known_limits => translate_params(
            &translate,
            "image.fileTooLarge",
            &[(
                "size",
                JsValue::from_str(&image_size_text_browser(javascript_number(
                    &required_property(&limits, "maxImageBytes", "image limits")?,
                )?)?),
            )],
        ),
        "IMAGES_TOO_LARGE" if known_limits => translate_params(
            &translate,
            "image.totalTooLarge",
            &[(
                "size",
                JsValue::from_str(&image_size_text_browser(javascript_number(
                    &required_property(&limits, "maxMessageImageBytes", "image limits")?,
                )?)?),
            )],
        ),
        _ => translate_params(
            &translate,
            "image.sendFailed",
            &[("reason", JsValue::from_str(&reason))],
        ),
    }
}

/// Resolves original-image lightbox strings.
///
/// # Errors
///
/// Returns when the translate face throws.
#[wasm_bindgen(js_name = lightboxLabels)]
#[allow(clippy::needless_pass_by_value)]
pub fn lightbox_labels_browser(translate: Function) -> Result<JsValue, JsValue> {
    Ok(lightbox_labels(&translate)?.into())
}

/// Resolves chat-history image strings and callbacks.
///
/// # Errors
///
/// Returns when the translate face throws.
#[wasm_bindgen(js_name = messageImageLabels)]
#[allow(clippy::needless_pass_by_value)]
pub fn message_image_labels_browser(translate: Function) -> Result<JsValue, JsValue> {
    message_image_labels(&translate)
}

/// Resolves full-page drop-overlay strings.
///
/// # Errors
///
/// Returns when the translate face throws or supplied limits are malformed.
#[wasm_bindgen(js_name = dropOverlayLabels)]
#[allow(clippy::needless_pass_by_value)]
pub fn drop_overlay_labels_browser(
    translate: Function,
    accepting: bool,
    limits: JsValue,
) -> Result<JsValue, JsValue> {
    if !accepting {
        return Ok(object(&[("title", translate_key(&translate, "image.dropBlocked")?)])?.into());
    }
    let title = translate_key(&translate, "image.dropTitle")?;
    let description = if limits.is_undefined() {
        JsValue::UNDEFINED
    } else {
        translate_params(
            &translate,
            "image.dropDesc",
            &[
                (
                    "count",
                    required_property(&limits, "count", "drop-overlay limits")?,
                ),
                (
                    "size",
                    required_property(&limits, "size", "drop-overlay limits")?,
                ),
            ],
        )?
    };
    Ok(object(&[("title", title), ("desc", description)])?.into())
}

/// Resolves composer draft-image rail strings.
///
/// # Errors
///
/// Returns when the translate face throws.
#[wasm_bindgen(js_name = attachmentRailLabels)]
#[allow(clippy::needless_pass_by_value)]
pub fn attachment_rail_labels_browser(translate: Function) -> Result<JsValue, JsValue> {
    Ok(object(&[
        ("group", translate_key(&translate, "image.pending")?),
        ("open", translate_key(&translate, "image.openOriginal")?),
        ("scrollLeft", translate_key(&translate, "image.scrollLeft")?),
        (
            "scrollRight",
            translate_key(&translate, "image.scrollRight")?,
        ),
    ])?
    .into())
}

pub(crate) fn message_image_labels(translate: &Function) -> Result<JsValue, JsValue> {
    let named_translate = translate.clone();
    let open_named = Closure::wrap(Box::new(move |label: JsValue| {
        translate_params(
            &named_translate,
            "image.openOriginalLabel",
            &[("label", label)],
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value();
    Ok(object(&[
        ("image", translate_key(translate, "image.label")?),
        ("open", translate_key(translate, "image.openOriginal")?),
        ("openNamed", open_named),
        ("loading", translate_key(translate, "image.loading")?),
        ("loadFailed", translate_key(translate, "image.loadFailed")?),
        ("lightbox", lightbox_labels(translate)?.into()),
    ])?
    .into())
}

fn lightbox_labels(translate: &Function) -> Result<Object, JsValue> {
    object(&[
        ("dialog", translate_key(translate, "image.preview")?),
        ("close", translate_key(translate, "image.closePreview")?),
    ])
}

fn translate_key(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn translate_params(
    translate: &Function,
    key: &str,
    parameters: &[(&str, JsValue)],
) -> Result<JsValue, JsValue> {
    translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(&JsValue::from_str(key), object(parameters)?.as_ref()),
    )
}

fn number_string(value: f64) -> Result<String, JsValue> {
    Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned a non-string").into())
}

fn javascript_number(value: &JsValue) -> Result<f64, JsValue> {
    Reflect::get(&js_sys::global(), &JsValue::from_str("Number"))?
        .dyn_into::<Function>()?
        .call1(&JsValue::UNDEFINED, value)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("Number() returned a non-number").into())
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}
