//! Compiled generic and first-party atomic Tool views.

use std::cell::RefCell;

use js_sys::{Array, Function, Reflect};
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ToolCallBlock, ToolRowState, ToolRowVariant,
    browser::{
        bool_or_undefined, class_props, create_element, extend_object, inject_style, object,
        optional_property, required_function, required_property, required_string, tag, translated,
        translated_with,
    },
    browser_apply::{BrowserModules, CONVERSATION_NS, configured_modules},
    browser_model::{
        BrowserToolBlock, diff_card_props, read_card_props, search_card_props, state_name,
        terminal_card_props, variant_name, web_card_props,
    },
    browser_row::{terminal_block_labels, tool_row_component},
    diff_card_model, plan_summary, read_card_model, search_card_model, terminal_card_model,
    terminal_failed, tool_row_model, web_card_model,
};

const BASH_CSS: &str = include_str!(
    "../../../packages/client/ui-tool/src/client/tool/toolviews/bash-sample.module.css"
);

const BASH_CARD: &str = "seekdeep-tool-bash-card";
const BASH_TERMINAL: &str = "seekdeep-tool-bash-terminal";
const BASH_IO_CARD: &str = "seekdeep-tool-bash-ioCard";
const BASH_IO_SECTION: &str = "seekdeep-tool-bash-ioSection";
const BASH_IO_LABEL: &str = "seekdeep-tool-bash-ioLabel";
const BASH_IO_DIVIDER: &str = "seekdeep-tool-bash-ioDivider";
const BASH_IO_TEXT: &str = "seekdeep-tool-bash-ioText";
const BASH_ROOT: &str = "seekdeep-tool-bash-root";
const BASH_LEADING: &str = "seekdeep-tool-bash-leading";
const BASH_CHEVRON: &str = "seekdeep-tool-bash-chevron";
const BASH_ICON_IDLE: &str = "seekdeep-tool-bash-iconIdle";
const BASH_CHEVRON_HOVER: &str = "seekdeep-tool-bash-chevronHover";
const BASH_TITLE: &str = "seekdeep-tool-bash-title";
const BASH_SEP: &str = "seekdeep-tool-bash-sep";
const BASH_SUMMARY: &str = "seekdeep-tool-bash-summary";
const BASH_ERROR_SUMMARY: &str = "seekdeep-tool-bash-errorSummary";
const BASH_BODY_WRAP: &str = "seekdeep-tool-bash-bodyWrap";
const BASH_INSPECT: &str = "seekdeep-tool-bash-inspectButton";
const BASH_HIDDEN: &str = "seekdeep-tool-bash-visuallyHidden";

