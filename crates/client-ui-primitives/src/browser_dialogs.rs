//! Compiled modal, disclosure, and risk-confirmation components.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{ICON_DEFINITIONS, browser_icons::render_icon, button_component};

const MODAL_CSS: &str = include_str!("../../../packages/client/ui-primitives/src/Modal.module.css");
const DISCLOSURE_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/DisclosureRow.module.css");
const RISK_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/RiskConfirmation.module.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    react_dom: JsValue,
}

/// Configures React modules and dialog-owned styles.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveDialogs)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_dialogs(
    react: JsValue,
    react_dom: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|slot| *slot.borrow_mut() = Some(BrowserModules { react, react_dom }));
    inject_style(
        "Modal",
        MODAL_CSS,
        &[
            "root",
            "mask",
            "dialog",
            "content",
            "header",
            "title",
            "close",
            "description",
            "body",
            "footer",
        ],
    )?;
    inject_style(
        "DisclosureRow",
        DISCLOSURE_CSS,
        &[
            "root",
            "row",
            "leading",
            "iconIdle",
            "chevronHover",
            "title",
        ],
    )?;
    inject_style(
        "RiskConfirmation",
        RISK_CSS,
        &[
            "confirmation",
            "confirmationContent",
            "modalAction",
            "confirmAction",
            "warning",
            "warningIcon",
            "acknowledgement",
        ],
    )
}

/// Returns the compiled `Modal` component.
///
/// # Errors
///
/// Returns missing module configuration.
#[wasm_bindgen(js_name = modalComponent)]
pub fn modal_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_modal(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `DisclosureRow` component.
///
/// # Errors
///
/// Returns missing module configuration.
#[wasm_bindgen(js_name = disclosureRowComponent)]
pub fn disclosure_row_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_disclosure(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `RiskConfirmation` component.
///
/// # Errors
///
/// Returns missing module configuration.
#[wasm_bindgen(js_name = riskConfirmationComponent)]
pub fn risk_confirmation_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_risk(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

#[allow(clippy::too_many_lines)]
fn render_modal(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let open = property_truthy(props, "open")?;
    let on_close = function(props, "onClose")?;
    let title = required_string(props, "title", "Modal props")?;
    let close_label = optional_string(props, "closeLabel")?.unwrap_or_else(|| "Close".to_owned());
    let effect_document = required_property(&js_sys::global(), "document", "global")?;
    let listener_document = effect_document.clone();
    let effect_close = on_close.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open {
            return Ok(JsValue::UNDEFINED);
        }
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
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let keydown = keydown.into_js_value().dyn_into::<Function>()?;
        call_method(
            &listener_document,
            "addEventListener",
            &[JsValue::from_str("keydown"), keydown.clone().into()],
        )?;
        let document = listener_document.clone();
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &document,
                "removeEventListener",
                &[JsValue::from_str("keydown"), keydown.clone().into()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::of2(&JsValue::from_bool(open), on_close.as_ref());
    function(&modules.react, "useEffect")?.call2(
        &modules.react,
        &effect.into_js_value(),
        &dependencies,
    )?;
    if !open {
        return Ok(JsValue::NULL);
    }

    let ui = ReactUi {
        react: modules.react.clone(),
    };
    let class_name = optional_string(props, "className")?;
    let content_class = optional_string(props, "contentClassName")?;
    let description = optional_string(props, "description")?;
    let children = Reflect::get(props, &JsValue::from_str("children"))?;
    let footer = Reflect::get(props, &JsValue::from_str("footer"))?;
    let headless = property_truthy(props, "headless")?;
    let mask = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name_for("Modal", "mask")),
            ),
            ("aria-hidden", JsValue::TRUE),
            ("onClick", on_close.clone().into()),
        ])?),
        &[],
    )?;
    let dialog_children = if headless {
        vec![children]
    } else {
        let heading = ui.tag(
            "h2",
            Some(&class_props(&class_name_for("Modal", "title"))?),
            &[JsValue::from_str(&title)],
        )?;
        let icon = icon_node(&modules.react, "IconCloseOutline16", 14.0, None)?;
        let close = ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name_for("Modal", "close")),
                ),
                ("aria-label", JsValue::from_str(&close_label)),
                ("onClick", on_close.into()),
            ])?),
            &[icon],
        )?;
        let header = ui.tag(
            "div",
            Some(&class_props(&class_name_for("Modal", "header"))?),
            &[heading, close],
        )?;
        let mut content_children = vec![header];
        if let Some(description) = description.filter(|value| !value.is_empty()) {
            content_children.push(ui.tag(
                "p",
                Some(&class_props(&class_name_for("Modal", "description"))?),
                &[JsValue::from_str(&description)],
            )?);
        }
        if !children.is_undefined() {
            content_children.push(ui.tag(
                "div",
                Some(&class_props(&class_name_for("Modal", "body"))?),
                &[children],
            )?);
        }
        let content = ui.tag(
            "div",
            Some(&class_props(&join_classes(
                [Some(class_name_for("Modal", "content")), content_class]
                    .into_iter()
                    .flatten(),
            ))?),
            &content_children,
        )?;
        let mut output = vec![content];
        if !footer.is_undefined() {
            output.push(ui.tag(
                "div",
                Some(&class_props(&class_name_for("Modal", "footer"))?),
                &[footer],
            )?);
        }
        output
    };
    let dialog = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [Some(class_name_for("Modal", "dialog")), class_name]
                        .into_iter()
                        .flatten(),
                )),
            ),
            ("role", JsValue::from_str("dialog")),
            ("aria-modal", JsValue::TRUE),
            ("aria-label", JsValue::from_str(&title)),
        ])?),
        &dialog_children,
    )?;
    let root = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class_name_for("Modal", "root")),
            ),
            ("role", JsValue::from_str("presentation")),
        ])?),
        &[mask, dialog],
    )?;
    let body = required_property(&effect_document, "body", "document")?;
    call_method(&modules.react_dom, "createPortal", &[root, body])
}

