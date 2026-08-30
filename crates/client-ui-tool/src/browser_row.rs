//! Compiled shared Tool summary row and expanded card body.

use std::cell::RefCell;

use js_sys::{Array, Function, Object};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser::{
        bool_or_undefined, class_props, create_element, extend_object, inject_style, object,
        optional_property, required_function, required_property, required_string, tag, translated,
        translated_with,
    },
    browser_apply::{BrowserModules, configured_modules},
};

const TOOL_ROW_CSS: &str =
    include_str!("../../../packages/client/ui-tool/src/client/tool/components/ToolRow.module.css");

const ROOT: &str = "seekdeep-tool-row-root";
const ROW: &str = "seekdeep-tool-row-row";
const LEADING: &str = "seekdeep-tool-row-leading";
const CHEVRON: &str = "seekdeep-tool-row-chevron";
const TITLE: &str = "seekdeep-tool-row-title";
const SEP: &str = "seekdeep-tool-row-sep";
const SUMMARY: &str = "seekdeep-tool-row-summary";
const SUMMARY_SUFFIX: &str = "seekdeep-tool-row-summarySuffix";
const FILE_LINK: &str = "seekdeep-tool-row-fileLink";
const ERROR_SUMMARY: &str = "seekdeep-tool-row-errorSummary";
const BODY_WRAP: &str = "seekdeep-tool-row-bodyWrap";
const INSPECT_BUTTON: &str = "seekdeep-tool-row-inspectButton";
const BODY_SCROLL: &str = "seekdeep-tool-row-bodyScroll";
const IO_CARD: &str = "seekdeep-tool-row-ioCard";
const IO_SECTION: &str = "seekdeep-tool-row-ioSection";
const IO_LABEL: &str = "seekdeep-tool-row-ioLabel";
const IO_DIVIDER: &str = "seekdeep-tool-row-ioDivider";
const IO_TEXT: &str = "seekdeep-tool-row-ioText";
const CODE_BODY: &str = "seekdeep-tool-row-codeBody";
const TERMINAL_BODY: &str = "seekdeep-tool-row-terminalBody";
const DIFF_BODY: &str = "seekdeep-tool-row-diffBody";
const READ_BODY: &str = "seekdeep-tool-row-readBody";
const SEARCH_BODY: &str = "seekdeep-tool-row-searchBody";
const WEB_BODY: &str = "seekdeep-tool-row-webBody";
const SEARCH_RECOVERY: &str = "seekdeep-tool-row-searchRecovery";
const VISUALLY_HIDDEN: &str = "seekdeep-tool-row-visuallyHidden";

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

pub(crate) fn configure_tool_row_component() -> Result<(), JsValue> {
    let modules = configured_modules()?;
    inject_style(
        "ToolRow",
        TOOL_ROW_CSS,
        &[
            ("root", ROOT),
            ("row", ROW),
            ("leading", LEADING),
            ("chevron", CHEVRON),
            ("title", TITLE),
            ("sep", SEP),
            ("summary", SUMMARY),
            ("summarySuffix", SUMMARY_SUFFIX),
            ("fileLink", FILE_LINK),
            ("errorSummary", ERROR_SUMMARY),
            ("bodyWrap", BODY_WRAP),
            ("inspectButton", INSPECT_BUTTON),
            ("bodyScroll", BODY_SCROLL),
            ("ioCard", IO_CARD),
            ("ioSection", IO_SECTION),
            ("ioLabel", IO_LABEL),
            ("ioDivider", IO_DIVIDER),
            ("ioText", IO_TEXT),
            ("codeBody", CODE_BODY),
            ("terminalBody", TERMINAL_BODY),
            ("diffBody", DIFF_BODY),
            ("readBody", READ_BODY),
            ("searchBody", SEARCH_BODY),
            ("webBody", WEB_BODY),
            ("searchRecovery", SEARCH_RECOVERY),
            ("visuallyHidden", VISUALLY_HIDDEN),
        ],
    )?;
    let render_modules = modules;
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_tool_row(&render_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled shared `ToolRow` component.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = toolRowComponent)]
pub fn tool_row_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|configured| {
        configured
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-tool ToolRow was not configured").into())
    })
}

