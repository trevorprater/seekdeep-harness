//! Browser candidate menu presentation and pointer lifecycle.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{
    BrowserModules, call_method, fragment, object, optional, required, required_function,
    required_string, tag, translated, use_effect, use_ref,
};

pub(crate) fn component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| render(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let menu = required(props, "menu", "MenuView")?;
    let on_pick = required_function(props, "onPick", "MenuView")?;
    let on_dismiss = required_function(props, "onDismiss", "MenuView")?;
    let translate = required_function(props, "t", "MenuView")?;
    let state = use_store(&modules.react, &menu)?;
    let list_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let max_height = modules.anchored_max_height.call3(
        &JsValue::UNDEFINED,
        &list_ref,
        &JsValue::from_f64(320.0),
        &state,
    )?;
    let open = required(&state, "open", "Menu state")?
        .as_bool()
        .unwrap_or(false);
    let highlight = optional(&state, "highlight")?;
    install_scroll_effect(&modules.react, highlight.as_ref())?;
    install_outside_effect(&modules.react, open, &list_ref, &on_dismiss)?;
    if !open {
        return Ok(JsValue::NULL);
    }
    let active_id = highlight
        .as_ref()
        .map(|highlight| {
            Ok::<_, JsValue>(option_id(
                &required_string(highlight, "source", "Menu highlight")?,
                required_index(highlight, "index")?,
            ))
        })
        .transpose()?;
    let mut group_nodes = Vec::new();
    for group in Array::from(&required(&state, "groups", "Menu state")?).iter() {
        let source = required_string(&group, "source", "Menu group")?;
        let status = required_string(&group, "status", "Menu group")?;
        let items = Array::from(&required(&group, "items", "Menu group")?);
        if status == "ready" && items.length() == 0 {
            continue;
        }
        let mut children = vec![tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-trigger-groupTitle"),
                ),
                ("role", JsValue::from_str("presentation")),
                ("data-source", JsValue::from_str(&source)),
            ])?),
            &[translated(&translate, &source)?],
        )?];
        if status == "pending" {
            children.push(tag(
                &modules.react,
                "div",
                Some(&object(&[
                    ("className", JsValue::from_str("seekdeep-trigger-loading")),
                    ("data-source", JsValue::from_str(&source)),
                ])?),
                &[translated(&translate, "loading")?],
            )?);
        } else {
            for (index, item) in items.iter().enumerate() {
                children.push(render_item(
                    modules,
                    &source,
                    index,
                    &item,
                    highlight.as_ref(),
                    &on_pick,
                )?);
            }
        }
        group_nodes.push(fragment(&modules.react, &children)?);
    }
    let viewport = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-trigger-viewport"),
        )])?),
        &group_nodes,
    )?;
    let style = Object::new();
    Reflect::set(&style, &JsValue::from_str("maxHeight"), &max_height)?;
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("ref", list_ref),
            ("className", JsValue::from_str("seekdeep-trigger-menu")),
            ("style", style.into()),
            ("role", JsValue::from_str("listbox")),
            ("aria-label", translated(&translate, "suggestions.aria")?),
            (
                "aria-activedescendant",
                active_id.map_or(JsValue::UNDEFINED, |id| JsValue::from_str(&id)),
            ),
        ])?),
        &[viewport],
    )
}

fn render_item(
    modules: &BrowserModules,
    source: &str,
    index: usize,
    item: &JsValue,
    highlight: Option<&JsValue>,
    on_pick: &Function,
) -> Result<JsValue, JsValue> {
    let name = required_string(item, "name", "trigger candidate")?;
    let active = highlight.is_some_and(|highlight| {
        required_string(highlight, "source", "Menu highlight").as_deref() == Ok(source)
            && required_index(highlight, "index") == Ok(index)
    });
    let pick = on_pick.clone();
    let source = source.to_owned();
    let pick_source = source.clone();
    let mouse_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call_method(&event, "preventDefault", &[])?;
        pick.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&pick_source),
            &JsValue::from_f64(usize_as_f64(index)),
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let mut children = Vec::new();
    if let Some(icon) = optional(item, "icon")? {
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str("seekdeep-trigger-itemIcon")),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[icon],
        )?);
    }
    children.push(tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-trigger-itemName"),
        )])?),
        &[JsValue::from_str(&name)],
    )?);
    if let Some(description) = optional(item, "description")? {
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-trigger-itemDescription"),
            )])?),
            &[description],
        )?);
    }
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("key", JsValue::from_str(&format!("{source}:{name}"))),
            ("id", JsValue::from_str(&option_id(&source, index))),
            ("type", JsValue::from_str("button")),
            ("role", JsValue::from_str("option")),
            ("aria-selected", JsValue::from_bool(active)),
            (
                "className",
                JsValue::from_str(if active {
                    "seekdeep-trigger-item seekdeep-trigger-active"
                } else {
                    "seekdeep-trigger-item"
                }),
            ),
            ("onMouseDown", mouse_down.into_js_value()),
        ])?),
        &children,
    )
}