#[allow(clippy::too_many_lines)]
fn render_disclosure(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let ui = ReactUi {
        react: modules.react.clone(),
    };
    let icon = required_property(props, "icon", "DisclosureRow props")?;
    let title = required_string(props, "title", "DisclosureRow props")?;
    let open = property_truthy(props, "open")?;
    let expandable = property_truthy(props, "expandable")?;
    let on_toggle = function(props, "onToggle")?;
    let expand_on_row = property_truthy(props, "expandOnRowClick")?;
    let preview = optional_bool(props, "previewChevron")?.unwrap_or(expandable);
    let keep = property_truthy(props, "keepContentWhenOpen")?;
    let collapsed = Reflect::get(props, &JsValue::from_str("collapsedContent"))?;
    let children = Reflect::get(props, &JsValue::from_str("children"))?;
    let row_expands = expandable && expand_on_row;
    let chevron_class = optional_string(props, "chevronClassName")?;
    let leading = if open {
        icon_node(
            &modules.react,
            "IconChevronDownOutline14",
            14.0,
            chevron_class.as_deref(),
        )?
    } else if preview {
        let idle = ui.tag(
            "span",
            Some(&class_props(&class_name_for("DisclosureRow", "iconIdle"))?),
            &[icon],
        )?;
        let hover_class = join_classes(
            [
                chevron_class,
                Some(class_name_for("DisclosureRow", "chevronHover")),
            ]
            .into_iter()
            .flatten(),
        );
        let hover = icon_node(
            &modules.react,
            "IconChevronDownOutline14",
            14.0,
            Some(&hover_class),
        )?;
        ui.fragment(&[idle, hover])?
    } else {
        icon
    };
    let leading_class = join_classes(
        [
            Some(class_name_for("DisclosureRow", "leading")),
            optional_string(props, "leadingClassName")?,
        ]
        .into_iter()
        .flatten(),
    );
    let leading = if expandable && !row_expands {
        let toggle = on_toggle.clone();
        let click = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            call_method(&event, "stopPropagation", &[])?;
            toggle.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(&leading_class)),
                ("aria-expanded", JsValue::from_bool(open)),
                ("onClick", click.into_js_value()),
            ])?),
            &[leading],
        )?
    } else {
        ui.tag("span", Some(&class_props(&leading_class)?), &[leading])?
    };
    let title = ui.tag(
        "span",
        Some(&class_props(&join_classes(
            [
                Some(class_name_for("DisclosureRow", "title")),
                optional_string(props, "titleClassName")?,
            ]
            .into_iter()
            .flatten(),
        ))?),
        &[JsValue::from_str(&title)],
    )?;
    let row_click = optional_js(row_expands, on_toggle.clone().into());
    let keyboard_toggle = on_toggle;
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = Reflect::get(&event, &JsValue::from_str("key"))?.as_string();
        if !matches!(key.as_deref(), Some("Enter" | " ")) {
            return Ok(());
        }
        call_method(&event, "preventDefault", &[])?;
        keyboard_toggle.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let mut row_children = vec![leading, title];
    if keep || !open {
        row_children.push(collapsed);
    }
    let row = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [
                        Some(class_name_for("DisclosureRow", "row")),
                        optional_string(props, "rowClassName")?,
                    ]
                    .into_iter()
                    .flatten(),
                )),
            ),
            ("data-disclosure-row", JsValue::TRUE),
            ("data-expandable", optional_js(row_expands, JsValue::TRUE)),
            (
                "role",
                optional_js(row_expands, JsValue::from_str("button")),
            ),
            ("tabIndex", optional_js(row_expands, JsValue::from_f64(0.0))),
            (
                "aria-expanded",
                optional_js(row_expands, JsValue::from_bool(open)),
            ),
            ("onClick", row_click),
            (
                "onKeyDown",
                optional_js(row_expands, keydown.into_js_value()),
            ),
        ])?),
        &row_children,
    )?;
    let mut root_children = vec![row];
    if open {
        root_children.push(children);
    }
    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [
                        Some(class_name_for("DisclosureRow", "root")),
                        optional_string(props, "className")?,
                    ]
                    .into_iter()
                    .flatten(),
                )),
            ),
            ("data-open", optional_js(open, JsValue::TRUE)),
        ])?),
        &root_children,
    )
}