#[allow(clippy::too_many_lines)] // The closed row body mirrors one source component.
fn render_tool_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "ToolRow props")?;
    let variant = required_string(props, "variant", "ToolRow props")?;
    let state = required_string(props, "state", "ToolRow props")?;
    if !matches!(
        variant.as_str(),
        "search" | "read" | "bash" | "write" | "edit" | "code" | "others"
    ) {
        return Err(js_sys::TypeError::new("ToolRow variant is unknown").into());
    }
    if !matches!(state.as_str(), "running" | "ok" | "error" | "stopped") {
        return Err(js_sys::TypeError::new("ToolRow state is unknown").into());
    }
    let icon = required_property(props, "icon", "ToolRow props")?;
    let title = required_property(props, "title", "ToolRow props")?;
    let summary = required_string(props, "summary", "ToolRow props")?;
    let body = required_property(props, "body", "ToolRow props")?;
    let output = optional_property(props, "output")?;
    let terminal = optional_property(props, "terminal")?;
    let diff = optional_property(props, "diff")?;
    let read = optional_property(props, "read")?;
    let search = optional_property(props, "search")?;
    let web = optional_property(props, "web")?;
    let card_present =
        terminal.is_some() || diff.is_some() || read.is_some() || search.is_some() || web.is_some();
    let expandable = !body.is_null() || output.is_some() || card_present;
    let state_pair = Array::from(
        &required_function(&modules.react, "useState", "React")?
            .call1(&modules.react, &JsValue::FALSE)?,
    );
    let expanded = state_pair.get(0).as_bool().unwrap_or(false);
    let set_expanded = state_pair
        .get(1)
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new("React useState setter must be a function"))?;
    let open = expanded && expandable;
    let update = Closure::wrap(Box::new(move |previous: JsValue| -> bool {
        !previous.as_bool().unwrap_or(false)
    }) as Box<dyn FnMut(JsValue) -> bool>);
    let toggle_setter = set_expanded;
    let update_value = update.into_js_value();
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        toggle_setter.call1(&JsValue::UNDEFINED, &update_value)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let error_summary =
        optional_property(props, "errorSummary")?.and_then(|value| value.as_string());
    let failure_line = (state == "error").then(|| error_summary.clone()).flatten();
    let summary_text = failure_line.as_deref().unwrap_or(&summary);
    let suffix = if failure_line.is_none() {
        optional_property(props, "summarySuffix")?.and_then(|value| value.as_string())
    } else {
        None
    };
    let file_path = optional_property(props, "filePath")?.and_then(|value| value.as_string());
    let open_file = optional_property(props, "onOpenFile")?
        .map(|value| {
            value
                .dyn_into::<Function>()
                .map_err(|_| js_sys::TypeError::new("ToolRow onOpenFile must be a function"))
        })
        .transpose()?;
    let file_link = file_path.is_some() && open_file.is_some() && failure_line.is_none();

    let collapsed = if summary_text.is_empty() {
        JsValue::FALSE
    } else {
        let mut children = vec![tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str(SEP)),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?];
        if file_link {
            let path = file_path
                .clone()
                .ok_or_else(|| js_sys::Error::new("ToolRow file link is missing its path"))?;
            let opener = open_file
                .clone()
                .ok_or_else(|| js_sys::Error::new("ToolRow file link is missing its opener"))?;
            let click_path = path.clone();
            let click = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                required_function(&event, "stopPropagation", "Mouse event")?.call0(&event)?;
                opener.call1(&JsValue::UNDEFINED, &JsValue::from_str(&click_path))?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                let key = required_string(&event, "key", "Keyboard event")?;
                if matches!(key.as_str(), "Enter" | " ") {
                    required_function(&event, "stopPropagation", "Keyboard event")?
                        .call0(&event)?;
                }
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            children.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str(FILE_LINK)),
                    ("onClick", click.into_js_value()),
                    ("onKeyDown", keydown.into_js_value()),
                ])?),
                &[JsValue::from_str(summary_text)],
            )?);
        } else {
            let class_name = if failure_line.is_some() {
                format!("{SUMMARY} {ERROR_SUMMARY}")
            } else {
                SUMMARY.to_owned()
            };
            children.push(tag(
                &modules.react,
                "span",
                Some(&class_props(&class_name)?),
                &[JsValue::from_str(summary_text)],
            )?);
        }
        if let Some(suffix) = suffix {
            children.push(tag(
                &modules.react,
                "span",
                Some(&class_props(SUMMARY_SUFFIX)?),
                &[JsValue::from_str(&suffix)],
            )?);
        }
        create_element(&modules.react, &modules.fragment, None, &children)?
    };

    let body_content = render_expanded_body(
        modules,
        &translate,
        &variant,
        &state,
        &body,
        output.as_ref(),
        terminal.as_ref(),
        diff.as_ref(),
        read.as_ref(),
        search.as_ref(),
        web.as_ref(),
    )?;
    let mut body_children = vec![body_content];
    if let Some(inspect) = optional_property(props, "inspect")? {
        body_children.push(tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(INSPECT_BUTTON)),
                ("onClick", inspect),
            ])?),
            &[
                create_element(
                    &modules.react,
                    &modules.primitive("IconInspectOutline12")?,
                    None,
                    &[],
                )?,
                JsValue::from_str("Inspect"),
            ],
        )?);
    }
    let body_wrap = tag(
        &modules.react,
        "div",
        Some(&class_props(BODY_WRAP)?),
        &body_children,
    )?;

    let leading = leading_for(modules, &state, icon)?;
    let disclosure = create_element(
        &modules.react,
        &modules.primitive("DisclosureRow")?,
        Some(&object(&[
            ("rowClassName", JsValue::from_str(ROW)),
            ("leadingClassName", JsValue::from_str(LEADING)),
            ("titleClassName", JsValue::from_str(TITLE)),
            ("chevronClassName", JsValue::from_str(CHEVRON)),
            ("icon", leading),
            ("title", title),
            ("open", JsValue::from_bool(open)),
            ("expandable", JsValue::from_bool(expandable)),
            ("expandOnRowClick", JsValue::TRUE),
            ("keepContentWhenOpen", JsValue::TRUE),
            ("onToggle", toggle.into_js_value()),
            ("collapsedContent", collapsed),
        ])?),
        &[body_wrap],
    )?;
    let mut children = Vec::new();
    if let Some(status) = state_status(&translate, &state)? {
        children.push(tag(
            &modules.react,
            "span",
            Some(&class_props(VISUALLY_HIDDEN)?),
            &[status],
        )?);
    }
    children.push(disclosure);
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("className", JsValue::from_str(ROOT)),
            ("data-variant", JsValue::from_str(&variant)),
            (
                "data-tool",
                optional_property(props, "toolName")?.unwrap_or(JsValue::UNDEFINED),
            ),
            ("data-state", JsValue::from_str(&state)),
        ])?),
        &children,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // Closed card precedence mirrors one source component.
