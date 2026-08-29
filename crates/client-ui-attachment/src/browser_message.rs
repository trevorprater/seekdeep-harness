//! Compiled message image, retry lifecycle, and gallery grouping.

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::browser::{
    BrowserDependencies, call_method, class_name, class_props, create_element, object,
    required_function, required_property, required_string,
};

pub(crate) fn message_component(dependencies: &BrowserDependencies, lightbox: &JsValue) -> JsValue {
    let dependencies = dependencies.clone();
    let lightbox = lightbox.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_message(&dependencies, &lightbox, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

pub(crate) fn gallery_component(dependencies: &BrowserDependencies, message: &JsValue) -> JsValue {
    let dependencies = dependencies.clone();
    let message = message.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_gallery(&dependencies, &message, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_message(
    dependencies: &BrowserDependencies,
    lightbox: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let react = &dependencies.react;
    let attachment = required_property(props, "attachment", "MessageImage props")?;
    let load = required_function(props, "load", "MessageImage props")?;
    let variant = required_string(props, "variant", "MessageImage props")?;
    let labels = required_property(props, "labels", "MessageImage props")?;
    let fallback_label = required_string(&labels, "image", "MessageImage labels")?;
    let open_title = required_string(&labels, "open", "MessageImage labels")?;
    let open_named = required_function(&labels, "openNamed", "MessageImage labels")?;
    let loading_label = required_string(&labels, "loading", "MessageImage labels")?;
    let failed_label = required_string(&labels, "loadFailed", "MessageImage labels")?;
    let lightbox_labels = required_property(&labels, "lightbox", "MessageImage labels")?;
    let (source, set_source) = use_state(react, &JsValue::NULL)?;
    let (error_value, set_error) = use_state(react, &JsValue::FALSE)?;
    let error = error_value.as_bool().unwrap_or(false);
    let (open_value, set_open) = use_state(react, &JsValue::FALSE)?;
    let open = open_value.as_bool().unwrap_or(false);
    let (attempt, set_attempt) = use_state(react, &JsValue::from_f64(0.0))?;

    let request_setter = set_attempt;
    let request = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater = Closure::wrap(Box::new(move |value: JsValue| {
            JsValue::from_f64(value.as_f64().unwrap_or(0.0) + 1.0)
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        request_setter
            .call1(&JsValue::UNDEFINED, &updater.into_js_value())
            .map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let request = use_callback(react, &request.into_js_value(), &Array::new())?;
    let close_setter = set_open.clone();
    let close = Closure::wrap(Box::new(move || {
        let _ = close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let close = use_callback(react, &close.into_js_value(), &Array::new())?;

    let fit_attachment = attachment.clone();
    let fit_variant = variant.clone();
    let fit_factory = Closure::wrap(Box::new(move || {
        if fit_variant == "single" {
            single_fit(&fit_attachment)
        } else {
            Ok(JsValue::UNDEFINED)
        }
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let fit_dependencies = Array::of2(&attachment, &JsValue::from_str(&variant));
    let fit = required_function(react, "useMemo", "React")?.call2(
        react,
        &fit_factory.into_js_value(),
        &fit_dependencies,
    )?;

    let effect_attachment = attachment.clone();
    let effect_load = load.clone();
    let effect_error = set_error;
    let effect_source = set_source;
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let live = Rc::new(Cell::new(true));
        effect_error.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        effect_source.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        let pending = effect_load.call1(&JsValue::UNDEFINED, &effect_attachment)?;
        let success_live = live.clone();
        let success_setter = effect_source.clone();
        let success = Closure::wrap(Box::new(move |url: JsValue| -> Result<(), JsValue> {
            if success_live.get() {
                success_setter.call1(&JsValue::UNDEFINED, &url)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let failure_live = live.clone();
        let failure_setter = effect_error.clone();
        let failure = Closure::wrap(Box::new(move |_error: JsValue| -> Result<(), JsValue> {
            if failure_live.get() {
                failure_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let chained = call_method(
            Promise::resolve(&pending).as_ref(),
            "then",
            &[success.into_js_value()],
        )?;
        call_method(&chained, "catch", &[failure.into_js_value()])?;
        Ok(Closure::wrap(Box::new(move || live.set(false)) as Box<dyn FnMut()>).into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let effect_dependencies = Array::of3(&attachment, load.as_ref(), &attempt);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &effect_dependencies,
    )?;

    let name = Reflect::get(&attachment, &JsValue::from_str("name"))?;
    let label = name.as_string().unwrap_or(fallback_label);
    if error {
        return create_element(
            react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name("MessageImage", "error")),
                ),
                ("data-variant", JsValue::from_str(&variant)),
                ("onClick", request.into()),
            ])?),
            &[JsValue::from_str(&failed_label)],
        );
    }
    let open_source = source.clone();
    let open_setter = set_open;
    let on_open = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !open_source.is_null() {
            open_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let frame_style = if fit.is_undefined() {
        JsValue::UNDEFINED
    } else {
        object(&[
            ("width", required_property(&fit, "width", "image fit")?),
            ("height", required_property(&fit, "height", "image fit")?),
        ])?
        .into()
    };
    let accessible = open_named.call1(&JsValue::UNDEFINED, &JsValue::from_str(&label))?;
    let content = if source.is_null() {
        create_element(
            react,
            &JsValue::from_str("span"),
            Some(&class_props(&class_name("MessageImage", "loading"))?),
            &[JsValue::from_str(&loading_label)],
        )?
    } else {
        let image_style = if fit.is_undefined() {
            JsValue::UNDEFINED
        } else {
            object(&[(
                "objectPosition",
                required_property(&fit, "objectPosition", "image fit")?,
            )])?
            .into()
        };
        create_element(
            react,
            &JsValue::from_str("img"),
            Some(&object(&[
                ("src", source.clone()),
                ("alt", JsValue::from_str(&label)),
                ("style", image_style),
            ])?),
            &[],
        )?
    };
    let frame = create_element(
        react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class_name("MessageImage", "frame")),
            ),
            ("data-variant", JsValue::from_str(&variant)),
            ("style", frame_style),
            ("title", JsValue::from_str(&open_title)),
            ("aria-label", accessible),
            ("onClick", on_open.into_js_value()),
        ])?),
        &[content],
    )?;
    let mut children = vec![frame];
    if open && !source.is_null() {
        children.push(create_element(
            react,
            lightbox,
            Some(&object(&[
                ("src", source),
                ("alt", JsValue::from_str(&label)),
                ("labels", lightbox_labels),
                ("onClose", close.into()),
            ])?),
            &[],
        )?);
    }
    create_element(react, &dependencies.fragment, None, &children)
}

fn render_gallery(
    dependencies: &BrowserDependencies,
    message: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let images = required_property(props, "images", "ImageGallery props")?;
    if !Array::is_array(&images) {
        return Err(js_sys::TypeError::new("ImageGallery images must be an array").into());
    }
    let images = Array::from(&images);
    if images.length() == 0 {
        return Ok(JsValue::NULL);
    }
    let load = required_function(props, "load", "ImageGallery props")?;
    let align = required_string(props, "align", "ImageGallery props")?;
    let labels = required_property(props, "labels", "ImageGallery props")?;
    let variant = if images.length() == 1 {
        "single"
    } else {
        "tile"
    };
    let mut children = Vec::new();
    for (index, image) in images.iter().enumerate() {
        let attachment = required_property(&image, "attachment", "ImageGallery item")?;
        let attachment_id = required_string(&attachment, "attachmentId", "image attachment")?;
        children.push(create_element(
            &dependencies.react,
            message,
            Some(&object(&[
                (
                    "key",
                    JsValue::from_str(&format!("{attachment_id}:{index}")),
                ),
                ("attachment", attachment),
                ("load", load.clone().into()),
                ("variant", JsValue::from_str(variant)),
                ("labels", labels.clone()),
            ])?),
            &[],
        )?);
    }
    create_element(
        &dependencies.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name("MessageImage", "gallery")),
            ),
            ("data-align", JsValue::from_str(&align)),
        ])?),
        &children,
    )
}

fn single_fit(attachment: &JsValue) -> Result<JsValue, JsValue> {
    let width = required_number(attachment, "width", "image attachment")?;
    let height = required_number(attachment, "height", "image attachment")?;
    let natural = width / height;
    let ratio = natural.clamp(0.25, 4.0);
    let (box_width, box_height) = if ratio >= 1.0 {
        (240.0, 240.0 / ratio)
    } else {
        (240.0 * ratio, 240.0)
    };
    let scale = 1.0_f64.min(width / box_width).min(height / box_height);
    Ok(object(&[
        (
            "width",
            JsValue::from_f64((box_width * scale).round().max(1.0)),
        ),
        (
            "height",
            JsValue::from_f64((box_height * scale).round().max(1.0)),
        ),
        (
            "objectPosition",
            JsValue::from_str(if natural < 0.25 {
                "center top"
            } else if natural > 4.0 {
                "left center"
            } else {
                "center"
            }),
        ),
    ])?
    .into())
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_callback(react: &JsValue, callback: &JsValue, deps: &Array) -> Result<Function, JsValue> {
    required_function(react, "useCallback", "React")?
        .call2(react, callback, deps)?
        .dyn_into()
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}
