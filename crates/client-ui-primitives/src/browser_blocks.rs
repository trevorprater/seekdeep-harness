//! Compiled structured result-card components.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect, Set};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{AnsiLine, AnsiStyle, parse_ansi_lines, write_clipboard};

const DIFF_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/DiffBlock.module.css");
const SEARCH_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/SearchBlock.module.css");
const TERMINAL_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/TerminalBlock.module.css");
const PILL_CSS: &str = include_str!("../../../packages/client/ui-primitives/src/Pill.module.css");
const STATE_DOT_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/StateDot.module.css");
const DEFAULT_DIFF_MAX_LINES: f64 = 16.0;
const DEFAULT_SEARCH_MAX_LINES: f64 = 16.0;
const DEFAULT_TERMINAL_MAX_LINES: f64 = 16.0;

thread_local! {
    static REACT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Returns the source-compatible default diff content-line cap.
#[wasm_bindgen(js_name = defaultDiffMaxLines)]
#[must_use]
pub fn default_diff_max_lines() -> f64 {
    DEFAULT_DIFF_MAX_LINES
}

/// Returns the source-compatible default search content-line cap.
#[wasm_bindgen(js_name = defaultSearchMaxLines)]
#[must_use]
pub fn default_search_max_lines() -> f64 {
    DEFAULT_SEARCH_MAX_LINES
}

/// Returns the source-compatible default terminal content-line cap.
#[wasm_bindgen(js_name = defaultTerminalMaxLines)]
#[must_use]
pub fn default_terminal_max_lines() -> f64 {
    DEFAULT_TERMINAL_MAX_LINES
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum SearchRow {
    File {
        path: String,
        count: usize,
        index: usize,
        collapsed: bool,
    },
    Match {
        line_number: i64,
        line: String,
        file_index: usize,
    },
    Path(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchModel {
    kind: String,
    rows: Vec<SearchRow>,
    copy_text: String,
    shown: usize,
    file_count: usize,
    truncated: bool,
    total: i64,
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
    )?;
    inject_style(
        "SearchBlock",
        SEARCH_CSS,
        &[
            "block",
            "header",
            "summary",
            "copyButton",
            "empty",
            "body",
            "line",
            "lineNumber",
            "fileHeader",
            "filePath",
            "fileCount",
            "expand",
        ],
    )?;
    inject_style(
        "TerminalBlock",
        TERMINAL_CSS,
        &[
            "block",
            "header",
            "prompt",
            "promptLine",
            "runState",
            "runStateLabel",
            "cwd",
            "command",
            "status",
            "copyButton",
            "empty",
            "output",
            "line",
            "expand",
        ],
    )?;
    inject_style("Pill", PILL_CSS, &["pill", "interactive", "active"])?;
    inject_style("StateDot", STATE_DOT_CSS, &["dot", "matrix", "cell"])
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

/// Returns the compiled `SearchBlock` component.
///
/// # Errors
///
/// Returns missing React configuration.
#[wasm_bindgen(js_name = searchBlockComponent)]
pub fn search_block_component() -> Result<JsValue, JsValue> {
    let react = configured_react()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_search(&react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

/// Returns the compiled `TerminalBlock` component.
///
/// # Errors
///
/// Returns missing React configuration.
#[wasm_bindgen(js_name = terminalBlockComponent)]
pub fn terminal_block_component() -> Result<JsValue, JsValue> {
    let react = configured_react()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_terminal(&react, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
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

#[allow(clippy::too_many_lines)]
fn render_search(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let (expanded, set_expanded) = use_state(react, &JsValue::FALSE)?;
    let expanded = expanded.as_bool().unwrap_or(false);
    let (collapsed, set_collapsed) = use_state(react, Set::new(&JsValue::UNDEFINED).as_ref())?;
    let collapsed = collapsed.dyn_into::<Set>()?;
    let (copied, set_copied) = use_state(react, &JsValue::FALSE)?;
    let copied = copied.as_bool().unwrap_or(false);
    let model = build_search_model(props, &collapsed)?;
    let max_lines = optional_number(props, "maxLines")?.unwrap_or(DEFAULT_SEARCH_MAX_LINES);
    let class_name = optional_string(props, "className")?;
    let empty = model.rows.is_empty();
    let copy_text = model.copy_text.clone();
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
    let hidden = model.rows.len().saturating_sub(head_count + tail_count);
    let capped = hidden > 0 && !expanded;
    let head = if capped {
        &model.rows[..head_count]
    } else {
        &model.rows
    };
    let natural_tail = if capped && tail_count > 0 {
        &model.rows[model.rows.len() - tail_count..]
    } else {
        &[]
    };
    let tail_header = natural_tail.first().and_then(|row| {
        let SearchRow::Match { file_index, .. } = row else {
            return None;
        };
        let already = head
            .iter()
            .any(|row| matches!(row, SearchRow::File { index, .. } if index == file_index));
        if already {
            None
        } else {
            model
                .rows
                .iter()
                .find(|row| matches!(row, SearchRow::File { index, .. } if index == file_index))
        }
    });
    let tail = if tail_header.is_some() {
        natural_tail.get(1..).unwrap_or_default()
    } else {
        natural_tail
    };

    let summary = search_summary(&model);
    let summary = create_element(
        react,
        "span",
        Some(&class_props(&class_name_for("SearchBlock", "summary"))?),
        &[JsValue::from_str(&summary)],
    )?;
    let mut header_children = vec![summary];
    if !empty {
        header_children.push(create_element(
            react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name_for("SearchBlock", "copyButton")),
                ),
                ("onClick", copy.into_js_value()),
            ])?),
            &[JsValue::from_str(if copied {
                "复制成功"
            } else {
                "复制"
            })],
        )?);
    }
    let header = create_element(
        react,
        "div",
        Some(&class_props(&class_name_for("SearchBlock", "header"))?),
        &header_children,
    )?;
    let result = if empty {
        create_element(
            react,
            "div",
            Some(&class_props(&class_name_for("SearchBlock", "empty"))?),
            &[JsValue::from_str("无结果")],
        )?
    } else {
        let mut rows = Vec::new();
        for row in head {
            rows.push(render_search_entry(
                react,
                row,
                &set_collapsed,
                &search_row_key(row),
            )?);
        }
        if hidden > 0 {
            let hidden = hidden.to_string();
            let aria = if expanded {
                "收起结果".to_owned()
            } else {
                format!("展开其余 {hidden} 行结果")
            };
            let text = if expanded {
                "收起".to_owned()
            } else {
                format!("… 其余 {hidden} 行")
            };
            rows.push(create_element(
                react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str(&class_name_for("SearchBlock", "expand")),
                    ),
                    ("aria-expanded", JsValue::from_bool(expanded)),
                    ("aria-label", JsValue::from_str(&aria)),
                    ("onClick", toggle.into_js_value()),
                ])?),
                &[JsValue::from_str(&text)],
            )?);
        }
        if let Some(header) = tail_header {
            rows.push(render_search_entry(
                react,
                header,
                &set_collapsed,
                &format!("tailHeader:{}", search_row_key(header)),
            )?);
        }
        for row in tail {
            rows.push(render_search_entry(
                react,
                row,
                &set_collapsed,
                &search_row_key(row),
            )?);
        }
        create_element(
            react,
            "div",
            Some(&class_props(&class_name_for("SearchBlock", "body"))?),
            &rows,
        )?
    };
    create_element(
        react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [Some(class_name_for("SearchBlock", "block")), class_name]
                        .into_iter()
                        .flatten(),
                )),
            ),
            ("data-search", JsValue::from_str(&model.kind)),
        ])?),
        &[header, result],
    )
}