fn render_expanded_body(
    modules: &BrowserModules,
    translate: &Function,
    variant: &str,
    state: &str,
    body: &JsValue,
    output: Option<&JsValue>,
    terminal: Option<&JsValue>,
    diff: Option<&JsValue>,
    read: Option<&JsValue>,
    search: Option<&JsValue>,
    web: Option<&JsValue>,
) -> Result<JsValue, JsValue> {
    if let Some(terminal) = terminal {
        let card = required_property(terminal, "card", "Terminal card model")?;
        let props = extend_object(
            &card,
            &[
                ("maxLines", JsValue::from_f64(f64::INFINITY)),
                ("labels", terminal_block_labels(translate)?.into()),
                ("className", JsValue::from_str(TERMINAL_BODY)),
            ],
        )?;
        return create_element(
            &modules.react,
            &modules.primitive("TerminalBlock")?,
            Some(&props),
            &[],
        );
    }
    if let Some(diff) = diff {
        let props = extend_object(
            &required_property(diff, "card", "Diff card model")?,
            &[
                ("maxLines", JsValue::from_f64(8.0)),
                ("className", JsValue::from_str(DIFF_BODY)),
            ],
        )?;
        return create_element(
            &modules.react,
            &modules.primitive("DiffBlock")?,
            Some(&props),
            &[],
        );
    }
    if let Some(read) = read {
        let props = extend_object(
            read,
            &[
                ("maxLines", JsValue::from_f64(8.0)),
                ("className", JsValue::from_str(READ_BODY)),
            ],
        )?;
        return create_element(
            &modules.react,
            &modules.primitive("ReadBlock")?,
            Some(&props),
            &[],
        );
    }
    if let Some(search) = search {
        let props = extend_object(
            &required_property(search, "card", "Search card model")?,
            &[
                ("maxLines", JsValue::from_f64(8.0)),
                ("className", JsValue::from_str(SEARCH_BODY)),
            ],
        )?;
        let card = create_element(
            &modules.react,
            &modules.primitive("SearchBlock")?,
            Some(&props),
            &[],
        )?;
        let recovery = optional_property(search, "recovery")?;
        if let Some(recovery) = recovery {
            let footer = tag(
                &modules.react,
                "div",
                Some(&class_props(SEARCH_RECOVERY)?),
                &[recovery],
            )?;
            return create_element(&modules.react, &modules.fragment, None, &[card, footer]);
        }
        return Ok(card);
    }
    if let Some(web) = web {
        let props = extend_object(web, &[("className", JsValue::from_str(WEB_BODY))])?;
        return create_element(
            &modules.react,
            &modules.primitive("WebBlock")?,
            Some(&props),
            &[],
        );
    }
    let mut children = Vec::new();
    if variant == "code" && !body.is_null() {
        let code = body
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("ToolRow code body must be a string"))?;
        let code_block = create_element(
            &modules.react,
            &modules.primitive("CodeBlock")?,
            Some(&object(&[
                ("code", JsValue::from_str(&code)),
                ("lang", JsValue::from_str("typescript")),
                ("copyLabel", translated(translate, "copy")?),
                ("copiedLabel", translated(translate, "copied")?),
                ("className", JsValue::from_str(CODE_BODY)),
            ])?),
            &[],
        )?;
        children.push(tag(
            &modules.react,
            "div",
            Some(&class_props(BODY_SCROLL)?),
            &[code_block],
        )?);
    }
    let card_body = (variant != "code" && !body.is_null()).then_some(body);
    if card_body.is_some() || output.is_some() {
        let mut sections = Vec::new();
        if let Some(body) = card_body {
            sections.push(io_section(modules, "IN", body, false)?);
        }
        if card_body.is_some() && output.is_some() {
            sections.push(tag(
                &modules.react,
                "span",
                Some(&object(&[
                    ("className", JsValue::from_str(IO_DIVIDER)),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[],
            )?);
        }
        if let Some(output) = output {
            sections.push(io_section(modules, "OUT", output, state == "error")?);
        }
        children.push(tag(
            &modules.react,
            "div",
            Some(&class_props(IO_CARD)?),
            &sections,
        )?);
    }
    create_element(&modules.react, &modules.fragment, None, &children)
}

fn io_section(
    modules: &BrowserModules,
    label: &str,
    value: &JsValue,
    error: bool,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "div",
        Some(&class_props(IO_SECTION)?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class_props(IO_LABEL)?),
                &[JsValue::from_str(label)],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[
                    ("className", JsValue::from_str(IO_TEXT)),
                    ("data-error", bool_or_undefined(error)),
                ])?),
                std::slice::from_ref(value),
            )?,
        ],
    )
}

