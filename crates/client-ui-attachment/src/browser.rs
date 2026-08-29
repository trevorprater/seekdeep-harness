//! Browser module configuration and public component faces.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_primitives::icon_components;
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};

use crate::{browser_lightbox, browser_message};
const LIGHTBOX_CSS: &str =
    include_str!("../../../packages/client/ui-attachment/src/ImageLightbox.module.css");
const MESSAGE_CSS: &str =
    include_str!("../../../packages/client/ui-attachment/src/MessageImage.module.css");

thread_local! {
    static CONFIGURED: RefCell<Option<Configured>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserDependencies {
    pub(crate) react: JsValue,
    pub(crate) react_dom: JsValue,
    pub(crate) fragment: JsValue,
    pub(crate) close_outline: JsValue,
}

#[derive(Clone)]
struct Configured {
    lightbox: JsValue,
    message_image: JsValue,
    image_gallery: JsValue,
}

/// Configures all attachment atoms over React, `ReactDOM`, and compiled icons.
///
/// # Errors
///
/// Returns on missing dependencies or stylesheet injection failures.
#[wasm_bindgen(js_name = configureClientUiAttachment)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_attachment(react: JsValue, react_dom: JsValue) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useCallback",
        "useEffect",
        "useLayoutEffect",
        "useMemo",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    required_function(&react_dom, "createPortal", "ReactDOM")?;
    let fragment = required_property(&react, "Fragment", "React")?;
    let icons = icon_components()?;
    let dependencies = BrowserDependencies {
        react,
        react_dom,
        fragment,
        close_outline: required_property(&icons, "IconCloseOutline16", "icons")?,
    };
    inject_all_styles()?;
    let lightbox = browser_lightbox::component(&dependencies);
    let message_image = browser_message::message_component(&dependencies, &lightbox);
    let image_gallery = browser_message::gallery_component(&dependencies, &message_image);
    CONFIGURED.with(|configured| {
        *configured.borrow_mut() = Some(Configured {
            lightbox,
            message_image,
            image_gallery,
        });
    });
    Ok(())
}

macro_rules! component_face {
    ($function:ident, $js_name:literal, $field:ident, $docs:literal) => {
        #[doc = $docs]
        ///
        /// # Errors
        ///
        /// Returns before configuration.
        #[wasm_bindgen(js_name = $js_name)]
        pub fn $function() -> Result<JsValue, JsValue> {
            configured().map(|configured| configured.$field)
        }
    };
}

component_face!(
    image_lightbox_component,
    "imageLightboxComponent",
    lightbox,
    "Returns compiled `ImageLightbox`."
);
component_face!(
    message_image_component,
    "messageImageComponent",
    message_image,
    "Returns compiled `MessageImage`."
);
component_face!(
    image_gallery_component,
    "imageGalleryComponent",
    image_gallery,
    "Returns compiled `ImageGallery`."
);

fn configured() -> Result<Configured, JsValue> {
    CONFIGURED.with(|configured| {
        configured
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-attachment was not configured").into())
    })
}

fn inject_all_styles() -> Result<(), JsValue> {
    inject_style(
        "ImageLightbox",
        LIGHTBOX_CSS,
        &["backdrop", "mask", "image", "close"],
    )?;
    inject_style(
        "MessageImage",
        MESSAGE_CSS,
        &["gallery", "frame", "loading", "error"],
    )
}

fn inject_style(name: &str, source: &str, classes: &[&str]) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = format!("@seekdeep-ai/seekdeep-client-ui-attachment/{name}.module.css");
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let mut css = source.to_owned();
    for class in classes {
        css = css.replace(
            &format!(".{class}"),
            &format!(".seekdeep-attachment-{}-{class}", kebab_name(name)),
        );
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin-css"),
            JsValue::from_str(&tag),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&css),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

pub(crate) fn class_name(module: &str, class: &str) -> String {
    format!("seekdeep-attachment-{}-{class}", kebab_name(module))
}

fn kebab_name(name: &str) -> String {
    let mut output = String::new();
    for (index, character) in name.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            output.push('-');
        }
        output.push(character.to_ascii_lowercase());
    }
    output
}

pub(crate) fn required_property(
    value: &JsValue,
    key: &str,
    owner: &str,
) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

pub(crate) fn required_function(
    value: &JsValue,
    key: &str,
    owner: &str,
) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

pub(crate) fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

pub(crate) fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

pub(crate) fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

pub(crate) fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

pub(crate) fn call_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