#[allow(clippy::too_many_lines)]
fn render_terminal(react: &JsValue, props: &JsValue) -> Result<JsValue, JsValue> {
    let command = required_string(props, "command", "TerminalBlock props")?;
    let cwd = optional_string(props, "cwd")?;
    let home = optional_string(props, "home")?;
    let text = optional_string(props, "output")?.unwrap_or_default();
    let exit_code = optional_number(props, "exitCode")?;
    let signal = optional_string(props, "signal")?;
    let running = Reflect::get(props, &JsValue::from_str("running"))?
        .as_bool()
        .unwrap_or(false);
    let max_lines = optional_number(props, "maxLines")?.unwrap_or(DEFAULT_TERMINAL_MAX_LINES);
    let class_name = optional_string(props, "className")?;
    let labels = Reflect::get(props, &JsValue::from_str("labels"))?;

    let mut lines = parse_ansi_lines(&text);
    if lines.len() > 1
        && lines
            .last()
            .is_some_and(|line| line.iter().all(|span| span.text.is_empty()))
    {
        lines.pop();
    }
    let empty = lines
        .iter()
        .all(|line| line.iter().all(|span| span.text.trim().is_empty()));
    let (expanded, set_expanded) = use_state(react, &JsValue::FALSE)?;
    let expanded = expanded.as_bool().unwrap_or(false);
    let (copied, set_copied) = use_state(react, &JsValue::FALSE)?;
    let copied = copied.as_bool().unwrap_or(false);
    let status = terminal_status(&labels, exit_code, signal.as_deref())?;
    let (state, state_label) = terminal_state(&labels, running, status.is_some())?;

    let prompt_body = command.strip_suffix('\n').unwrap_or(&command);
    let command_lines = prompt_body.split('\n').collect::<Vec<_>>();
    let mut prompt_rows = Vec::new();
    for (index, line) in command_lines.iter().enumerate() {
        let mut children = Vec::new();
        if index == 0 {
            children.push(render_terminal_state_dot(
                react,
                state,
                &class_name_for("TerminalBlock", "runState"),
            )?);
        }
        let prompt = if index > 0 || cwd.is_none() {
            "$".to_owned()
        } else {
            prompt_label(cwd.as_deref().unwrap_or_default(), home.as_deref())
        };
        children.push(create_element(
            react,
            "span",
            Some(&class_props(&class_name_for("TerminalBlock", "cwd"))?),
            &[JsValue::from_str(&prompt)],
        )?);
        children.push(create_element(
            react,
            "span",
            Some(&class_props(&class_name_for("TerminalBlock", "command"))?),
            &[JsValue::from_str(line)],
        )?);
        prompt_rows.push(create_element(
            react,
            "div",
            Some(&object(&[
                ("key", JsValue::from_f64(index_to_f64(index))),
                (
                    "className",
                    JsValue::from_str(&class_name_for("TerminalBlock", "promptLine")),
                ),
            ])?),
            &children,
        )?);
    }
    let mut prompt_children = vec![create_element(
        react,
        "span",
        Some(&class_props(&class_name_for(
            "TerminalBlock",
            "runStateLabel",
        ))?),
        &[JsValue::from_str(&state_label)],
    )?];
    prompt_children.extend(prompt_rows);
    let prompt = create_element(
        react,
        "div",
        Some(&class_props(&class_name_for("TerminalBlock", "prompt"))?),
        &prompt_children,
    )?;
    let mut header_children = vec![prompt];
    if let Some(status) = status {
        header_children.push(create_element(
            react,
            "span",
            Some(&class_props(&join_classes([
                class_name_for("Pill", "pill"),
                class_name_for("TerminalBlock", "status"),
            ]))?),
            &[JsValue::from_str(&status)],
        )?);
    }
    if !running && !empty {
        let copy_text = text.clone();
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
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            call_method(&pending, "then", &[settled.into_js_value()])?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        header_children.push(create_element(
            react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name_for("TerminalBlock", "copyButton")),
                ),
                ("onClick", copy.into_js_value()),
            ])?),
            &[JsValue::from_str(&terminal_label(
                &labels,
                if copied { "copied" } else { "copy" },
                if copied { "复制成功" } else { "复制" },
            )?)],
        )?);
    }
    let header = create_element(
        react,
        "div",
        Some(&class_props(&class_name_for("TerminalBlock", "header"))?),
        &header_children,
    )?;
    let mut root_children = vec![header];
    if !running {
        if empty {
            root_children.push(create_element(
                react,
                "div",
                Some(&class_props(&class_name_for("TerminalBlock", "empty"))?),
                &[JsValue::from_str(&terminal_label(
                    &labels,
                    "noOutput",
                    "无输出",
                )?)],
            )?);
        } else {
            root_children.push(render_terminal_output(
                react,
                &lines,
                max_lines,
                expanded,
                set_expanded,
                &labels,
            )?);
        }
    }
    create_element(
        react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [Some(class_name_for("TerminalBlock", "block")), class_name]
                        .into_iter()
                        .flatten(),
                )),
            ),
            ("data-terminal", JsValue::from_str("")),
            (
                "data-running",
                if running {
                    JsValue::from_str("")
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &root_children,
    )
}

