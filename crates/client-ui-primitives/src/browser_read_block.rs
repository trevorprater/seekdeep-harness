//! Compiled line-numbered read-result surface over the shared highlighter.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser_code_block::inject_namespaced_style, highlight_lines, highlight_store_faces,
    write_clipboard,
};

const READ_BLOCK_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/ReadBlock.module.css");
/// Source-compatible default height cap.
pub const DEFAULT_READ_MAX_LINES: u32 = 16;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    subscribe: Function,
    snapshot: Function,
}

/// Configures React for the compiled `ReadBlock` over the shared highlighter.
///
/// # Errors
///
/// Returns before highlighter configuration or on stylesheet injection failure.
#[wasm_bindgen(js_name = configureClientUiPrimitiveReadBlock)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_read_block(react: JsValue) -> Result<(), JsValue> {
    let (subscribe, snapshot) = highlight_store_faces()?;
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            subscribe,
            snapshot,
        });
    });
    inject_style()
}

/// Returns the compiled `ReadBlock` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = readBlockComponent)]
pub fn read_block_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_read_block(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the source-compatible default content-line cap.
#[wasm_bindgen(js_name = defaultReadMaxLines)]
#[must_use]
pub fn default_read_max_lines() -> u32 {
    DEFAULT_READ_MAX_LINES
}

#[allow(clippy::too_many_lines)]
fn render_read_block(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let label = optional_string(props, "label")?;
    let lines = required_array(props, "lines", "ReadBlock props")?;
    let total_lines = required_number(props, "totalLines", "ReadBlock props")?;
    let lang = optional_string(props, "lang")?;
    let max_lines =
        optional_number(props, "maxLines")?.unwrap_or_else(|| f64::from(DEFAULT_READ_MAX_LINES));
    let class_name = optional_string(props, "className")?;
    let raw_lines = lines.clone();
    let raw_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(JsValue::from_str(
            &raw_lines
                .iter()
                .map(|line| required_string(&line, "text", "ReadBlock line"))
                .collect::<Result<Vec<_>, _>>()?
                .join("\n"),
        ))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let raw_dependencies = Array::of1(lines.as_ref());
    let raw = use_memo(react, &raw_factory.into_js_value(), &raw_dependencies)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("ReadBlock raw memo must be a string"))?;
    let loaded = required_function(react, "useSyncExternalStore", "React")?.call3(
        react,
        modules.subscribe.as_ref(),
        modules.snapshot.as_ref(),
        modules.snapshot.as_ref(),
    )?;
    let highlight_raw = raw.clone();
    let highlight_lang = lang.clone();
    let highlight_factory = Closure::wrap(Box::new(move || {
        highlight_lines(highlight_raw.clone(), highlight_lang.clone())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let highlight_dependencies = Array::new();
    highlight_dependencies.push(&JsValue::from_str(&raw));
    highlight_dependencies.push(
        &lang
            .as_deref()
            .map_or(JsValue::UNDEFINED, JsValue::from_str),
    );
    highlight_dependencies.push(&loaded);
    let highlighted = use_memo(
        react,
        &highlight_factory.into_js_value(),
        &highlight_dependencies,
    )?;
    let highlighted = (!highlighted.is_undefined()).then(|| Array::from(&highlighted));
    let (expanded_value, set_expanded) = use_state(react, &JsValue::FALSE)?;
    let expanded = expanded_value.as_bool().unwrap_or(false);
    let (copied_value, set_copied) = use_state(react, &JsValue::FALSE)?;
    let copied = copied_value.as_bool().unwrap_or(false);

    let copy_text = raw.clone();
    let copy_setter = set_copied;
    let on_copy = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if copied {
            return Ok(());
        }
        let pending = write_clipboard(copy_text.clone());
        let setter = copy_setter.clone();
        let settled = Closure::wrap(Box::new(move |accepted: JsValue| -> Result<(), JsValue> {
            if accepted.as_bool() != Some(true) {
                return Ok(());
            }
            set_state(&setter, &JsValue::TRUE)?;
            let reset = setter.clone();
            let callback = Closure::wrap(Box::new(move || {
                let _ = set_state(&reset, &JsValue::FALSE);
            }) as Box<dyn FnMut()>);
            required_function(&js_sys::global(), "setTimeout", "global")?.call2(
                &js_sys::global(),
                &callback.into_js_value(),
                &JsValue::from_f64(1_000.0),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        call_method(
            Promise::resolve(&pending).as_ref(),
            "then",
            &[settled.into_js_value()],
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let copy_dependencies = Array::new();
    copy_dependencies.push(&JsValue::from_bool(copied));
    copy_dependencies.push(&JsValue::from_str(&raw));
    let on_copy = use_callback(react, &on_copy.into_js_value(), &copy_dependencies)?;
    let toggle_setter = set_expanded;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater = Closure::wrap(Box::new(move |value: JsValue| {
            JsValue::from_bool(value.as_bool() != Some(true))
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        toggle_setter
            .call1(&JsValue::UNDEFINED, &updater.into_js_value())
            .map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let toggle = use_callback(react, &toggle.into_js_value(), &Array::new())?;

    let length = f64::from(lines.length());
    let hidden = length - max_lines;
    let capped = hidden > 0.0 && !expanded;
    let head = js_sys::Math::ceil(max_lines / 2.0);
    let tail = max_lines - head;
    let windowed = length < total_lines;
    let paired = Array::new();
    for index in 0..lines.length() {
        let spans = highlighted
            .as_ref()
            .map_or(JsValue::UNDEFINED, |rows| rows.get(index));
        paired.push(&Array::of2(&lines.get(index), &spans));
    }
    let primary = if capped {
        array_slice(&paired, 0.0, Some(head))?
    } else {
        paired.clone()
    };
    let mut body_children = render_rows(react, &primary)?;
    if hidden > 0.0 {
        let hidden_label = js_number_text(hidden)?;
        let aria_label = if expanded {
            "收起内容".to_owned()
        } else {
            format!("展开其余 {hidden_label} 行")
        };
        let visible_label = if expanded {
            "收起".to_owned()
        } else {
            format!("… 其余 {hidden_label} 行")
        };
        body_children.push(create_element(
            react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-read-block-expand"),
                ),
                ("aria-expanded", JsValue::from_bool(expanded)),
                ("aria-label", JsValue::from_str(&aria_label)),
                ("onClick", toggle.clone().into()),
            ])?),
            &[JsValue::from_str(&visible_label)],
        )?);
    }
    if capped {
        body_children.extend(render_rows(
            react,
            &array_slice(&paired, length - tail, None)?,
        )?);
    }

    let mut action_children = Vec::new();
    if windowed {
        action_children.push(create_element(
            react,
            &JsValue::from_str("span"),
            Some(&class_props("seekdeep-primitive-read-block-count")?),
            &[JsValue::from_str(&format!(
                "显示 {} / {} 行",
                lines.length(),
                js_number_text(total_lines)?
            ))],
        )?);
    }
    action_children.push(create_element(
        react,
        &JsValue::from_str("span"),
        Some(&class_props("seekdeep-primitive-read-block-lang")?),
        &[JsValue::from_str(lang.as_deref().unwrap_or_default())],
    )?);
    if lines.length() > 0 {
        action_children.push(create_element(
            react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-read-block-copyButton"),
                ),
                ("onClick", on_copy.into()),
            ])?),
            &[JsValue::from_str(if copied {
                "复制成功"
            } else {
                "复制"
            })],
        )?);
    }
    let banner = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-primitive-read-block-banner")?),
        &[
            create_element(
                react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-primitive-read-block-label")?),
                &[JsValue::from_str(label.as_deref().unwrap_or_default())],
            )?,
            create_element(
                react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-primitive-read-block-action")?),
                &action_children,
            )?,
        ],
    )?;
    let body = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-primitive-read-block-body")?),
        &body_children,
    )?;
    let mut classes = vec!["seekdeep-primitive-read-block-block".to_owned()];
    if let Some(class_name) = class_name.filter(|class_name| !class_name.is_empty()) {
        classes.push(class_name);
    }
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("className", JsValue::from_str(&classes.join(" "))),
            ("data-read", JsValue::from_str("")),
        ])?),
        &[banner, body],
    )
}