thread_local! {
    static GENERIC: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static ASK: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static BASH: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static FILE_MUTATION: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static READ: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static SEARCH: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static TODO: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static WEB: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static PLUGINS: RefCell<Vec<JsValue>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn configure_tool_view_components() -> Result<(), JsValue> {
    let modules = configured_modules()?;
    inject_style(
        "bash-sample",
        BASH_CSS,
        &[
            ("card", BASH_CARD),
            ("terminal", BASH_TERMINAL),
            ("ioCard", BASH_IO_CARD),
            ("ioSection", BASH_IO_SECTION),
            ("ioLabel", BASH_IO_LABEL),
            ("ioDivider", BASH_IO_DIVIDER),
            ("ioText", BASH_IO_TEXT),
            ("root", BASH_ROOT),
            ("leading", BASH_LEADING),
            ("chevron", BASH_CHEVRON),
            ("iconIdle", BASH_ICON_IDLE),
            ("chevronHover", BASH_CHEVRON_HOVER),
            ("title", BASH_TITLE),
            ("sep", BASH_SEP),
            ("summary", BASH_SUMMARY),
            ("errorSummary", BASH_ERROR_SUMMARY),
            ("bodyWrap", BASH_BODY_WRAP),
            ("inspectButton", BASH_INSPECT),
            ("visuallyHidden", BASH_HIDDEN),
        ],
    )?;
    GENERIC.with(|value| *value.borrow_mut() = Some(component(&modules, render_generic)));
    ASK.with(|value| *value.borrow_mut() = Some(component(&modules, render_ask)));
    BASH.with(|value| *value.borrow_mut() = Some(component(&modules, render_bash)));
    FILE_MUTATION
        .with(|value| *value.borrow_mut() = Some(component(&modules, render_file_mutation)));
    READ.with(|value| *value.borrow_mut() = Some(component(&modules, render_read)));
    SEARCH.with(|value| *value.borrow_mut() = Some(component(&modules, render_search)));
    TODO.with(|value| *value.borrow_mut() = Some(component(&modules, render_todo)));
    WEB.with(|value| *value.borrow_mut() = Some(component(&modules, render_web)));
    let plugins = vec![
        registrant_plugin("bash-toolview-sample", &["bash"], bash_row_component()?)?,
        registrant_plugin("read-toolview", &["read"], read_row_component()?)?,
        registrant_plugin(
            "file-mutation-toolview",
            &["edit", "write"],
            file_mutation_row_component()?,
        )?,
        registrant_plugin(
            "search-toolview",
            &["grep", "glob"],
            search_row_component()?,
        )?,
        registrant_plugin(
            "web-toolview",
            &["web_search", "web_fetch"],
            web_row_component()?,
        )?,
        registrant_plugin("todo-toolview", &["todo_write"], todo_row_component()?)?,
        registrant_plugin(
            "ask-question-toolview",
            &["ask_user_question"],
            ask_question_row_component()?,
        )?,
    ];
    PLUGINS.with(|configured| *configured.borrow_mut() = plugins);
    Ok(())
}

type Renderer = fn(&BrowserModules, &JsValue) -> Result<JsValue, JsValue>;

fn component(modules: &BrowserModules, renderer: Renderer) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(Box::new(move |props: JsValue| renderer(&modules, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

macro_rules! component_getter {
    ($function:ident, $js_name:literal, $cell:ident, $label:literal) => {
        #[doc = concat!("Returns the compiled `", $label, "` component.")]
        ///
        /// # Errors
        ///
        /// Returns before browser configuration.
        #[wasm_bindgen(js_name = $js_name)]
        pub fn $function() -> Result<JsValue, JsValue> {
            $cell.with(|configured| {
                configured.borrow().clone().ok_or_else(|| {
                    js_sys::Error::new(concat!("client-ui-tool ", $label, " was not configured"))
                        .into()
                })
            })
        }
    };
}

component_getter!(
    generic_tool_card_component,
    "genericToolCardComponent",
    GENERIC,
    "GenericToolCard"
);
component_getter!(
    ask_question_row_component,
    "askQuestionRowComponent",
    ASK,
    "AskQuestionRow"
);
component_getter!(bash_row_component, "bashRowComponent", BASH, "BashRow");
component_getter!(
    file_mutation_row_component,
    "fileMutationRowComponent",
    FILE_MUTATION,
    "FileMutationRow"
);
component_getter!(read_row_component, "readRowComponent", READ, "ReadRow");
component_getter!(
    search_row_component,
    "searchRowComponent",
    SEARCH,
    "SearchRow"
);
component_getter!(todo_row_component, "todoRowComponent", TODO, "TodoRow");
component_getter!(web_row_component, "webRowComponent", WEB, "WebRow");

pub(crate) fn tool_view_plugins() -> Result<Vec<JsValue>, JsValue> {
    PLUGINS.with(|configured| {
        let values = configured.borrow().clone();
        if values.is_empty() {
            Err(js_sys::Error::new("client-ui-tool registrants were not configured").into())
        } else {
            Ok(values)
        }
    })
}

fn render_generic(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let cwd = optional_property(props, "cwd")?.and_then(|value| value.as_string());
    let model = tool_row_model(&block.tool_name, &block.model, cwd.as_deref());
    let terminal = terminal_card_model(&block.model, cwd.as_deref());
    let read = read_card_model(&block.model, cwd.as_deref());
    let diff = diff_card_model(&block.model);
    let search = search_card_model(&block.model);
    let web = web_card_model(&block.model);
    let state = if model.state == ToolRowState::Ok && terminal.as_ref().is_some_and(terminal_failed)
    {
        ToolRowState::Error
    } else {
        model.state
    };
    let single_file = model.file_path.is_some();
    let summary = terminal
        .as_ref()
        .and_then(|value| value.description.as_deref())
        .or_else(|| search.as_ref().and_then(|value| value.title.as_deref()))
        .unwrap_or(&model.summary);
    let row = tool_row_component()?;
    create_element(
        &modules.react,
        &row,
        Some(&object(&[
            ("t", required_property(props, "t", "GenericToolCard props")?),
            ("variant", JsValue::from_str(variant_name(model.variant))),
            ("toolName", JsValue::from_str(&block.tool_name)),
            ("icon", variant_icon(modules, model.variant)?),
            ("title", JsValue::from_str(&model.title)),
            ("summary", JsValue::from_str(summary)),
            (
                "body",
                if single_file {
                    JsValue::NULL
                } else {
                    option_string(model.body.as_deref())
                },
            ),
            ("output", option_string(model.output.as_deref())),
            (
                "errorSummary",
                option_string(model.error_summary.as_deref()),
            ),
            ("terminal", terminal_model_value(terminal.as_ref())?),
            ("diff", diff_model_value(diff.as_ref())?),
            ("read", read_model_value(read.as_ref())?),
            ("search", search_model_value(search.as_ref())?),
            ("web", web_model_value(web.as_ref())?),
            ("state", JsValue::from_str(state_name(state))),
            ("filePath", option_string(model.file_path.as_deref())),
            (
                "onOpenFile",
                if single_file {
                    required_property(props, "openFile", "GenericToolCard props")?
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "inspect",
                optional_property(props, "inspect")?.unwrap_or(JsValue::UNDEFINED),
            ),
        ])?),
        &[],
    )
}

fn render_file_mutation(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let cwd = optional_property(props, "cwd")?.and_then(|value| value.as_string());
    let model = tool_row_model(&block.tool_name, &block.model, cwd.as_deref());
    let row_icon = modules.primitive("IconEditOutline16")?;
    render_standard_row(
        modules,
        props,
        &block.tool_name,
        &model,
        &row_icon,
        &[("size", JsValue::from_f64(14.0))],
        None,
        JsValue::NULL,
        diff_model_value(diff_card_model(&block.model).as_ref())?,
        JsValue::NULL,
        JsValue::NULL,
        JsValue::NULL,
        true,
    )
}

fn render_read(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let cwd = optional_property(props, "cwd")?.and_then(|value| value.as_string());
    let model = tool_row_model(&block.tool_name, &block.model, cwd.as_deref());
    let row_icon = modules.primitive("IconBrowseOutline16")?;
    render_standard_row(
        modules,
        props,
        &block.tool_name,
        &model,
        &row_icon,
        &[("size", JsValue::from_f64(14.0))],
        None,
        JsValue::NULL,
        JsValue::NULL,
        read_model_value(read_card_model(&block.model, cwd.as_deref()).as_ref())?,
        JsValue::NULL,
        JsValue::NULL,
        true,
    )
}

fn render_search(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let model = tool_row_model(&block.tool_name, &block.model, None);
    let search = search_card_model(&block.model);
    let title = match block.tool_name.as_str() {
        "grep" => "Grep",
        "glob" => "Glob",
        _ => &model.title,
    };
    let summary = search
        .as_ref()
        .and_then(|value| value.title.as_deref())
        .unwrap_or(&model.summary);
    let row_icon = modules.primitive("IconSearchOutline16")?;
    render_standard_row(
        modules,
        props,
        &block.tool_name,
        &model,
        &row_icon,
        &[("size", JsValue::from_f64(14.0))],
        Some((title, summary)),
        JsValue::NULL,
        JsValue::NULL,
        JsValue::NULL,
        search_model_value(search.as_ref())?,
        JsValue::NULL,
        false,
    )
}

fn render_web(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let model = tool_row_model(&block.tool_name, &block.model, None);
    let (title, icon) = if block.tool_name == "web_fetch" {
        ("Fetch", modules.primitive("IconBrowseOutline16")?)
    } else {
        ("Search", modules.primitive("IconGlobeOutline14")?)
    };
    render_standard_row(
        modules,
        props,
        &block.tool_name,
        &model,
        &icon,
        &[("size", JsValue::from_f64(14.0))],
        Some((title, &model.summary)),
        JsValue::NULL,
        JsValue::NULL,
        JsValue::NULL,
        JsValue::NULL,
        web_model_value(web_card_model(&block.model).as_ref())?,
        false,
    )
}

fn render_todo(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let model = tool_row_model(&block.tool_name, &block.model, None);
    let translate = required_function(props, "t", "TodoRow props")?;
    let summary = todo_summary(block.raw_arguments(), &translate)?;
    let (summary, extra) = summary.unwrap_or((model.summary.clone(), 0));
    let title = translated(&translate, "todo.rowTitle")?;
    let row = tool_row_component()?;
    create_element(
        &modules.react,
        &row,
        Some(&object(&[
            ("t", translate.clone().into()),
            ("variant", JsValue::from_str(variant_name(model.variant))),
            ("toolName", JsValue::from_str(&block.tool_name)),
            (
                "icon",
                create_element(
                    &modules.react,
                    &modules.primitive("IconChecklistOutline14")?,
                    None,
                    &[],
                )?,
            ),
            ("title", title),
            ("summary", JsValue::from_str(&summary)),
            (
                "summarySuffix",
                if extra == 0 {
                    JsValue::NULL
                } else {
                    JsValue::from_str(&format!("+{extra}"))
                },
            ),
            ("body", option_string(model.body.as_deref())),
            ("output", option_string(model.output.as_deref())),
            (
                "errorSummary",
                option_string(model.error_summary.as_deref()),
            ),
            ("state", JsValue::from_str(state_name(model.state))),
            (
                "inspect",
                optional_property(props, "inspect")?.unwrap_or(JsValue::UNDEFINED),
            ),
        ])?),
        &[],
    )
}

fn render_ask(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let model = tool_row_model(&block.tool_name, &block.model, None);
    let translate = required_function(props, "t", "AskQuestionRow props")?;
    let mut summary = model.summary.clone();
    let mut state = model.state;
    match block.error_code() {
        Some("ASK_CANCELLED") => {
            summary = translated(&translate, "ask.cancelled")?
                .as_string()
                .unwrap_or_default();
        }
        Some("ASK_ABORTED") => {
            summary = translated(&translate, "ask.interrupted")?
                .as_string()
                .unwrap_or_default();
            state = ToolRowState::Stopped;
        }
        _ if model.state == ToolRowState::Running => {
            summary = translated(&translate, "ask.waiting")?
                .as_string()
                .unwrap_or_default();
        }
        _ if model.state == ToolRowState::Ok
            && matches!(block.model, ToolCallBlock::Settled { .. }) =>
        {
            if let Some(answered) = answered_summary(&block.concatenated_text(), &translate)? {
                summary = answered;
            }
        }
        _ => {}
    }
    let row = tool_row_component()?;
    create_element(
        &modules.react,
        &row,
        Some(&object(&[
            ("t", translate.clone().into()),
            ("variant", JsValue::from_str(variant_name(model.variant))),
            ("toolName", JsValue::from_str(&block.tool_name)),
            (
                "icon",
                create_element(
                    &modules.react,
                    &modules.primitive("IconQuestionOutline14")?,
                    None,
                    &[],
                )?,
            ),
            ("title", translated(&translate, "ask.rowTitle")?),
            ("summary", JsValue::from_str(&summary)),
            ("body", option_string(model.body.as_deref())),
            ("output", option_string(model.output.as_deref())),
            ("state", JsValue::from_str(state_name(state))),
            (
                "inspect",
                optional_property(props, "inspect")?.unwrap_or(JsValue::UNDEFINED),
            ),
        ])?),
        &[],
    )
}

#[allow(clippy::too_many_lines)] // The bespoke sample row is one closed source component.
fn render_bash(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let block = parsed_from_props(props)?;
    let model = tool_row_model(&block.tool_name, &block.model, None);
    let session_id = required_property(props, "sessionId", "BashRow props")?;
    let selector_session = session_id.clone();
    let selector = Closure::wrap(Box::new(move |list: JsValue| -> Result<JsValue, JsValue> {
        let by_id = required_property(&list, "byId", "session list")?;
        let row = Reflect::get(&by_id, &selector_session)?;
        if row.is_null() || row.is_undefined() {
            Ok(JsValue::UNDEFINED)
        } else {
            Reflect::get(&row, &JsValue::from_str("cwd"))
        }
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let cwd = required_function(props, "useSessions", "BashRow props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?
        .as_string();
    let terminal = terminal_card_model(&block.model, cwd.as_deref());
    let state = if model.state == ToolRowState::Ok && terminal.as_ref().is_some_and(terminal_failed)
    {
        ToolRowState::Error
    } else {
        model.state
    };
    let translate = required_function(props, "t", "BashRow props")?;
    let state_pair = Array::from(
        &required_function(&modules.react, "useState", "React")?
            .call1(&modules.react, &JsValue::FALSE)?,
    );
    let expanded = state_pair.get(0).as_bool().unwrap_or(false);
    let set_expanded = state_pair.get(1).dyn_into::<Function>()?;
    let generic_error = terminal.is_none()
        && model.state == ToolRowState::Error
        && (model.body.is_some() || model.output.is_some());
    let expandable = terminal.is_some() || generic_error;
    let open = expanded && expandable;
    let update = Closure::wrap(Box::new(move |previous: JsValue| -> bool {
        !previous.as_bool().unwrap_or(false)
    }) as Box<dyn FnMut(JsValue) -> bool>);
    let update_value = update.into_js_value();
    let click_setter = set_expanded.clone();
    let click_update = update_value.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        click_setter.call1(&JsValue::UNDEFINED, &click_update)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let key_setter = set_expanded;
    let key_update = update_value;
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required_string(&event, "key", "Keyboard event")?;
        if matches!(key.as_str(), "Enter" | " ") {
            required_function(&event, "preventDefault", "Keyboard event")?.call0(&event)?;
            key_setter.call1(&JsValue::UNDEFINED, &key_update)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let leading = if open {
        icon(
            modules,
            "IconChevronDownOutline14",
            &[("className", JsValue::from_str(BASH_CHEVRON))],
        )?
    } else if expandable {
        create_element(
            &modules.react,
            &modules.fragment,
            None,
            &[
                tag(
                    &modules.react,
                    "span",
                    Some(&class_props(BASH_ICON_IDLE)?),
                    &[bash_state_icon(modules, state)?],
                )?,
                icon(
                    modules,
                    "IconChevronDownOutline14",
                    &[(
                        "className",
                        JsValue::from_str(&format!("{BASH_CHEVRON} {BASH_CHEVRON_HOVER}")),
                    )],
                )?,
            ],
        )?
    } else {
        bash_state_icon(modules, state)?
    };
    let failure_line = (model.state == ToolRowState::Error)
        .then_some(model.error_summary.as_deref())
        .flatten();
    let summary = failure_line
        .or_else(|| {
            terminal
                .as_ref()
                .and_then(|terminal| terminal.description.as_deref())
        })
        .unwrap_or(&model.summary);
    let summary_class = if failure_line.is_some() {
        format!("{BASH_SUMMARY} {BASH_ERROR_SUMMARY}")
    } else {
        BASH_SUMMARY.to_owned()
    };
    let mut row_children = vec![tag(
        &modules.react,
        "span",
        Some(&class_props(BASH_LEADING)?),
        &[leading],
    )?];
    if let Some(status) = bash_status(&translate, state)? {
        row_children.push(tag(
            &modules.react,
            "span",
            Some(&class_props(BASH_HIDDEN)?),
            &[status],
        )?);
    }
    row_children.extend([
        tag(
            &modules.react,
            "span",
            Some(&class_props(BASH_TITLE)?),
            &[JsValue::from_str(&model.title)],
        )?,
        tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str(BASH_SEP)),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?,
        tag(
            &modules.react,
            "span",
            Some(&class_props(&summary_class)?),
            &[JsValue::from_str(summary)],
        )?,
    ]);
    let row = tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("className", JsValue::from_str(BASH_ROOT)),
            ("data-sample", JsValue::from_str("bash")),
            ("data-variant", JsValue::from_str("bash")),
            ("data-state", JsValue::from_str(state_name(state))),
            ("data-expandable", bool_or_undefined(expandable)),
            (
                "role",
                if expandable {
                    JsValue::from_str("button")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "tabIndex",
                if expandable {
                    JsValue::from_f64(0.0)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-expanded",
                if expandable {
                    JsValue::from_bool(open)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "onClick",
                if expandable {
                    click.into_js_value()
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "onKeyDown",
                if expandable {
                    keydown.into_js_value()
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &row_children,
    )?;
    let mut children = vec![row];
    if open {
        let mut expanded_children = vec![if let Some(terminal) = terminal.as_ref() {
            let props = extend_object(
                terminal_card_props(terminal)?.as_ref(),
                &[
                    ("maxLines", JsValue::from_f64(f64::INFINITY)),
                    ("labels", terminal_block_labels(&translate)?.into()),
                    ("className", JsValue::from_str(BASH_TERMINAL)),
                ],
            )?;
            create_element(
                &modules.react,
                &modules.primitive("TerminalBlock")?,
                Some(&props),
                &[],
            )?
        } else {
            bash_io_card(modules, &model)?
        }];
        if let Some(inspect) = optional_property(props, "inspect")? {
            expanded_children.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    ("className", JsValue::from_str(BASH_INSPECT)),
                    ("onClick", inspect),
                ])?),
                &[
                    icon(modules, "IconInspectOutline12", &[])?,
                    JsValue::from_str("Inspect"),
                ],
            )?);
        }
        children.push(tag(
            &modules.react,
            "div",
            Some(&class_props(BASH_BODY_WRAP)?),
            &expanded_children,
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class_props(BASH_CARD)?),
        &children,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_standard_row(
    modules: &BrowserModules,
    props: &JsValue,
    tool_name: &str,
    model: &crate::ToolRowModel,
    icon_kind: &JsValue,
    icon_props: &[(&str, JsValue)],
    title_summary: Option<(&str, &str)>,
    terminal: JsValue,
    diff: JsValue,
    read: JsValue,
    search: JsValue,
    web: JsValue,
    file_link: bool,
) -> Result<JsValue, JsValue> {
    let (title, summary) = title_summary.unwrap_or((&model.title, &model.summary));
    let row = tool_row_component()?;
    create_element(
        &modules.react,
        &row,
        Some(&object(&[
            ("t", required_property(props, "t", "Tool view props")?),
            ("variant", JsValue::from_str(variant_name(model.variant))),
            ("toolName", JsValue::from_str(tool_name)),
            (
                "icon",
                create_element(&modules.react, icon_kind, Some(&object(icon_props)?), &[])?,
            ),
            ("title", JsValue::from_str(title)),
            ("summary", JsValue::from_str(summary)),
            ("body", JsValue::NULL),
            ("output", option_string(model.output.as_deref())),
            (
                "errorSummary",
                option_string(model.error_summary.as_deref()),
            ),
            ("terminal", terminal),
            ("diff", diff),
            ("read", read),
            ("search", search),
            ("web", web),
            ("state", JsValue::from_str(state_name(model.state))),
            (
                "filePath",
                if file_link {
                    option_string(model.file_path.as_deref())
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "onOpenFile",
                if file_link {
                    required_property(props, "openFile", "Tool view props")?
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "inspect",
                optional_property(props, "inspect")?.unwrap_or(JsValue::UNDEFINED),
            ),
        ])?),
        &[],
    )
}

fn parsed_from_props(props: &JsValue) -> Result<BrowserToolBlock, JsValue> {
    BrowserToolBlock::parse(&required_property(props, "block", "Tool view props")?)
}

fn variant_icon(modules: &BrowserModules, variant: ToolRowVariant) -> Result<JsValue, JsValue> {
    let name = match variant {
        ToolRowVariant::Search => "IconSearchOutline16",
        ToolRowVariant::Read => "IconBrowseOutline16",
        ToolRowVariant::Bash => "IconApiOutline14",
        ToolRowVariant::Write | ToolRowVariant::Edit => "IconEditOutline16",
        ToolRowVariant::Code => "IconCodeOutline16",
        ToolRowVariant::Others => "IconSparkle16",
    };
    icon(modules, name, &[("size", JsValue::from_f64(14.0))])
}

fn icon(
    modules: &BrowserModules,
    name: &str,
    props: &[(&str, JsValue)],
) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &modules.primitive(name)?,
        (!props.is_empty())
            .then(|| object(props))
            .transpose()?
            .as_ref(),
        &[],
    )
}

fn bash_state_icon(modules: &BrowserModules, state: ToolRowState) -> Result<JsValue, JsValue> {
    match state {
        ToolRowState::Error | ToolRowState::Stopped => create_element(
            &modules.react,
            &modules.primitive("StateDot")?,
            Some(&object(&[(
                "state",
                JsValue::from_str(if state == ToolRowState::Error {
                    "error"
                } else {
                    "warning"
                }),
            )])?),
            &[],
        ),
        ToolRowState::Running | ToolRowState::Ok => icon(
            modules,
            "IconApiOutline14",
            &[("size", JsValue::from_f64(14.0))],
        ),
    }
}

fn bash_status(translate: &Function, state: ToolRowState) -> Result<Option<JsValue>, JsValue> {
    let key = match state {
        ToolRowState::Running => Some("bash.running"),
        ToolRowState::Error => Some("bash.failed"),
        ToolRowState::Stopped => Some("bash.stopped"),
        ToolRowState::Ok => None,
    };
    key.map(|key| translated(translate, key)).transpose()
}

fn bash_io_card(modules: &BrowserModules, model: &crate::ToolRowModel) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    if let Some(body) = &model.body {
        children.push(bash_io_section(modules, "IN", body, false)?);
    }
    if model.body.is_some() && model.output.is_some() {
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                ("className", JsValue::from_str(BASH_IO_DIVIDER)),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?);
    }
    if let Some(output) = &model.output {
        children.push(bash_io_section(modules, "OUT", output, true)?);
    }
    tag(
        &modules.react,
        "div",
        Some(&class_props(BASH_IO_CARD)?),
        &children,
    )
}

fn bash_io_section(
    modules: &BrowserModules,
    label: &str,
    text: &str,
    error: bool,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "div",
        Some(&class_props(BASH_IO_SECTION)?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&class_props(BASH_IO_LABEL)?),
                &[JsValue::from_str(label)],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[
                    ("className", JsValue::from_str(BASH_IO_TEXT)),
                    ("data-error", bool_or_undefined(error)),
                ])?),
                &[JsValue::from_str(text)],
            )?,
        ],
    )
}

fn terminal_model_value(model: Option<&crate::TerminalCardModel>) -> Result<JsValue, JsValue> {
    model.map_or(Ok(JsValue::NULL), |model| {
        object(&[
            ("card", terminal_card_props(model)?.into()),
            (
                "description",
                option_undefined(model.description.as_deref()),
            ),
        ])
        .map(Into::into)
    })
}

fn diff_model_value(model: Option<&crate::DiffCardModel>) -> Result<JsValue, JsValue> {
    model.map_or(Ok(JsValue::NULL), |model| {
        object(&[("card", diff_card_props(model)?.into())]).map(Into::into)
    })
}

fn read_model_value(model: Option<&crate::ReadCardModel>) -> Result<JsValue, JsValue> {
    model.map_or(Ok(JsValue::NULL), |model| {
        read_card_props(model).map(Into::into)
    })
}

fn search_model_value(model: Option<&crate::SearchCardModel>) -> Result<JsValue, JsValue> {
    model.map_or(Ok(JsValue::NULL), |model| {
        object(&[
            ("card", search_card_props(model)?.into()),
            ("title", option_undefined(model.title.as_deref())),
            ("recovery", option_undefined(model.recovery.as_deref())),
        ])
        .map(Into::into)
    })
}

fn web_model_value(model: Option<&crate::WebCardModel>) -> Result<JsValue, JsValue> {
    model.map_or(Ok(JsValue::NULL), |model| {
        web_card_props(model).map(Into::into)
    })
}

fn option_string(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::NULL, JsValue::from_str)
}

fn option_undefined(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::UNDEFINED, JsValue::from_str)
}

fn answered_summary(text: &str, translate: &Function) -> Result<Option<String>, JsValue> {
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return Ok(None);
    };
    let Some(answers) = parsed.get("answers").and_then(Value::as_array) else {
        return Ok(None);
    };
    if answers
        .iter()
        .any(|answer| !matches!(answer, Value::Object(_) | Value::Array(_)))
    {
        return Ok(None);
    }
    let answered = answers
        .iter()
        .filter(|answer| {
            answer
                .get("selected")
                .and_then(Value::as_array)
                .is_some_and(|selected| !selected.is_empty())
                || answer
                    .get("custom")
                    .and_then(Value::as_str)
                    .is_some_and(|custom| !custom.is_empty())
        })
        .count();
    Ok(translated_with(
        translate,
        "ask.answered",
        &object(&[
            ("answered", usize_number(answered)?),
            ("total", usize_number(answers.len())?),
        ])?,
    )?
    .as_string())
}

fn todo_summary(raw: &str, translate: &Function) -> Result<Option<(String, usize)>, JsValue> {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Ok(None);
    };
    let Some(todos) = parsed.get("todos").and_then(Value::as_array) else {
        return Ok(None);
    };
    if todos
        .iter()
        .any(|todo| !matches!(todo, Value::Object(_) | Value::Array(_)))
    {
        return Ok(None);
    }
    let summary = plan_summary(todos);
    let head = translated_with(
        translate,
        "todo.completed",
        &object(&[
            ("done", usize_number(summary.done)?),
            ("total", usize_number(summary.total)?),
        ])?,
    )?
    .as_string()
    .unwrap_or_default();
    let text = summary
        .active_content
        .map_or_else(|| head.clone(), |active| format!("{head} · {active}"));
    Ok(Some((text, summary.active_extra)))
}

fn registrant_plugin(name: &str, keys: &[&str], component: JsValue) -> Result<JsValue, JsValue> {
    let owned_keys = keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
    let apply = Closure::wrap(Box::new(move |ctx: JsValue| -> Result<(), JsValue> {
        let slots = required_property(&ctx, "slots", "Toolview context")?;
        let register_slots = slots.clone();
        let register_component = component.clone();
        let keys = owned_keys.clone();
        let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let disposers = Array::new();
            for key in &keys {
                disposers.push(&crate::browser::call_method(
                    &register_slots,
                    "register",
                    &[
                        object(&[
                            ("name", JsValue::from_str("tool.call.toolview")),
                            ("key", JsValue::from_str(key)),
                            ("locale", JsValue::from_str(CONVERSATION_NS)),
                        ])?
                        .into(),
                        register_component.clone(),
                    ],
                )?);
            }
            Ok(if disposers.length() == 1 {
                disposers.get(0)
            } else {
                disposers.into()
            })
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        crate::browser::call_method(
            &slots,
            "inject",
            &[
                JsValue::from_str("tool.call.toolview"),
                install.into_js_value(),
            ],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let inject = Array::of1(&JsValue::from_str("slots"));
    Ok(object(&[
        ("name", JsValue::from_str(name)),
        ("inject", inject.into()),
        ("apply", apply.into_js_value()),
    ])?
    .into())
}

fn usize_number(value: usize) -> Result<JsValue, JsValue> {
    let value = u32::try_from(value)
        .map_err(|_| js_sys::RangeError::new("Tool item count exceeds the browser array limit"))?;
    Ok(JsValue::from_f64(f64::from(value)))
}