fn render_terminal_output(
    react: &JsValue,
    lines: &[AnsiLine],
    max_lines: f64,
    expanded: bool,
    set_expanded: Function,
    labels: &JsValue,
) -> Result<JsValue, JsValue> {
    let (head_count, tail_count) = cap_counts(max_lines, lines.len());
    let hidden = lines.len().saturating_sub(head_count + tail_count);
    let capped = hidden > 0 && !expanded;
    let head = if capped { &lines[..head_count] } else { lines };
    let tail = if capped && tail_count > 0 {
        &lines[lines.len() - tail_count..]
    } else {
        &[]
    };
    let mut rows = Vec::new();
    for (index, line) in head.iter().enumerate() {
        rows.push(render_terminal_line(react, line, index)?);
    }
    if hidden > 0 {
        let setter = set_expanded;
        let toggle = Closure::wrap(Box::new(move || {
            let invert = Function::new_with_args("value", "return !value");
            setter.call1(&JsValue::UNDEFINED, &invert)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let aria = if expanded {
            terminal_label(labels, "collapseAria", "收起输出")?
        } else {
            terminal_count_label(
                labels,
                "expandAria",
                hidden,
                &format!("展开其余 {hidden} 行输出"),
            )?
        };
        let text = if expanded {
            terminal_label(labels, "collapse", "收起")?
        } else {
            terminal_count_label(labels, "expand", hidden, &format!("… 其余 {hidden} 行"))?
        };
        rows.push(create_element(
            react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class_name_for("TerminalBlock", "expand")),
                ),
                ("aria-expanded", JsValue::from_bool(expanded)),
                ("aria-label", JsValue::from_str(&aria)),
                ("onClick", toggle.into_js_value()),
            ])?),
            &[JsValue::from_str(&text)],
        )?);
    }
    for (index, line) in tail.iter().enumerate() {
        rows.push(render_terminal_line(react, line, index)?);
    }
    create_element(
        react,
        "div",
        Some(&class_props(&class_name_for("TerminalBlock", "output"))?),
        &rows,
    )
}

