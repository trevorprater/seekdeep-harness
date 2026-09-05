//! Compiled React atoms, state indicators, banners, and portal surfaces.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

const BUTTON_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/Button.module.css");
const PILL_CSS: &str = include_str!("../../../packages/client/ui-primitives/src/Pill.module.css");
const INPUT_CSS: &str = include_str!("../../../packages/client/ui-primitives/src/Input.module.css");
const STATE_DOT_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/StateDot.module.css");
const CONNECTION_BANNER_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/ConnectionBanner.module.css");
const ONBOARDING_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/OnboardingSurface.module.css");
const TOAST_CSS: &str = include_str!("../../../packages/client/ui-primitives/src/Toast.module.css");

const TOAST_HOLD_MS: u32 = 3_000;
const TOAST_FADE_MS: u32 = 1_000;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    react_dom: JsValue,
}

/// Configures React modules and installs package-owned atom styles.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveAtoms)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_atoms(
    react: JsValue,
    react_dom: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|slot| *slot.borrow_mut() = Some(BrowserModules { react, react_dom }));
    for (name, css, classes) in [
        (
            "Button",
            BUTTON_CSS,
            &[
                "button", "primary", "ghost", "outline", "toolbar", "md", "sm", "icon",
            ][..],
        ),
        ("Pill", PILL_CSS, &["pill", "interactive", "active"]),
        ("Input", INPUT_CSS, &["wrap", "icon", "input"]),
        ("StateDot", STATE_DOT_CSS, &["dot", "matrix", "cell"]),
        ("ConnectionBanner", CONNECTION_BANNER_CSS, &["banner"]),
        (
            "OnboardingSurface",
            ONBOARDING_CSS,
            &["onboardingOverlay", "onboardingMask", "onboardingStage"],
        ),
        ("Toast", TOAST_CSS, &["toast", "icon", "text"]),
    ] {
        inject_style(name, css, classes)?;
    }
    Ok(())
}