fn use_store(react: &JsValue, store: &JsValue) -> Result<JsValue, JsValue> {
    let subscribe_store = store.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        call_method(&subscribe_store, "subscribe", &[listener.into()])
    })
        as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
    let snapshot_store = store.clone();
    let snapshot = Closure::wrap(
        Box::new(move || call_method(&snapshot_store, "getSnapshot", &[]))
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
    );
    required_function(react, "useSyncExternalStore", "React")?.call2(
        react,
        &subscribe.into_js_value(),
        &snapshot.into_js_value(),
    )
}

fn install_scroll_effect(react: &JsValue, highlight: Option<&JsValue>) -> Result<(), JsValue> {
    let source = highlight
        .map(|value| required_string(value, "source", "Menu highlight"))
        .transpose()?;
    let index = highlight
        .map(|value| required_index(value, "index"))
        .transpose()?;
    let effect_source = source.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let (Some(source), Some(index)) = (&effect_source, index) else {
            return Ok(JsValue::UNDEFINED);
        };
        let document = required(&js_sys::global(), "document", "global")?;
        let node = call_method(
            &document,
            "getElementById",
            &[JsValue::from_str(&option_id(source, index))],
        )?;
        if !node.is_null() {
            let options = object(&[("block", JsValue::from_str("nearest"))])?;
            call_method(&node, "scrollIntoView", &[options.into()])?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(
            &source.map_or(JsValue::NULL, |value| JsValue::from_str(&value)),
            &index.map_or(JsValue::NULL, |value| {
                JsValue::from_f64(usize_as_f64(value))
            }),
        ),
    )
}

fn install_outside_effect(
    react: &JsValue,
    open: bool,
    list_ref: &JsValue,
    on_dismiss: &Function,
) -> Result<(), JsValue> {
    let list_ref = list_ref.clone();
    let dismiss = on_dismiss.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open {
            return Ok(JsValue::UNDEFINED);
        }
        let document = required(&js_sys::global(), "document", "global")?;
        let listener_ref = list_ref.clone();
        let listener_dismiss = dismiss.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let target = Reflect::get(&event, &JsValue::from_str("target"))?;
            if !target.is_instance_of::<web_sys::Node>() {
                return Ok(());
            }
            let list = Reflect::get(&listener_ref, &JsValue::from_str("current"))?;
            if list
                .dyn_ref::<web_sys::Node>()
                .zip(target.dyn_ref::<web_sys::Node>())
                .is_some_and(|(list, target)| list.contains(Some(target)))
            {
                return Ok(());
            }
            let card = if list.is_null() {
                JsValue::NULL
            } else {
                call_method(
                    &list,
                    "closest",
                    &[JsValue::from_str("[data-composer-card]")],
                )?
            };
            if card
                .dyn_ref::<web_sys::Node>()
                .zip(target.dyn_ref::<web_sys::Node>())
                .is_some_and(|(card, target)| card.contains(Some(target)))
            {
                return Ok(());
            }
            listener_dismiss.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        call_method(
            &document,
            "addEventListener",
            &[
                JsValue::from_str("pointerdown"),
                listener.clone(),
                JsValue::TRUE,
            ],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &document,
                "removeEventListener",
                &[
                    JsValue::from_str("pointerdown"),
                    listener.clone(),
                    JsValue::TRUE,
                ],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), &on_dismiss.clone().into()),
    )
}

fn option_id(source: &str, index: usize) -> String {
    format!("seekdeep-slash-option-{source}-{index}")
}

fn required_index(value: &JsValue, key: &str) -> Result<usize, JsValue> {
    let value = required(value, key, "numeric value")?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("{key} must be numeric")))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
