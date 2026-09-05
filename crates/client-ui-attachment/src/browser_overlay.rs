//! Compiled full-viewport attachment drop invitation.

use js_sys::Reflect;
use wasm_bindgen::{JsValue, closure::Closure};

use crate::browser::{
    BrowserDependencies, call_method, class_name, class_props, create_element, object,
    required_property, required_string,
};

const ENABLED_INNER: &str =
    include_str!("../../../packages/client/ui-attachment/assets/upload-enabled.svg.html");
const DISABLED_INNER: &str =
    include_str!("../../../packages/client/ui-attachment/assets/upload-disabled.svg.html");

pub(crate) fn component(dependencies: &BrowserDependencies) -> JsValue {
    let dependencies = dependencies.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render(&dependencies, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

fn render(dependencies: &BrowserDependencies, props: &JsValue) -> Result<JsValue, JsValue> {
    let disabled = required_property(props, "disabled", "DropOverlay props")?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("DropOverlay disabled must be a boolean"))?;
    let labels = required_property(props, "labels", "DropOverlay props")?;
    let title = required_string(&labels, "title", "DropOverlay labels")?;
    let description = Reflect::get(&labels, &JsValue::from_str("desc"))?;
    let inner = object(&[(
        "__html",
        JsValue::from_str(if disabled {
            DISABLED_INNER.trim_end()
        } else {
            ENABLED_INNER.trim_end()
        }),
    )])?;
    let illustration = create_element(
        &dependencies.react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("width", JsValue::from_f64(115.0)),
            ("height", JsValue::from_f64(84.0)),
            ("viewBox", JsValue::from_str("0 0 115 84")),
            ("fill", JsValue::from_str("none")),
            ("xmlns", JsValue::from_str("http://www.w3.org/2000/svg")),
            ("dangerouslySetInnerHTML", inner.into()),
        ])?),
        &[],
    )?;
    let illustration = create_element(
        &dependencies.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name("DropOverlay", "illustration")),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[illustration],
    )?;
    let title = create_element(
        &dependencies.react,
        &JsValue::from_str("div"),
        Some(&class_props(&class_name("DropOverlay", "title"))?),
        &[JsValue::from_str(&title)],
    )?;
    let mut content = vec![illustration, title];
    if !disabled && !description.is_undefined() {
        content.push(create_element(
            &dependencies.react,
            &JsValue::from_str("div"),
            Some(&class_props(&class_name("DropOverlay", "desc"))?),
            &[description],
        )?);
    }
    let wrap = create_element(
        &dependencies.react,
        &JsValue::from_str("div"),
        Some(&class_props(&class_name("DropOverlay", "wrap"))?),
        &content,
    )?;
    let mask = create_element(
        &dependencies.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name("DropOverlay", "mask")),
            ),
            ("role", JsValue::from_str("status")),
        ])?),
        &[wrap],
    )?;
    let document = required_property(&js_sys::global(), "document", "global")?;
    let body = required_property(&document, "body", "document")?;
    call_method(&dependencies.react_dom, "createPortal", &[mask, body])
}