/// Returns the compiled `Button` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = buttonComponent)]
pub fn button_component() -> Result<JsValue, JsValue> {
    let ui = configured_ui()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_button(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

/// Returns the compiled `Pill` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = pillComponent)]
pub fn pill_component() -> Result<JsValue, JsValue> {
    let ui = configured_ui()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_pill(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

/// Returns the compiled `Input` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = inputComponent)]
pub fn input_component() -> Result<JsValue, JsValue> {
    let ui = configured_ui()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_input(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

/// Returns the compiled `StateDot` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = stateDotComponent)]
pub fn state_dot_component() -> Result<JsValue, JsValue> {
    let ui = configured_ui()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_state_dot(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `ConnectionBanner` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = connectionBannerComponent)]
pub fn connection_banner_component() -> Result<JsValue, JsValue> {
    let ui = configured_ui()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_connection_banner(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `OnboardingSurface` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = onboardingSurfaceComponent)]
pub fn onboarding_surface_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_onboarding(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `Toast` component.
///
/// # Errors
///
/// Returns an error when the browser modules have not been configured.
#[wasm_bindgen(js_name = toastComponent)]
pub fn toast_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_toast(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

fn render_button(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let variant = optional_string(props, "variant")?.unwrap_or_else(|| "ghost".to_owned());
    let size = optional_string(props, "size")?.unwrap_or_else(|| "md".to_owned());
    let icon = Reflect::get(props, &JsValue::from_str("icon"))?;
    let children = Reflect::get(props, &JsValue::from_str("children"))?;
    let class_name = optional_string(props, "className")?;
    let native = rest_props(props, &["variant", "size", "icon", "className", "children"])?;
    if Reflect::get(&native, &JsValue::from_str("type"))?.is_undefined() {
        set(&native, "type", &JsValue::from_str("button"))?;
    }
    let variant = matches!(
        variant.as_str(),
        "primary" | "ghost" | "outline" | "toolbar"
    )
    .then(|| atom_class("Button", &variant));
    let size = matches!(size.as_str(), "md" | "sm").then(|| atom_class("Button", &size));
    set(
        &native,
        "className",
        &JsValue::from_str(&classes(
            [
                Some(atom_class("Button", "button")),
                variant,
                size,
                class_name,
            ]
            .into_iter()
            .flatten(),
        )),
    )?;
    let mut content = Vec::new();
    if !icon.is_null() && !icon.is_undefined() {
        content.push(ui.tag(
            "span",
            Some(&class_props(&atom_class("Button", "icon"))?),
            &[icon],
        )?);
    }
    if !children.is_undefined() {
        content.push(children);
    }
    ui.tag("button", Some(&native), &content)
}

fn render_pill(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let active = property_truthy(props, "active")?;
    let class_name = optional_string(props, "className")?;
    let children = Reflect::get(props, &JsValue::from_str("children"))?;
    let on_click = Reflect::get(props, &JsValue::from_str("onClick"))?;
    let base = atom_class("Pill", "pill");
    if !on_click.is_truthy() {
        return ui.tag(
            "span",
            Some(&class_props(&classes(
                [
                    Some(base),
                    active.then(|| atom_class("Pill", "active")),
                    class_name,
                ]
                .into_iter()
                .flatten(),
            ))?),
            &[children],
        );
    }
    let native = rest_props(props, &["active", "className", "children", "onClick"])?;
    if Reflect::get(&native, &JsValue::from_str("type"))?.is_undefined() {
        set(&native, "type", &JsValue::from_str("button"))?;
    }
    set(&native, "onClick", &on_click)?;
    set(
        &native,
        "className",
        &JsValue::from_str(&classes(
            [
                Some(base),
                Some(atom_class("Pill", "interactive")),
                active.then(|| atom_class("Pill", "active")),
                class_name,
            ]
            .into_iter()
            .flatten(),
        )),
    )?;
    ui.tag("button", Some(&native), &[children])
}

fn render_input(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let icon = Reflect::get(props, &JsValue::from_str("icon"))?;
    let class_name = optional_string(props, "className")?;
    let native = rest_props(props, &["icon", "className"])?;
    set(
        &native,
        "className",
        &JsValue::from_str(&atom_class("Input", "input")),
    )?;
    let mut children = Vec::new();
    if !icon.is_null() && !icon.is_undefined() {
        children.push(ui.tag(
            "span",
            Some(&class_props(&atom_class("Input", "icon"))?),
            &[icon],
        )?);
    }
    children.push(ui.tag("input", Some(&native), &[])?);
    ui.tag(
        "span",
        Some(&class_props(&classes(
            [Some(atom_class("Input", "wrap")), class_name]
                .into_iter()
                .flatten(),
        ))?),
        &children,
    )
}

fn render_state_dot(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let state = required_string(props, "state", "StateDot props")?;
    let size = optional_number(props, "size")?.unwrap_or(10.0);
    let class_name = optional_string(props, "className")?;
    if state == "ongoing" {
        let mut cells = Vec::new();
        for (index, (x, y)) in [
            (0, 0),
            (4, 0),
            (8, 0),
            (8, 4),
            (8, 8),
            (4, 8),
            (0, 8),
            (0, 4),
        ]
        .into_iter()
        .enumerate()
        {
            let delay = (i32::try_from(index).expect("eight cells") - 8) * 125;
            cells.push(
                ui.tag(
                    "rect",
                    Some(&object(&[
                        ("key", JsValue::from_str(&format!("{x}-{y}"))),
                        (
                            "className",
                            JsValue::from_str(&atom_class("StateDot", "cell")),
                        ),
                        ("x", JsValue::from_f64(f64::from(x))),
                        ("y", JsValue::from_f64(f64::from(y))),
                        ("width", JsValue::from_str("2")),
                        ("height", JsValue::from_str("2")),
                        (
                            "style",
                            object(&[(
                                "animationDelay",
                                JsValue::from_str(&format!("{delay}ms")),
                            )])?
                            .into(),
                        ),
                    ])?),
                    &[],
                )?,
            );
        }
        return ui.tag(
            "svg",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&classes(
                        [Some(atom_class("StateDot", "matrix")), class_name]
                            .into_iter()
                            .flatten(),
                    )),
                ),
                ("data-state", JsValue::from_str("ongoing")),
                ("width", JsValue::from_f64(size)),
                ("height", JsValue::from_f64(size)),
                ("viewBox", JsValue::from_str("0 0 10 10")),
                ("shapeRendering", JsValue::from_str("crispEdges")),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &cells,
        );
    }
    ui.tag(
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&classes(
                    [Some(atom_class("StateDot", "dot")), class_name]
                        .into_iter()
                        .flatten(),
                )),
            ),
            ("data-state", JsValue::from_str(&state)),
            (
                "style",
                object(&[
                    ("width", JsValue::from_f64(size)),
                    ("height", JsValue::from_f64(size)),
                ])?
                .into(),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )
}

fn render_connection_banner(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    if !property_truthy(props, "reconnecting")? {
        return Ok(JsValue::NULL);
    }
    let label =
        optional_string(props, "label")?.unwrap_or_else(|| "连接已断开，正在重连…".to_owned());
    ui.tag(
        "div",
        Some(&class_props(&atom_class("ConnectionBanner", "banner"))?),
        &[JsValue::from_str(&label)],
    )
}

fn render_onboarding(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let document = required_property(&js_sys::global(), "document", "global")?;
    let effect_document = document.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let root = call_method(
            &effect_document,
            "getElementById",
            &[JsValue::from_str("root")],
        )?;
        if root.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        Reflect::set(&root, &JsValue::from_str("inert"), &JsValue::TRUE)?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = Reflect::set(&root, &JsValue::from_str("inert"), &JsValue::FALSE);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    function(&modules.react, "useEffect")?.call2(
        &modules.react,
        &effect.into_js_value(),
        &Array::new(),
    )?;
    let ui = ReactUi {
        react: modules.react.clone(),
    };
    let children = Reflect::get(props, &JsValue::from_str("children"))?;
    let mask = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&atom_class("OnboardingSurface", "onboardingMask")),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )?;
    let stage = ui.tag(
        "div",
        Some(&class_props(&atom_class(
            "OnboardingSurface",
            "onboardingStage",
        ))?),
        &[children],
    )?;
    let overlay = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&atom_class("OnboardingSurface", "onboardingOverlay")),
            ),
            ("role", JsValue::from_str("presentation")),
        ])?),
        &[mask, stage],
    )?;
    let body = required_property(&document, "body", "document")?;
    call_method(&modules.react_dom, "createPortal", &[overlay, body])
}