#[allow(clippy::too_many_lines)]
fn render_risk(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let ui = ReactUi {
        react: modules.react.clone(),
    };
    let open = property_truthy(props, "open")?;
    let title = required_string(props, "title", "RiskConfirmation props")?;
    let description = required_string(props, "description", "RiskConfirmation props")?;
    let acknowledgement = required_string(props, "acknowledgeLabel", "RiskConfirmation props")?;
    let cancel = required_string(props, "cancelLabel", "RiskConfirmation props")?;
    let confirm = required_string(props, "confirmLabel", "RiskConfirmation props")?;
    let acknowledged = property_truthy(props, "acknowledged")?;
    let disabled = property_truthy(props, "disabled")?;
    let on_change = function(props, "onAcknowledgedChange")?;
    let on_cancel = function(props, "onCancel")?;
    let on_confirm = function(props, "onConfirm")?;
    let warning = icon_node(
        &modules.react,
        "IconWarningOutline16",
        18.0,
        Some(&class_name_for("RiskConfirmation", "warningIcon")),
    )?;
    let description = ui.tag("p", None, &[JsValue::from_str(&description)])?;
    let warning = ui.tag(
        "div",
        Some(&class_props(&class_name_for(
            "RiskConfirmation",
            "warning",
        ))?),
        &[warning, description],
    )?;
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let current = required_property(&event, "currentTarget", "change event")?;
        let checked = Reflect::get(&current, &JsValue::from_str("checked"))?;
        on_change.call1(&JsValue::UNDEFINED, &checked)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let checkbox = ui.tag(
        "input",
        Some(&object(&[
            ("type", JsValue::from_str("checkbox")),
            ("checked", JsValue::from_bool(acknowledged)),
            ("disabled", JsValue::from_bool(disabled)),
            ("autoFocus", JsValue::TRUE),
            ("onChange", change.into_js_value()),
        ])?),
        &[],
    )?;
    let acknowledgement_text = ui.tag("span", None, &[JsValue::from_str(&acknowledgement)])?;
    let acknowledgement = ui.tag(
        "label",
        Some(&class_props(&class_name_for(
            "RiskConfirmation",
            "acknowledgement",
        ))?),
        &[checkbox, acknowledgement_text],
    )?;
    let button = button_component()?;
    let cancel_button = ui.element(
        &button,
        Some(&object(&[
            ("variant", JsValue::from_str("outline")),
            (
                "className",
                JsValue::from_str(&class_name_for("RiskConfirmation", "modalAction")),
            ),
            ("onClick", on_cancel.clone().into()),
        ])?),
        &[JsValue::from_str(&cancel)],
    )?;
    let confirm_button = ui.element(
        &button,
        Some(&object(&[
            ("variant", JsValue::from_str("primary")),
            (
                "className",
                JsValue::from_str(&class_name_for("RiskConfirmation", "confirmAction")),
            ),
            ("disabled", JsValue::from_bool(disabled || !acknowledged)),
            ("onClick", on_confirm.into()),
        ])?),
        &[JsValue::from_str(&confirm)],
    )?;
    let footer = ui.fragment(&[cancel_button, confirm_button])?;
    let modal = modal_component()?;
    ui.element(
        &modal,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", on_cancel.into()),
            ("title", JsValue::from_str(&title)),
            (
                "className",
                JsValue::from_str(&class_name_for("RiskConfirmation", "confirmation")),
            ),
            (
                "contentClassName",
                JsValue::from_str(&class_name_for("RiskConfirmation", "confirmationContent")),
            ),
            ("footer", footer),
        ])?),
        &[warning, acknowledgement],
    )
}