fn render_rows(react: &JsValue, pairs: &Array) -> Result<Vec<JsValue>, JsValue> {
    pairs
        .iter()
        .map(|pair| {
            let pair = Array::from(&pair);
            let line = pair.get(0);
            let number = required_property(&line, "number", "ReadBlock line")?;
            let text = required_string(&line, "text", "ReadBlock line")?;
            let spans = pair.get(1);
            let content = if spans.is_undefined() {
                vec![JsValue::from_str(&text)]
            } else {
                Array::from(&spans)
                    .iter()
                    .enumerate()
                    .map(|(index, span)| {
                        let index = u32::try_from(index).map_err(|_| {
                            js_sys::RangeError::new("highlight span index exceeds JS array range")
                        })?;
                        let text = required_string(&span, "text", "highlight span")?;
                        let style = required_property(&span, "style", "highlight span")?;
                        create_element(
                            react,
                            &JsValue::from_str("span"),
                            Some(&object(&[
                                ("key", JsValue::from_f64(f64::from(index))),
                                ("style", style),
                            ])?),
                            &[JsValue::from_str(&text)],
                        )
                    })
                    .collect::<Result<Vec<_>, JsValue>>()?
            };
            create_element(
                react,
                &JsValue::from_str("div"),
                Some(&object(&[
                    ("key", number.clone()),
                    (
                        "className",
                        JsValue::from_str("seekdeep-primitive-read-block-line"),
                    ),
                ])?),
                &[
                    create_element(
                        react,
                        &JsValue::from_str("span"),
                        Some(&object(&[
                            (
                                "className",
                                JsValue::from_str("seekdeep-primitive-read-block-gutter"),
                            ),
                            ("aria-hidden", JsValue::TRUE),
                        ])?),
                        &[number],
                    )?,
                    create_element(
                        react,
                        &JsValue::from_str("span"),
                        Some(&class_props("seekdeep-primitive-read-block-content")?),
                        &content,
                    )?,
                ],
            )
        })
        .collect()
}

