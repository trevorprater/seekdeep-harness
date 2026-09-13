//! Compiled draft attachment rail and scroll lifecycle.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::browser::{
    BrowserDependencies, call_method, class_name, create_element, object, required_function,
    required_property, required_string,
};

const WHEEL_LINE_PX: f64 = 16.0;

pub(crate) fn component(dependencies: &BrowserDependencies) -> JsValue {
    let dependencies = dependencies.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render(&dependencies, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::float_cmp, clippy::too_many_lines)] // `deltaMode` is an exact integer DOM enum.
fn render(dependencies: &BrowserDependencies, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &dependencies.react;
    let items = required_property(props, "items", "AttachmentRail props")?;
    if !Array::is_array(&items) {
        return Err(js_sys::TypeError::new("AttachmentRail items must be an array").into());
    }
    let items = Array::from(&items);
    let labels = required_property(props, "labels", "AttachmentRail props")?;
    let group_label = required_string(&labels, "group", "AttachmentRail labels")?;
    let open_label = required_string(&labels, "open", "AttachmentRail labels")?;
    let left_label = required_string(&labels, "scrollLeft", "AttachmentRail labels")?;
    let right_label = required_string(&labels, "scrollRight", "AttachmentRail labels")?;
    let on_open = required_function(props, "onOpen", "AttachmentRail props")?;
    let on_remove = required_function(props, "onRemove", "AttachmentRail props")?;
    let rail_ref = use_ref(react, &JsValue::NULL)?;
    let count_ref = use_ref(react, &JsValue::NULL)?;
    let initial_edges = object(&[("left", JsValue::FALSE), ("right", JsValue::FALSE)])?;
    let (edges, set_edges) = use_state(react, initial_edges.as_ref())?;

    let edge_ref = rail_ref.clone();
    let edge_setter = set_edges;
    let update_edges = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let element = current(&edge_ref)?;
        if element.is_null() {
            return Ok(());
        }
        let scroll_left = required_number(&element, "scrollLeft", "attachment rail")?;
        let scroll_width = required_number(&element, "scrollWidth", "attachment rail")?;
        let client_width = required_number(&element, "clientWidth", "attachment rail")?;
        let left = scroll_left > 1.0;
        let right = scroll_left < scroll_width - client_width - 1.0;
        let updater = Closure::wrap(
            Box::new(move |previous: JsValue| -> Result<JsValue, JsValue> {
                if Reflect::get(&previous, &JsValue::from_str("left"))?.as_bool() == Some(left)
                    && Reflect::get(&previous, &JsValue::from_str("right"))?.as_bool()
                        == Some(right)
                {
                    return Ok(previous);
                }
                Ok(object(&[
                    ("left", JsValue::from_bool(left)),
                    ("right", JsValue::from_bool(right)),
                ])?
                .into())
            }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
        edge_setter
            .call1(&JsValue::UNDEFINED, &updater.into_js_value())
            .map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into::<Function>()?;
    let update_edges = use_callback(react, update_edges.as_ref(), &Array::new())?;

    let layout_count = count_ref.clone();
    let layout_rail = rail_ref.clone();
    let layout_update = update_edges.clone();
    let item_count = items.length();
    let layout = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let previous = current(&layout_count)?;
        let grew = !previous.is_null()
            && previous
                .as_f64()
                .is_some_and(|value| f64::from(item_count) > value);
        set_current(&layout_count, &JsValue::from_f64(f64::from(item_count)))?;
        let element = current(&layout_rail)?;
        if element.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        if grew {
            let end = required_number(&element, "scrollWidth", "attachment rail")?
                - required_number(&element, "clientWidth", "attachment rail")?;
            Reflect::set(
                &element,
                &JsValue::from_str("scrollLeft"),
                &JsValue::from_f64(end),
            )?;
        }
        layout_update.call0(&JsValue::UNDEFINED)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useLayoutEffect", "React")?.call2(
        react,
        &layout.into_js_value(),
        &Array::of2(
            &JsValue::from_f64(f64::from(item_count)),
            update_edges.as_ref(),
        ),
    )?;

    let effect_rail = rail_ref.clone();
    let effect_update = update_edges.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let element = current(&effect_rail)?;
        if element.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let mut observer = JsValue::NULL;
        let resize = Reflect::get(&js_sys::global(), &JsValue::from_str("ResizeObserver"))?;
        if resize.is_function() {
            observer = Reflect::construct(
                &resize.dyn_into::<Function>()?,
                &Array::of1(effect_update.as_ref()),
            )?;
            call_method(&observer, "observe", std::slice::from_ref(&element))?;
        }
        let wheel_element = element.clone();
        let wheel = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let delta_y = required_number(&event, "deltaY", "wheel event")?;
            if delta_y == 0.0 {
                return Ok(());
            }
            let delta_x = required_number(&event, "deltaX", "wheel event")?;
            let mode = required_number(&event, "deltaMode", "wheel event")?;
            let client_width = required_number(&wheel_element, "clientWidth", "attachment rail")?;
            let scale = if mode == 1.0 {
                WHEEL_LINE_PX
            } else if mode == 2.0 {
                client_width
            } else {
                1.0
            };
            call_method(&event, "preventDefault", &[])?;
            let left = if delta_x == 0.0 {
                delta_y.signum() * (delta_y.abs() * scale).min(60.0)
            } else {
                delta_x * scale
            };
            call_method(
                &wheel_element,
                "scrollBy",
                &[object(&[
                    ("left", JsValue::from_f64(left)),
                    ("behavior", JsValue::from_str("auto")),
                ])?
                .into()],
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()?;
        call_method(
            &element,
            "addEventListener",
            &[
                JsValue::from_str("wheel"),
                wheel.clone().into(),
                object(&[("passive", JsValue::FALSE)])?.into(),
            ],
        )?;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if !observer.is_null() {
                call_method(&observer, "disconnect", &[])?;
            }
            call_method(
                &element,
                "removeEventListener",
                &[JsValue::from_str("wheel"), wheel.clone().into()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(update_edges.as_ref()),
    )?;

    let mut root_children = Vec::new();
    if edge(&edges, "left")? {
        root_children.push(render_arrow(
            dependencies,
            &rail_ref,
            -1.0,
            &left_label,
            "arrowLeft",
        )?);
    }
    let mut item_nodes = Vec::new();
    for item in items.iter() {
        let id = required_string(&item, "id", "AttachmentRail item")?;
        let preview = required_string(&item, "previewUrl", "AttachmentRail item")?;
        let alt = required_string(&item, "alt", "AttachmentRail item")?;
        let remove_label = required_string(&item, "removeLabel", "AttachmentRail item")?;
        let open_item = item.clone();
        let open = on_open.clone();
        let open_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            open.call1(&JsValue::UNDEFINED, &open_item)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let image = create_element(
            react,
            &JsValue::from_str("img"),
            Some(&object(&[
                ("src", JsValue::from_str(&preview)),
                ("alt", JsValue::from_str(&alt)),
            ])?),
            &[],
        )?;
        let thumbnail = create_element(
            react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name("AttachmentRail", "thumbnail")),
                ),
                ("title", JsValue::from_str(&open_label)),
                ("onClick", open_click.into_js_value()),
            ])?),
            &[image],
        )?;
        let remove_item = item.clone();
        let remove = on_remove.clone();
        let remove_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            remove.call1(&JsValue::UNDEFINED, &remove_item)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let close_icon = create_element(
            react,
            &dependencies.close_fill,
            Some(&object(&[("size", JsValue::from_f64(12.0))])?),
            &[],
        )?;
        let remove = create_element(
            react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name("AttachmentRail", "remove")),
                ),
                ("aria-label", JsValue::from_str(&remove_label)),
                ("onClick", remove_click.into_js_value()),
            ])?),
            &[close_icon],
        )?;
        item_nodes.push(create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                (
                    "className",
                    JsValue::from_str(&class_name("AttachmentRail", "item")),
                ),
            ])?),
            &[thumbnail, remove],
        )?);
    }
    let rail = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", rail_ref.clone()),
            (
                "className",
                JsValue::from_str(&class_name("AttachmentRail", "rail")),
            ),
            ("role", JsValue::from_str("group")),
            ("aria-label", JsValue::from_str(&group_label)),
            ("onScroll", update_edges.into()),
        ])?),
        &item_nodes,
    )?;
    root_children.push(rail);
    if edge(&edges, "right")? {
        root_children.push(render_arrow(
            dependencies,
            &rail_ref,
            1.0,
            &right_label,
            "arrowRight",
        )?);
    }
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[(
            "className",
            JsValue::from_str(&class_name("AttachmentRail", "root")),
        )])?),
        &root_children,
    )
}