#[allow(clippy::too_many_lines)]
fn render_toast(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let text = required_string(props, "text", "Toast props")?;
    let icon = Reflect::get(props, &JsValue::from_str("icon"))?;
    let anchor = Reflect::get(props, &JsValue::from_str("anchor"))?;
    let on_done = function(props, "onDone")?;
    let timer_done = on_done.clone();
    let timer_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let global = js_sys::global();
        let callback = timer_done.clone();
        let handle = function(&global, "setTimeout")?.call2(
            &global,
            callback.as_ref(),
            &JsValue::from_f64(f64::from(TOAST_HOLD_MS + TOAST_FADE_MS)),
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = function(&js_sys::global(), "clearTimeout")
                .and_then(|clear| clear.call1(&js_sys::global(), &handle));
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    function(&modules.react, "useEffect")?.call2(
        &modules.react,
        &timer_effect.into_js_value(),
        &Array::of1(on_done.as_ref()),
    )?;

    let (left, set_left) = use_state(&modules.react, &JsValue::NULL)?;
    let layout_anchor = anchor.clone();
    let layout = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if layout_anchor.is_null() || layout_anchor.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let measure_anchor = layout_anchor.clone();
        let measure_setter = set_left.clone();
        let measure = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let rect = call_method(&measure_anchor, "getBoundingClientRect", &[])?;
            let left = required_number(&rect, "left", "DOMRect")?;
            let width = required_number(&rect, "width", "DOMRect")?;
            set_state(&measure_setter, &JsValue::from_f64(left + width / 2.0))
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let measure = measure.into_js_value().dyn_into::<Function>()?;
        measure.call0(&JsValue::UNDEFINED)?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("resize"), measure.clone().into()],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &window,
                "removeEventListener",
                &[JsValue::from_str("resize"), measure.clone().into()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    function(&modules.react, "useLayoutEffect")?.call2(
        &modules.react,
        &layout.into_js_value(),
        &Array::of1(&anchor),
    )?;

    let ui = ReactUi {
        react: modules.react.clone(),
    };
    let mut children = Vec::new();
    if !icon.is_undefined() {
        children.push(ui.tag(
            "span",
            Some(&object(&[
                ("className", JsValue::from_str(&atom_class("Toast", "icon"))),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[icon],
        )?);
    }
    children.push(ui.tag(
        "span",
        Some(&class_props(&atom_class("Toast", "text"))?),
        &[JsValue::from_str(&text)],
    )?);
    let style = left
        .as_f64()
        .map(|left| object(&[("left", JsValue::from_f64(left))]))
        .transpose()?;
    let toast = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&atom_class("Toast", "toast")),
            ),
            ("role", JsValue::from_str("alert")),
            ("style", style.map_or(JsValue::UNDEFINED, Into::into)),
        ])?),
        &children,
    )?;
    let document = required_property(&js_sys::global(), "document", "global")?;
    let body = required_property(&document, "body", "document")?;
    call_method(&modules.react_dom, "createPortal", &[toast, body])
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|slot| {
        slot.borrow()
            .clone()
            .ok_or_else(|| js_error("client-ui-primitives atom module was not configured"))
    })
}

