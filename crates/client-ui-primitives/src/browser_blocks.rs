//! Compiled structured result-card components.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::write_clipboard;

const DIFF_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/DiffBlock.module.css");
const DEFAULT_DIFF_MAX_LINES: f64 = 16.0;

thread_local! {
    static REACT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffRowKind {
    Path,
    Delete,
    Add,
    Gap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffRow {
    kind: DiffRowKind,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffModel {
    rows: Vec<DiffRow>,
    added: usize,
    removed: usize,
    files: usize,
}

/// Configures React and installs structured-card styles.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveBlocks)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_blocks(react: JsValue) -> Result<(), JsValue> {
    REACT.with(|slot| *slot.borrow_mut() = Some(react));
    inject_style(
        "DiffBlock",
        DIFF_CSS,
        &[
            "block",
            "copyButton",
            "body",
            "line",
            "path",
            "del",
            "add",
            "gap",
            "expand",
            "footer",
        ],
    )
}

/// Returns the compiled `DiffBlock` component.
///
/// # Errors
///
/// Returns missing React configuration.
#[wasm_bindgen(js_name = diffBlockComponent)]
pub fn diff_block_component() -> Result<JsValue, JsValue> {
    let react = configured_react()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_diff(&react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

#[allow(clippy::too_many_lines)]
fn render_diff(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let model = build_diff_model(&required_property(props, "diffs", "DiffBlock props")?)?;
    let max_lines = optional_number(props, "maxLines")?.unwrap_or(DEFAULT_DIFF_MAX_LINES);
    let class_name = optional_string(props, "className")?;
    let (expanded, set_expanded) = use_state(react, &JsValue::FALSE)?;
    let expanded = expanded.as_bool().unwrap_or(false);
    let (copied, set_copied) = use_state(react, &JsValue::FALSE)?;
    let copied = copied.as_bool().unwrap_or(false);
    if model.rows.is_empty() {
        return Ok(JsValue::NULL);
    }
    let copy_text = copy_text(&model.rows);
    let copy_setter = set_copied;
    let copy = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
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
            let callback = Closure::wrap(Box::new(move || set_state(&reset, &JsValue::FALSE))
                as Box<dyn FnMut() -> Result<(), JsValue>>);
            let window = required_property(&js_sys::global(), "window", "global")?;
            function(&window, "setTimeout")?.call2(
                &window,
                &callback.into_js_value(),
                &JsValue::from_f64(1_000.0),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        call_method(&pending, "then", &[settled.into_js_value()])?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let toggle_setter = set_expanded;
    let toggle = Closure::wrap(Box::new(move || {
        let invert = Function::new_with_args("value", "return !value");
        toggle_setter.call1(&JsValue::UNDEFINED, &invert)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);

    let (head_count, tail_count) = cap_counts(max_lines, model.rows.len());
    let hidden_count = model.rows.len().saturating_sub(head_count + tail_count);
    let capped = hidden_count > 0 && !expanded;
    let head = if capped {
        &model.rows[..head_count]
    } else {
        &model.rows
    };
    let tail = if capped && tail_count > 0 {
        &model.rows[model.rows.len() - tail_count..]
    } else {
        &[]
    };
    let mut body = Vec::new();
    for row in head {
        body.push(render_diff_row(react, row)?);
    }
    if hidden_count > 0 {
        let hidden = hidden_count.to_string();
        let expand_aria = if expanded {
            "收起差异".to_owned()
        } else {
            format!("展开其余 {hidden} 行差异")
        };
        let expand_text = if expanded {
            "收起".to_owned()
        } else {
            format!("… 其余 {hidden} 行")
        };
        body.push(create_element(
            react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name_for("DiffBlock", "expand")),
                ),
                ("aria-expanded", JsValue::from_bool(expanded)),
                ("aria-label", JsValue::from_str(&expand_aria)),
                ("onClick", toggle.into_js_value()),
            ])?),
            &[JsValue::from_str(&expand_text)],
        )?);
    }
    for row in tail {
        body.push(render_diff_row(react, row)?);
    }
    let copy_button = create_element(
        react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class_name_for("DiffBlock", "copyButton")),
            ),
            ("onClick", copy.into_js_value()),
        ])?),
        &[JsValue::from_str(if copied {
            "复制成功"
        } else {
            "复制"
        })],
    )?;
    let body = create_element(
        react,
        "div",
        Some(&class_props(&class_name_for("DiffBlock", "body"))?),
        &body,
    )?;
    let footer = format!(
        "└ +{} -{} · {} file{}",
        model.added,
        model.removed,
        model.files,
        if model.files == 1 { "" } else { "s" }
    );
    let footer = create_element(
        react,
        "div",
        Some(&class_props(&class_name_for("DiffBlock", "footer"))?),
        &[JsValue::from_str(&footer)],
    )?;
    create_element(
        react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [Some(class_name_for("DiffBlock", "block")), class_name]
                        .into_iter()
                        .flatten(),
                )),
            ),
            ("data-diff", JsValue::from_str("")),
        ])?),
        &[copy_button, body, footer],
    )
}