fn inject_style() -> Result<(), JsValue> {
    let replacements = [
        ("copyButton", "seekdeep-primitive-read-block-copyButton"),
        ("banner", "seekdeep-primitive-read-block-banner"),
        ("label", "seekdeep-primitive-read-block-label"),
        ("action", "seekdeep-primitive-read-block-action"),
        ("count", "seekdeep-primitive-read-block-count"),
        ("lang", "seekdeep-primitive-read-block-lang"),
        ("body", "seekdeep-primitive-read-block-body"),
        ("gutter", "seekdeep-primitive-read-block-gutter"),
        ("content", "seekdeep-primitive-read-block-content"),
        ("expand", "seekdeep-primitive-read-block-expand"),
        ("block", "seekdeep-primitive-read-block-block"),
        ("line", "seekdeep-primitive-read-block-line"),
    ];
    inject_namespaced_style("ReadBlock", READ_BLOCK_CSS, &replacements)
}

fn js_number_text(value: f64) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(value))?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String(number) returned a non-string").into())
}

fn array_slice(array: &Array, start: f64, end: Option<f64>) -> Result<Array, JsValue> {
    let mut arguments = vec![JsValue::from_f64(start)];
    if let Some(end) = end {
        arguments.push(JsValue::from_f64(end));
    }
    Ok(call_method(array.as_ref(), "slice", &arguments)?.unchecked_into())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives ReadBlock module was not configured").into()
        })
    })
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_memo(react: &JsValue, factory: &JsValue, dependencies: &Array) -> Result<JsValue, JsValue> {
    required_function(react, "useMemo", "React")?.call2(react, factory, dependencies)
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &Array,
) -> Result<Function, JsValue> {
    required_function(react, "useCallback", "React")?
        .call2(react, callback, dependencies)?
        .dyn_into()
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_array(value: &JsValue, key: &str, owner: &str) -> Result<Array, JsValue> {
    let value = required_property(value, key, owner)?;
    if Array::is_array(&value) {
        Ok(value.unchecked_into())
    } else {
        Err(js_sys::TypeError::new(&format!("{owner} {key} must be an array")).into())
    }
}

fn optional_number(value: &JsValue, key: &str) -> Result<Option<f64>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_f64()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a number")).into())
    }
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
    }
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
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
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