fn icon_node(
    react: &JsValue,
    name: &str,
    size: f64,
    class_name: Option<&str>,
) -> Result<JsValue, JsValue> {
    let definition = ICON_DEFINITIONS
        .iter()
        .find(|definition| definition.name == name)
        .copied()
        .ok_or_else(|| js_error(&format!("missing icon {name}")))?;
    render_icon(
        react,
        definition,
        &object(&[
            ("size", JsValue::from_f64(size)),
            (
                "className",
                class_name.map_or(JsValue::UNDEFINED, JsValue::from_str),
            ),
        ])?
        .into(),
    )
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|slot| {
        slot.borrow()
            .clone()
            .ok_or_else(|| js_error("client-ui-primitives dialog module was not configured"))
    })
}

fn inject_style(component: &str, css: &str, locals: &[&str]) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = format!("@seekdeep-ai/seekdeep-client-ui-primitives/{component}.module.css");
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
    {
        let selector = format!("style[data-plugin-css=\"{tag}\"]");
        if !query
            .call1(&document, &JsValue::from_str(&selector))?
            .is_null()
        {
            return Ok(());
        }
    }
    let mut rewritten = css.to_owned();
    let mut locals = locals.to_vec();
    locals.sort_by_key(|local| std::cmp::Reverse(local.len()));
    for local in locals {
        rewritten = rewritten.replace(
            &format!(".{local}"),
            &format!(".{}", class_name_for(component, local)),
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
        &JsValue::from_str(&rewritten),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn class_name_for(component: &str, local: &str) -> String {
    format!(
        "seekdeep-primitive-{}-{local}",
        component
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>()
    )
}

fn join_classes(values: impl IntoIterator<Item = String>) -> String {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn optional_js(condition: bool, value: JsValue) -> JsValue {
    if condition { value } else { JsValue::UNDEFINED }
}

fn property_truthy(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?.is_truthy())
}

fn optional_bool(value: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_bool()
            .map(Some)
            .ok_or_else(|| js_error(&format!("{key} must be a boolean")))
    }
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_string()
            .map(Some)
            .ok_or_else(|| js_error(&format!("{key} must be a string")))
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_error(&format!("{owner} {key:?} must be a string")))
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_error(&format!(
            "{owner} omitted required property {key:?}"
        )))
    } else {
        Ok(property)
    }
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required_property(value, key, "object")?.dyn_into::<Function>()
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(&format!("client-ui-primitives: {message}")).into()
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
}

impl ReactUi {
    fn element(
        &self,
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
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }

    fn tag(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        self.element(&JsValue::from_str(name), props, children)
    }

    fn fragment(&self, children: &[JsValue]) -> Result<JsValue, JsValue> {
        self.element(
            &required_property(&self.react, "Fragment", "React")?,
            None,
            children,
        )
    }
}