fn leading_for(modules: &BrowserModules, state: &str, icon: JsValue) -> Result<JsValue, JsValue> {
    let dot = match state {
        "error" => Some("error"),
        "stopped" => Some("warning"),
        _ => None,
    };
    match dot {
        Some(dot) => create_element(
            &modules.react,
            &modules.primitive("StateDot")?,
            Some(&object(&[("state", JsValue::from_str(dot))])?),
            &[],
        ),
        None => Ok(icon),
    }
}

fn state_status(translate: &Function, state: &str) -> Result<Option<JsValue>, JsValue> {
    let key = match state {
        "running" => Some("row.running"),
        "error" => Some("row.failed"),
        "stopped" => Some("row.stopped"),
        _ => None,
    };
    key.map(|key| translated(translate, key)).transpose()
}

pub(crate) fn terminal_block_labels(translate: &Function) -> Result<Object, JsValue> {
    let signal_translate = translate.clone();
    let signal = Closure::wrap(Box::new(move |value: JsValue| -> Result<JsValue, JsValue> {
        translated_with(
            &signal_translate,
            "terminal.signal",
            &object(&[("signal", value)])?,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let exit_translate = translate.clone();
    let exit_code = Closure::wrap(Box::new(move |value: JsValue| -> Result<JsValue, JsValue> {
        translated_with(
            &exit_translate,
            "terminal.exitCode",
            &object(&[("code", value)])?,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let expand_aria_translate = translate.clone();
    let expand_aria = Closure::wrap(Box::new(move |value: JsValue| -> Result<JsValue, JsValue> {
        translated_with(
            &expand_aria_translate,
            "terminal.expandAria",
            &object(&[("n", value)])?,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let expand_translate = translate.clone();
    let expand = Closure::wrap(Box::new(move |value: JsValue| -> Result<JsValue, JsValue> {
        translated_with(
            &expand_translate,
            "terminal.expandRest",
            &object(&[("n", value)])?,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    object(&[
        ("signal", signal.into_js_value()),
        ("exitCode", exit_code.into_js_value()),
        ("running", translated(translate, "terminal.running")?),
        ("failed", translated(translate, "terminal.failed")?),
        ("done", translated(translate, "terminal.done")?),
        ("copy", translated(translate, "copy")?),
        ("copied", translated(translate, "copied")?),
        ("noOutput", translated(translate, "terminal.noOutput")?),
        (
            "collapseAria",
            translated(translate, "terminal.collapseAria")?,
        ),
        ("collapse", translated(translate, "collapse")?),
        ("expandAria", expand_aria.into_js_value()),
        ("expand", expand.into_js_value()),
    ])
}