fn render_terminal_line(react: &JsValue, line: &AnsiLine, key: usize) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    for (index, span) in line.iter().enumerate() {
        if let Some(style) = &span.style {
            children.push(create_element(
                react,
                "span",
                Some(&object(&[
                    ("key", JsValue::from_f64(index_to_f64(index))),
                    ("style", ansi_style(style)?.into()),
                ])?),
                &[JsValue::from_str(&span.text)],
            )?);
        } else {
            children.push(JsValue::from_str(&span.text));
        }
    }
    create_element(
        react,
        "div",
        Some(&object(&[
            ("key", JsValue::from_f64(index_to_f64(key))),
            (
                "className",
                JsValue::from_str(&class_name_for("TerminalBlock", "line")),
            ),
        ])?),
        &children,
    )
}

fn ansi_style(style: &AnsiStyle) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in [
        ("color", style.color.as_deref().map(JsValue::from_str)),
        (
            "backgroundColor",
            style.background_color.as_deref().map(JsValue::from_str),
        ),
        (
            "fontWeight",
            style
                .font_weight
                .map(|value| JsValue::from_f64(f64::from(value))),
        ),
        ("opacity", style.opacity.map(JsValue::from_f64)),
        (
            "fontStyle",
            style.font_style.as_deref().map(JsValue::from_str),
        ),
        (
            "textDecoration",
            style.text_decoration.as_deref().map(JsValue::from_str),
        ),
        (
            "visibility",
            style.visibility.as_deref().map(JsValue::from_str),
        ),
    ] {
        if let Some(value) = value {
            Reflect::set(&object, &JsValue::from_str(key), &value)?;
        }
    }
    Ok(object)
}