fn render_arrow(
    dependencies: &BrowserDependencies,
    rail_ref: &JsValue,
    direction: f64,
    label: &str,
    side: &str,
) -> Result<JsValue, JsValue> {
    let reference = rail_ref.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let element = current(&reference)?;
        if element.is_null() {
            return Ok(());
        }
        let width = required_number(&element, "clientWidth", "attachment rail")?;
        let behavior = page_behavior()?;
        call_method(
            &element,
            "scrollBy",
            &[object(&[
                (
                    "left",
                    JsValue::from_f64(direction * (width - 64.0).max(200.0)),
                ),
                ("behavior", JsValue::from_str(behavior)),
            ])?
            .into()],
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let icon = create_element(
        &dependencies.react,
        if direction < 0.0 {
            &dependencies.chevron_left
        } else {
            &dependencies.chevron_right
        },
        None,
        &[],
    )?;
    create_element(
        &dependencies.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&format!(
                    "{} {}",
                    class_name("AttachmentRail", "arrow"),
                    class_name("AttachmentRail", side)
                )),
            ),
            ("aria-label", JsValue::from_str(label)),
            ("onClick", click.into_js_value()),
        ])?),
        &[icon],
    )
}

fn page_behavior() -> Result<&'static str, JsValue> {
    let window = Reflect::get(&js_sys::global(), &JsValue::from_str("window"))?;
    let media = Reflect::get(&window, &JsValue::from_str("matchMedia"))?;
    if !media.is_function() {
        return Ok("smooth");
    }
    let query = media.dyn_into::<Function>()?.call1(
        &window,
        &JsValue::from_str("(prefers-reduced-motion: reduce)"),
    )?;
    Ok(
        if Reflect::get(&query, &JsValue::from_str("matches"))?.as_bool() == Some(true) {
            "auto"
        } else {
            "smooth"
        },
    )
}

fn edge(edges: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(edges, &JsValue::from_str(key))?.as_bool() == Some(true))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
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

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}
