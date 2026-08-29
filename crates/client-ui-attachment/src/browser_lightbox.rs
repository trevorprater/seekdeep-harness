//! Compiled original-image lightbox with focus restoration.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::browser::{
    BrowserDependencies, call_method, class_name, create_element, object, required_function,
    required_property, required_string,
};

pub(crate) fn component(dependencies: &BrowserDependencies) -> JsValue {
    let dependencies = dependencies.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render(&dependencies, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)] // Closed lightbox tree and effect lifecycle stay together.
fn render(dependencies: &BrowserDependencies, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &dependencies.react;
    let source = required_string(props, "src", "ImageLightbox props")?;
    let alt = required_string(props, "alt", "ImageLightbox props")?;
    let labels = required_property(props, "labels", "ImageLightbox props")?;
    let dialog_label = required_string(&labels, "dialog", "ImageLightbox labels")?;
    let close_label = required_string(&labels, "close", "ImageLightbox labels")?;
    let on_close = required_function(props, "onClose", "ImageLightbox props")?;
    let close_ref = use_ref(react, &JsValue::NULL)?;
    let restore_ref = use_ref(react, &JsValue::NULL)?;

    let effect_close_ref = close_ref.clone();
    let effect_restore_ref = restore_ref.clone();
    let effect_close = on_close.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let document = required_property(&js_sys::global(), "document", "global")?;
        let active = Reflect::get(&document, &JsValue::from_str("activeElement"))?;
        let restore = if is_html_element(&active)? {
            active
        } else {
            JsValue::NULL
        };
        set_current(&effect_restore_ref, &restore)?;
        focus_if_present(&current(&effect_close_ref)?)?;
        let close = effect_close.clone();
        let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if Reflect::get(&event, &JsValue::from_str("key"))?
                .as_string()
                .as_deref()
                == Some("Escape")
            {
                close.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("keydown"), keydown.clone().into()],
        )?;
        let restore = effect_restore_ref.clone();
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &window,
                "removeEventListener",
                &[JsValue::from_str("keydown"), keydown.clone().into()],
            )?;
            focus_if_present(&current(&restore)?)
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(on_close.as_ref()),
    )?;

    let mask = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name("ImageLightbox", "mask")),
            ),
            ("aria-hidden", JsValue::TRUE),
            ("onMouseDown", on_close.clone().into()),
        ])?),
        &[],
    )?;
    let image = create_element(
        react,
        &JsValue::from_str("img"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name("ImageLightbox", "image")),
            ),
            ("src", JsValue::from_str(&source)),
            ("alt", JsValue::from_str(&alt)),
        ])?),
        &[],
    )?;
    let icon = create_element(
        react,
        &dependencies.close_outline,
        Some(&object(&[("size", JsValue::from_f64(16.0))])?),
        &[],
    )?;
    let close = create_element(
        react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("ref", close_ref),
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class_name("ImageLightbox", "close")),
            ),
            ("aria-label", JsValue::from_str(&close_label)),
            ("onClick", on_close.into()),
        ])?),
        &[icon],
    )?;
    let backdrop = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name("ImageLightbox", "backdrop")),
            ),
            ("role", JsValue::from_str("dialog")),
            ("aria-modal", JsValue::TRUE),
            ("aria-label", JsValue::from_str(&dialog_label)),
        ])?),
        &[mask, image, close],
    )?;
    let document = required_property(&js_sys::global(), "document", "global")?;
    let body = required_property(&document, "body", "document")?;
    call_method(&dependencies.react_dom, "createPortal", &[backdrop, body])
}

fn is_html_element(value: &JsValue) -> Result<bool, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(false);
    }
    let constructor = required_function(&js_sys::global(), "HTMLElement", "global")?;
    let prototype = required_property(&constructor, "prototype", "HTMLElement")?;
    required_function(&prototype, "isPrototypeOf", "HTMLElement prototype")?
        .call1(&prototype, value)?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("isPrototypeOf returned a non-boolean").into())
}

fn focus_if_present(value: &JsValue) -> Result<(), JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(());
    }
    if let Ok(focus) = Reflect::get(value, &JsValue::from_str("focus"))
        && focus.is_function()
    {
        focus.dyn_into::<Function>()?.call0(value)?;
    }
    Ok(())
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}