fn render_terminal_state_dot(
    react: &JsValue,
    state: &str,
    class_name: &str,
) -> Result<JsValue, JsValue> {
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
            cells.push(create_element(
                react,
                "rect",
                Some(&object(&[
                    ("key", JsValue::from_str(&format!("{x}-{y}"))),
                    (
                        "className",
                        JsValue::from_str(&class_name_for("StateDot", "cell")),
                    ),
                    ("x", JsValue::from_f64(f64::from(x))),
                    ("y", JsValue::from_f64(f64::from(y))),
                    ("width", JsValue::from_str("2")),
                    ("height", JsValue::from_str("2")),
                    (
                        "style",
                        object(&[("animationDelay", JsValue::from_str(&format!("{delay}ms")))])?
                            .into(),
                    ),
                ])?),
                &[],
            )?);
        }
        return create_element(
            react,
            "svg",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&join_classes([
                        class_name_for("StateDot", "matrix"),
                        class_name.to_owned(),
                    ])),
                ),
                ("data-state", JsValue::from_str("ongoing")),
                ("width", JsValue::from_f64(10.0)),
                ("height", JsValue::from_f64(10.0)),
                ("viewBox", JsValue::from_str("0 0 10 10")),
                ("shapeRendering", JsValue::from_str("crispEdges")),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &cells,
        );
    }
    create_element(
        react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes([
                    class_name_for("StateDot", "dot"),
                    class_name.to_owned(),
                ])),
            ),
            ("data-state", JsValue::from_str(state)),
            (
                "style",
                object(&[
                    ("width", JsValue::from_f64(10.0)),
                    ("height", JsValue::from_f64(10.0)),
                ])?
                .into(),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )
}