fn configured_ui() -> Result<ReactUi, JsValue> {
    Ok(ReactUi {
        react: configured_modules()?.react,
    })
}

fn inject_style(component: &str, css: &str, locals: &[&str]) -> Result<(), JsValue> {
    let global = js_sys::global();
    let document = Reflect::get(&global, &JsValue::from_str("document"))?;
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
            &format!(".{}", atom_class(component, local)),
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
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-primitives"),
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

fn atom_class(component: &str, local: &str) -> String {
    format!(
        "seekdeep-primitive-{}-{local}",
        component
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>()
    )
}

fn rest_props(source: &JsValue, excluded: &[&str]) -> Result<Object, JsValue> {
    let output = Object::new();
    for key in Object::keys(&Object::from(source.clone())).iter() {
        let key_text = key.as_string().unwrap_or_default();
        if excluded.contains(&key_text.as_str()) {
            continue;
        }
        Reflect::set(&output, &key, &Reflect::get(source, &key)?)?;
    }
    Ok(output)
}

fn classes(values: impl IntoIterator<Item = String>) -> String {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn property_truthy(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?.is_truthy())
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

fn optional_number(value: &JsValue, key: &str) -> Result<Option<f64>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_f64()
            .map(Some)
            .ok_or_else(|| js_error(&format!("{key} must be a number")))
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_error(&format!("{owner} {key:?} must be a string")))
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_error(&format!("{owner} {key:?} must be a number")))
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
        set(&object, key, value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into::<Function>()?))
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(&format!("client-ui-primitives: {message}")).into()
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
}

impl ReactUi {
    fn tag(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        let arguments = Array::new();
        arguments.push(&JsValue::from_str(name));
        arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            arguments.push(child);
        }
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }
}