fn build_diff_model(value: &JsValue) -> Result<DiffModel, JsValue> {
    let diffs = Array::from(value);
    let mut rows = Vec::new();
    let mut paths = Vec::<String>::new();
    let mut previous = None::<String>;
    let mut added = 0_usize;
    let mut removed = 0_usize;
    for value in diffs.iter() {
        let path = required_string(&value, "path", "DiffHunk")?;
        if !paths.contains(&path) {
            paths.push(path.clone());
        }
        rows.push(DiffRow {
            kind: if previous.as_ref() == Some(&path) {
                DiffRowKind::Gap
            } else {
                DiffRowKind::Path
            },
            text: if previous.as_ref() == Some(&path) {
                "⋯".to_owned()
            } else {
                path.clone()
            },
        });
        previous = Some(path);
        let old = Reflect::get(&value, &JsValue::from_str("oldText"))?;
        if !old.is_null() {
            let old = old
                .as_string()
                .ok_or_else(|| js_error("DiffHunk oldText must be string or null"))?;
            for text in content_lines(&old) {
                rows.push(DiffRow {
                    kind: DiffRowKind::Delete,
                    text,
                });
                removed += 1;
            }
        }
        let new = required_string(&value, "newText", "DiffHunk")?;
        for text in content_lines(&new) {
            rows.push(DiffRow {
                kind: DiffRowKind::Add,
                text,
            });
            added += 1;
        }
    }
    Ok(DiffModel {
        rows,
        added,
        removed,
        files: paths.len(),
    })
}

fn content_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n').map(str::to_owned).collect()
}

fn copy_text(rows: &[DiffRow]) -> String {
    rows.iter()
        .map(|row| match row.kind {
            DiffRowKind::Delete => format!("- {}", row.text),
            DiffRowKind::Add => format!("+ {}", row.text),
            DiffRowKind::Path | DiffRowKind::Gap => row.text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cap_counts(max_lines: f64, total: usize) -> (usize, usize) {
    if !max_lines.is_finite() || max_lines <= 0.0 {
        return (
            if max_lines.is_sign_positive() {
                total
            } else {
                0
            },
            0,
        );
    }
    let cap = format!("{:.0}", max_lines.floor())
        .parse::<usize>()
        .unwrap_or(total)
        .min(total);
    let head = cap.div_ceil(2);
    (head, cap - head)
}

fn render_diff_row(react: &JsValue, row: &DiffRow) -> Result<JsValue, JsValue> {
    let kind = match row.kind {
        DiffRowKind::Path => "path",
        DiffRowKind::Delete => "del",
        DiffRowKind::Add => "add",
        DiffRowKind::Gap => "gap",
    };
    create_element(
        react,
        "div",
        Some(&class_props(&join_classes([
            class_name_for("DiffBlock", "line"),
            class_name_for("DiffBlock", kind),
        ]))?),
        &[JsValue::from_str(&row.text)],
    )
}

fn configured_react() -> Result<JsValue, JsValue> {
    REACT.with(|slot| {
        slot.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives block module was not configured").into()
        })
    })
}

fn inject_style(component: &str, css: &str, locals: &[&str]) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = format!("@seekdeep-ai/seekdeep-client-ui-primitives/{component}.module.css");
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
        .ok_or_else(|| js_error(&format!("{owner} {key} must be a string")))
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Err(js_error(&format!("{owner} omitted {key}")))
    } else {
        Ok(value)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let output = Object::new();
    for (key, value) in entries {
        Reflect::set(&output, &JsValue::from_str(key), value)?;
    }
    Ok(output)
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

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn create_element(
    react: &JsValue,
    kind: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(&JsValue::from_str(kind));
    arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
    for child in children {
        arguments.push(child);
    }
    function(react, "createElement")?.apply(react, &arguments)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(&format!("client-ui-primitives: {message}")).into()
}