fn prompt_label(cwd: &str, home: Option<&str>) -> String {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    if home.is_some_and(|home| trimmed == home.trim_end_matches(['/', '\\'])) {
        return "~".to_owned();
    }
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

fn terminal_status(
    labels: &JsValue,
    exit_code: Option<f64>,
    signal: Option<&str>,
) -> Result<Option<String>, JsValue> {
    if let Some(signal) = signal {
        return terminal_dynamic_label(
            labels,
            "signal",
            &JsValue::from_str(signal),
            &format!("信号 {signal}"),
        )
        .map(Some);
    }
    if let Some(exit_code) = exit_code.filter(|exit_code| *exit_code != 0.0) {
        return terminal_dynamic_label(
            labels,
            "exitCode",
            &JsValue::from_f64(exit_code),
            &format!("退出码 {}", javascript_number(exit_code)),
        )
        .map(Some);
    }
    Ok(None)
}

fn terminal_state(
    labels: &JsValue,
    running: bool,
    failed: bool,
) -> Result<(&'static str, String), JsValue> {
    if running {
        Ok(("ongoing", terminal_label(labels, "running", "运行中")?))
    } else if failed {
        Ok(("error", terminal_label(labels, "failed", "失败")?))
    } else {
        Ok(("done", terminal_label(labels, "done", "已完成")?))
    }
}

fn terminal_label(labels: &JsValue, key: &str, fallback: &str) -> Result<String, JsValue> {
    if labels.is_null() || labels.is_undefined() {
        return Ok(fallback.to_owned());
    }
    let value = Reflect::get(labels, &JsValue::from_str(key))?;
    if value.is_undefined() {
        Ok(fallback.to_owned())
    } else {
        value
            .as_string()
            .ok_or_else(|| js_error(&format!("TerminalBlock label {key} must be a string")))
    }
}

fn terminal_dynamic_label(
    labels: &JsValue,
    key: &str,
    argument: &JsValue,
    fallback: &str,
) -> Result<String, JsValue> {
    if labels.is_null() || labels.is_undefined() {
        return Ok(fallback.to_owned());
    }
    let value = Reflect::get(labels, &JsValue::from_str(key))?;
    if value.is_undefined() {
        return Ok(fallback.to_owned());
    }
    value
        .dyn_into::<Function>()?
        .call1(&JsValue::UNDEFINED, argument)?
        .as_string()
        .ok_or_else(|| js_error(&format!("TerminalBlock label {key} must return a string")))
}

fn terminal_count_label(
    labels: &JsValue,
    key: &str,
    hidden: usize,
    fallback: &str,
) -> Result<String, JsValue> {
    terminal_dynamic_label(
        labels,
        key,
        &JsValue::from_f64(index_to_f64(hidden)),
        fallback,
    )
}

fn javascript_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn build_search_model(props: &JsValue, collapsed: &Set) -> Result<SearchModel, JsValue> {
    let kind = required_string(props, "kind", "SearchBlock props")?;
    let truncated = Reflect::get(props, &JsValue::from_str("truncated"))?
        .as_bool()
        .unwrap_or(false);
    let total = required_number_i64(props, "total", "SearchBlock props")?;
    if kind == "paths" {
        let paths = Array::from(&required_property(props, "paths", "SearchBlock props")?)
            .iter()
            .map(|value| {
                value
                    .as_string()
                    .ok_or_else(|| js_error("search path must be a string"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(SearchModel {
            kind,
            rows: paths.iter().cloned().map(SearchRow::Path).collect(),
            copy_text: paths.join("\n"),
            shown: paths.len(),
            file_count: 0,
            truncated,
            total,
        });
    }
    if kind != "matches" {
        return Err(js_error("SearchBlock kind must be matches or paths"));
    }
    let files = Array::from(&required_property(props, "files", "SearchBlock props")?);
    let mut rows = Vec::new();
    let mut copy_groups = Vec::new();
    let mut shown = 0_usize;
    for (index, file) in files.iter().enumerate() {
        let path = required_string(&file, "path", "SearchFileGroup")?;
        let matches = Array::from(&required_property(&file, "matches", "SearchFileGroup")?);
        let is_collapsed = collapsed.has(&JsValue::from_f64(index_to_f64(index)));
        let match_count = usize::try_from(matches.length()).expect("u32 fits usize");
        rows.push(SearchRow::File {
            path: path.clone(),
            count: match_count,
            index,
            collapsed: is_collapsed,
        });
        let mut copy_lines = vec![path.clone()];
        for value in matches.iter() {
            let line_number = required_number_i64(&value, "lineNumber", "Search line")?;
            let line = required_string(&value, "line", "Search line")?;
            copy_lines.push(format!("{line_number}: {line}"));
            shown += 1;
            if !is_collapsed {
                rows.push(SearchRow::Match {
                    line_number,
                    line,
                    file_index: index,
                });
            }
        }
        copy_groups.push(copy_lines.join("\n"));
    }
    Ok(SearchModel {
        kind,
        rows,
        copy_text: copy_groups.join("\n\n"),
        shown,
        file_count: usize::try_from(files.length()).expect("u32 fits usize"),
        truncated,
        total,
    })
}

fn search_summary(model: &SearchModel) -> String {
    let count = if model.truncated {
        format!("显示 {} / 共 {}", model.shown, model.total)
    } else {
        model.shown.to_string()
    };
    if model.kind == "paths" {
        format!("{count} 个路径")
    } else {
        format!("{count} 处匹配 · {} 个文件", model.file_count)
    }
}

fn render_search_row(
    react: &JsValue,
    row: &SearchRow,
    set_collapsed: &Function,
) -> Result<JsValue, JsValue> {
    match row {
        SearchRow::Path(path) => create_element(
            react,
            "div",
            Some(&class_props(&class_name_for("SearchBlock", "line"))?),
            &[JsValue::from_str(path)],
        ),
        SearchRow::Match {
            line_number, line, ..
        } => {
            let number = create_element(
                react,
                "span",
                Some(&class_props(&class_name_for("SearchBlock", "lineNumber"))?),
                &[JsValue::from_str(&format!("{line_number}: "))],
            )?;
            create_element(
                react,
                "div",
                Some(&class_props(&class_name_for("SearchBlock", "line"))?),
                &[number, JsValue::from_str(line)],
            )
        }
        SearchRow::File {
            path,
            count,
            index,
            collapsed,
        } => {
            let setter = set_collapsed.clone();
            let index = *index;
            let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let updater = Closure::wrap(Box::new(move |previous: JsValue| -> JsValue {
                    let next = Set::new(&previous);
                    let key = JsValue::from_f64(index_to_f64(index));
                    if next.has(&key) {
                        next.delete(&key);
                    } else {
                        next.add(&key);
                    }
                    next.into()
                })
                    as Box<dyn FnMut(JsValue) -> JsValue>);
                setter.call1(&JsValue::UNDEFINED, &updater.into_js_value())?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let path = create_element(
                react,
                "span",
                Some(&class_props(&class_name_for("SearchBlock", "filePath"))?),
                &[JsValue::from_str(path)],
            )?;
            let count = create_element(
                react,
                "span",
                Some(&class_props(&class_name_for("SearchBlock", "fileCount"))?),
                &[JsValue::from_str(&count.to_string())],
            )?;
            create_element(
                react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str(&class_name_for("SearchBlock", "fileHeader")),
                    ),
                    ("aria-expanded", JsValue::from_bool(!collapsed)),
                    ("onClick", click.into_js_value()),
                ])?),
                &[path, count],
            )
        }
    }
}

fn render_search_entry(
    react: &JsValue,
    row: &SearchRow,
    set_collapsed: &Function,
    key: &str,
) -> Result<JsValue, JsValue> {
    create_element(
        react,
        "div",
        Some(&object(&[("key", JsValue::from_str(key))])?),
        &[render_search_row(react, row, set_collapsed)?],
    )
}

fn search_row_key(row: &SearchRow) -> String {
    match row {
        SearchRow::Match {
            line_number,
            file_index,
            ..
        } => format!("match:{file_index}:{line_number}"),
        SearchRow::File { index, .. } => format!("file:{index}"),
        SearchRow::Path(path) => format!("path:{path}"),
    }
}

fn index_to_f64(index: usize) -> f64 {
    index.to_string().parse().expect("usize renders as f64")
}

fn required_number_i64(value: &JsValue, key: &str, owner: &str) -> Result<i64, JsValue> {
    let number = required_property(value, key, owner)?
        .as_f64()
        .filter(|number| number.is_finite() && number.fract() == 0.0)
        .ok_or_else(|| js_error(&format!("{owner} {key} must be an integer")))?;
    format!("{number:.0}")
        .parse()
        .map_err(|_| js_error(&format!("{owner} {key} is outside i64")))
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
    if max_lines.is_nan() || max_lines == f64::INFINITY {
        return (total, 0);
    }
    if max_lines == f64::NEG_INFINITY || max_lines <= 0.0 {
        return (0, 0);
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
