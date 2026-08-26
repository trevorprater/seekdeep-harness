//! Compiled hover/focus tooltip with ref forwarding and viewport fitting.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

const TOOLTIP_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/Tooltip.module.css");
const EDGE_MARGIN: f64 = 12.0;

thread_local! {
    static REACT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Configures React and installs the tooltip stylesheet.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveTooltip)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_tooltip(react: JsValue) -> Result<(), JsValue> {
    REACT.with(|slot| *slot.borrow_mut() = Some(react));
    inject_style()
}

/// Returns the compiled `Tooltip` component.
///
/// # Errors
///
/// Returns missing React configuration.
#[wasm_bindgen(js_name = tooltipComponent)]
pub fn tooltip_component() -> Result<JsValue, JsValue> {
    let react = configured_react()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_tooltip(&react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

#[allow(clippy::too_many_lines)]
fn render_tooltip(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let label = required_property(props, "label", "Tooltip props")?;
    let side = optional_string(props, "side")?.unwrap_or_else(|| "right".to_owned());
    let delay = optional_number(props, "delayMs")?.unwrap_or(0.0);
    let disabled = property_truthy(props, "disabled")?;
    let max_width = optional_number(props, "maxWidth")?;
    let child = required_property(props, "children", "Tooltip props")?;
    let anchor = use_ref(react, &JsValue::NULL)?;
    let child_ref = Reflect::get(&child, &JsValue::from_str("ref"))?;
    let merged_anchor = anchor.clone();
    let forwarded_ref = child_ref.clone();
    let merged = Closure::wrap(Box::new(move |element: JsValue| -> Result<(), JsValue> {
        Reflect::set(&merged_anchor, &JsValue::from_str("current"), &element)?;
        if forwarded_ref.is_function() {
            forwarded_ref
                .clone()
                .dyn_into::<Function>()?
                .call1(&JsValue::UNDEFINED, &element)?;
        } else if !forwarded_ref.is_null() && !forwarded_ref.is_undefined() {
            Reflect::set(&forwarded_ref, &JsValue::from_str("current"), &element)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let merged = use_callback(react, &merged.into_js_value(), &Array::of1(&child_ref))?;
    let (position, set_position) = use_state(react, &JsValue::NULL)?;
    let (placement, set_placement) = use_state(react, &JsValue::from_str(&side))?;
    let placement_text = placement.as_string().unwrap_or_else(|| side.clone());
    let bubble = use_ref(react, &JsValue::NULL)?;
    let resolved_label = if position.is_null() {
        JsValue::NULL
    } else if label.is_function() {
        label
            .clone()
            .dyn_into::<Function>()?
            .call0(&JsValue::UNDEFINED)?
    } else {
        label.clone()
    };
    let (x, top, bottom) = if position.is_null() {
        (0.0, 0.0, 0.0)
    } else {
        (
            required_number(&position, "x", "tooltip position")?,
            required_number(&position, "top", "tooltip position")?,
            required_number(&position, "bottom", "tooltip position")?,
        )
    };
    let y = if position.is_null() {
        0.0
    } else if placement_text == "right" {
        top + (bottom - top) / 2.0
    } else if placement_text == "top" {
        top - 8.0
    } else {
        bottom + 8.0
    };

    let layout_position = position.clone();
    let layout_bubble = bubble.clone();
    let layout_side = side.clone();
    let layout_placement = placement_text.clone();
    let layout_set_placement = set_placement.clone();
    let layout = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if layout_position.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let fit_position = layout_position.clone();
        let fit_bubble = layout_bubble.clone();
        let fit_side = layout_side.clone();
        let fit_placement = layout_placement.clone();
        let fit_setter = layout_set_placement.clone();
        let fit = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let element = Reflect::get(&fit_bubble, &JsValue::from_str("current"))?;
            if element.is_null() {
                return Ok(());
            }
            let style = required_property(&element, "style", "tooltip bubble")?;
            let base_x = required_number(&fit_position, "x", "tooltip position")?;
            Reflect::set(
                &style,
                &JsValue::from_str("left"),
                &JsValue::from_str(&format!("{base_x}px")),
            )?;
            let bounds = call_method(&element, "getBoundingClientRect", &[])?;
            let right = required_number(&bounds, "right", "DOMRect")?;
            let left = required_number(&bounds, "left", "DOMRect")?;
            let height = required_number(&bounds, "height", "DOMRect")?;
            let window = required_property(&js_sys::global(), "window", "global")?;
            let inner_width = required_number(&window, "innerWidth", "window")?;
            let inner_height = required_number(&window, "innerHeight", "window")?;
            let mut dx = 0.0;
            if right > inner_width - EDGE_MARGIN {
                dx = inner_width - EDGE_MARGIN - right;
            }
            if left + dx < EDGE_MARGIN {
                dx = EDGE_MARGIN - left;
            }
            Reflect::set(
                &style,
                &JsValue::from_str("left"),
                &JsValue::from_str(&format!("{}px", base_x + dx)),
            )?;
            if fit_side == "right" {
                return Ok(());
            }
            let position_top = required_number(&fit_position, "top", "tooltip position")?;
            let position_bottom = required_number(&fit_position, "bottom", "tooltip position")?;
            let fits_below = position_bottom + 8.0 + height <= inner_height - EDGE_MARGIN;
            let fits_above = position_top - 8.0 - height >= EDGE_MARGIN;
            if fit_placement == "bottom" && !fits_below && fits_above {
                set_state(&fit_setter, &JsValue::from_str("top"))?;
            }
            if fit_placement == "top" && !fits_above && fits_below {
                set_state(&fit_setter, &JsValue::from_str("bottom"))?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let fit = fit.into_js_value().dyn_into::<Function>()?;
        fit.call0(&JsValue::UNDEFINED)?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("resize"), fit.clone().into()],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &window,
                "removeEventListener",
                &[JsValue::from_str("resize"), fit.clone().into()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let layout_dependencies = Array::new();
    layout_dependencies.push(&placement);
    layout_dependencies.push(&position);
    layout_dependencies.push(&resolved_label);
    layout_dependencies.push(&JsValue::from_str(&side));
    function(react, "useLayoutEffect")?.call2(
        react,
        &layout.into_js_value(),
        &layout_dependencies,
    )?;

    let timer = use_ref(react, &JsValue::NULL)?;
    let triggers = use_ref(
        react,
        &object(&[("hover", JsValue::FALSE), ("focus", JsValue::FALSE)])?.into(),
    )?;
    let cancel_timer = timer.clone();
    let cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let current = Reflect::get(&cancel_timer, &JsValue::from_str("current"))?;
        if current.is_null() {
            return Ok(());
        }
        let global = js_sys::global();
        function(&global, "clearTimeout")?.call1(&global, &current)?;
        Reflect::set(&cancel_timer, &JsValue::from_str("current"), &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let cancel = use_callback(react, &cancel.into_js_value(), &Array::new())?;
    let effect_cancel = cancel.clone();
    let effect_triggers = triggers.clone();
    let effect_set_position = set_position.clone();
    let disable_effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if disabled {
            effect_cancel.call0(&JsValue::UNDEFINED)?;
            Reflect::set(
                &effect_triggers,
                &JsValue::from_str("current"),
                &object(&[("hover", JsValue::FALSE), ("focus", JsValue::FALSE)])?.into(),
            )?;
            set_state(&effect_set_position, &JsValue::NULL)?;
        }
        Ok(effect_cancel.clone().into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    function(react, "useEffect")?.call2(
        react,
        &disable_effect.into_js_value(),
        &Array::of2(cancel.as_ref(), &JsValue::from_bool(disabled)),
    )?;

    let show_anchor = anchor;
    let show_set_placement = set_placement;
    let show_set_position = set_position.clone();
    let show_side = side.clone();
    let show = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if disabled {
            return Ok(());
        }
        let element = Reflect::get(&show_anchor, &JsValue::from_str("current"))?;
        if element.is_null() {
            return Ok(());
        }
        let bounds = call_method(&element, "getBoundingClientRect", &[])?;
        let left = required_number(&bounds, "left", "DOMRect")?;
        let right = required_number(&bounds, "right", "DOMRect")?;
        let width = required_number(&bounds, "width", "DOMRect")?;
        let top = required_number(&bounds, "top", "DOMRect")?;
        let bottom = required_number(&bounds, "bottom", "DOMRect")?;
        set_state(&show_set_placement, &JsValue::from_str(&show_side))?;
        set_state(
            &show_set_position,
            &object(&[
                (
                    "x",
                    JsValue::from_f64(if show_side == "right" {
                        right + 10.0
                    } else {
                        left + width / 2.0
                    }),
                ),
                ("top", JsValue::from_f64(top)),
                ("bottom", JsValue::from_f64(bottom)),
            ])?
            .into(),
        )
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let show = show.into_js_value().dyn_into::<Function>()?;
    let delayed_cancel = cancel.clone();
    let delayed_show = show.clone();
    let delayed_timer = timer;
    let show_after = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        delayed_cancel.call0(&JsValue::UNDEFINED)?;
        if delay <= 0.0 {
            delayed_show.call0(&JsValue::UNDEFINED)?;
            return Ok(());
        }
        let callback_timer = delayed_timer.clone();
        let callback_show = delayed_show.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            Reflect::set(
                &callback_timer,
                &JsValue::from_str("current"),
                &JsValue::NULL,
            )?;
            callback_show.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let global = js_sys::global();
        let handle = function(&global, "setTimeout")?.call2(
            &global,
            &callback.into_js_value(),
            &JsValue::from_f64(delay),
        )?;
        Reflect::set(&delayed_timer, &JsValue::from_str("current"), &handle)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let show_after = show_after.into_js_value().dyn_into::<Function>()?;

    let child_props = required_property(&child, "props", "tooltip child")?;
    let enter_child = optional_function(&child_props, "onMouseEnter")?;
    let enter_triggers = triggers.clone();
    let enter_show = show_after;
    let enter = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if let Some(handler) = &enter_child {
            handler.call1(&JsValue::UNDEFINED, &event)?;
        }
        set_trigger(&enter_triggers, "hover", true)?;
        enter_show.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let leave_child = optional_function(&child_props, "onMouseLeave")?;
    let leave_triggers = triggers.clone();
    let leave_cancel = cancel.clone();
    let leave_position = set_position.clone();
    let leave = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if let Some(handler) = &leave_child {
            handler.call1(&JsValue::UNDEFINED, &event)?;
        }
        set_trigger(&leave_triggers, "hover", false)?;
        leave_cancel.call0(&JsValue::UNDEFINED)?;
        set_state(&leave_position, &JsValue::NULL)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let focus_child = optional_function(&child_props, "onFocus")?;
    let focus_triggers = triggers.clone();
    let focus_cancel = cancel.clone();
    let focus_show = show;
    let focus = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if let Some(handler) = &focus_child {
            handler.call1(&JsValue::UNDEFINED, &event)?;
        }
        set_trigger(&focus_triggers, "focus", true)?;
        focus_cancel.call0(&JsValue::UNDEFINED)?;
        focus_show.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let blur_child = optional_function(&child_props, "onBlur")?;
    let blur_triggers = triggers;
    let blur_cancel = cancel;
    let blur_position = set_position;
    let blur = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if let Some(handler) = &blur_child {
            handler.call1(&JsValue::UNDEFINED, &event)?;
        }
        set_trigger(&blur_triggers, "focus", false)?;
        blur_cancel.call0(&JsValue::UNDEFINED)?;
        let current = required_property(&blur_triggers, "current", "tooltip triggers")?;
        if !property_truthy(&current, "hover")? && !property_truthy(&current, "focus")? {
            set_state(&blur_position, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let injected = object(&[
        ("ref", merged.into()),
        ("onMouseEnter", enter.into_js_value()),
        ("onMouseLeave", leave.into_js_value()),
        ("onFocus", focus.into_js_value()),
        ("onBlur", blur.into_js_value()),
    ])?;
    let anchor = function(react, "cloneElement")?.call2(react, &child, &injected)?;
    let mut output = vec![anchor];
    if !position.is_null() {
        let mut style = vec![
            ("left", JsValue::from_f64(x)),
            ("top", JsValue::from_f64(y)),
        ];
        if let Some(max_width) = max_width {
            style.push(("maxWidth", JsValue::from_f64(max_width)));
        }
        output.push(create_element(
            react,
            &JsValue::from_str("span"),
            Some(&object(&[
                ("ref", bubble),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-tooltip-bubble"),
                ),
                ("data-side", JsValue::from_str(&placement_text)),
                ("style", object(&style)?.into()),
                ("role", JsValue::from_str("tooltip")),
            ])?),
            &[resolved_label],
        )?);
    }
    create_element(
        react,
        &required_property(react, "Fragment", "React")?,
        None,
        &output,
    )
}

fn set_trigger(reference: &JsValue, key: &str, value: bool) -> Result<(), JsValue> {
    let current = required_property(reference, "current", "tooltip triggers")?;
    Reflect::set(
        &current,
        &JsValue::from_str(key),
        &JsValue::from_bool(value),
    )?;
    Ok(())
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = "@seekdeep-ai/seekdeep-client-ui-primitives/Tooltip.module.css";
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("style[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let css = TOOLTIP_CSS.replace(".bubble", ".seekdeep-primitive-tooltip-bubble");
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin-css"), JsValue::from_str(tag)],
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

fn configured_react() -> Result<JsValue, JsValue> {
    REACT.with(|slot| {
        slot.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives tooltip module was not configured").into()
        })
    })
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value.dyn_into::<Function>().map(Some)
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
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
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
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a number")).into())
    }
}

fn property_truthy(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?.is_truthy())
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(value)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required_property(value, key, "object")?.dyn_into::<Function>()
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

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef")?.call1(react, initial)
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &Array,
) -> Result<Function, JsValue> {
    function(react, "useCallback")?
        .call2(react, callback, dependencies)?
        .dyn_into::<Function>()
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn create_element(
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
    function(react, "createElement")?.apply(react, &arguments)
}
