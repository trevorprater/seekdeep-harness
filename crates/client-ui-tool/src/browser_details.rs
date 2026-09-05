//! Compiled card-aware Tool output body for the conversation details panel.

use std::cell::RefCell;

use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    ToolCallBlock,
    browser::{
        bool_or_undefined, class_props, create_element, extend_object, inject_style, object,
        optional_property, required_function, required_property, tag, translated,
    },
    browser_apply::{BrowserModules, configured_modules},
    browser_model::{
        BrowserToolBlock, diff_card_props, read_card_props, search_card_props, terminal_card_props,
        web_card_props,
    },
    browser_row::terminal_block_labels,
    diff_card_model, read_card_model, result_text, search_card_model, terminal_card_model,
    web_card_model,
};

const DETAILS_CSS: &str =
    include_str!("../../../packages/client/ui-tool/src/client/tool/ToolDetails.module.css");
const DESCRIPTION: &str = "seekdeep-tool-details-description";
const CARD_BODY: &str = "seekdeep-tool-details-cardBody";
const RECOVERY: &str = "seekdeep-tool-details-recovery";
const CODE: &str = "seekdeep-tool-details-code";
const READ: &str = "seekdeep-tool-details-read";
const WEB: &str = "seekdeep-tool-details-web";
const EMPTY: &str = "seekdeep-tool-details-empty";

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

pub(crate) fn configure_tool_details_component() -> Result<(), JsValue> {
    let modules = configured_modules()?;
    inject_style(
        "ToolDetails",
        DETAILS_CSS,
        &[
            ("description", DESCRIPTION),
            ("cardBody", CARD_BODY),
            ("recovery", RECOVERY),
            ("code", CODE),
            ("read", READ),
            ("web", WEB),
            ("empty", EMPTY),
        ],
    )?;
    let render_modules = modules;
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_tool_details(&render_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled details-panel Tool output renderer.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = toolDetailsComponent)]
pub fn tool_details_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-tool ToolDetails was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed card-kind precedence stays visible in one renderer.
fn render_tool_details(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let raw = required_property(props, "block", "ToolDetails props")?;
    let block = BrowserToolBlock::parse(&raw)?;
    let cwd = optional_property(props, "cwd")?.and_then(|value| value.as_string());
    let translate = required_function(props, "t", "ToolDetails props")?;
    if let Some(terminal) = terminal_card_model(&block.model, cwd.as_deref()) {
        let mut children = Vec::new();
        if let Some(description) = terminal.description.as_deref() {
            children.push(tag(
                &modules.react,
                "div",
                Some(&class_props(DESCRIPTION)?),
                &[JsValue::from_str(description)],
            )?);
        }
        let props = extend_object(
            terminal_card_props(&terminal)?.as_ref(),
            &[
                ("labels", terminal_block_labels(&translate)?.into()),
                ("className", JsValue::from_str(CARD_BODY)),
            ],
        )?;
        children.push(create_element(
            &modules.react,
            &modules.primitive("TerminalBlock")?,
            Some(&props),
            &[],
        )?);
        return create_element(&modules.react, &modules.fragment, None, &children);
    }
    if let Some(read) = read_card_model(&block.model, cwd.as_deref()) {
        let props = extend_object(
            read_card_props(&read)?.as_ref(),
            &[("className", JsValue::from_str(READ))],
        )?;
        return create_element(
            &modules.react,
            &modules.primitive("ReadBlock")?,
            Some(&props),
            &[],
        );
    }
    if let Some(diff) = diff_card_model(&block.model) {
        let props = extend_object(
            diff_card_props(&diff)?.as_ref(),
            &[("className", JsValue::from_str(CARD_BODY))],
        )?;
        return create_element(
            &modules.react,
            &modules.primitive("DiffBlock")?,
            Some(&props),
            &[],
        );
    }
    if let Some(search) = search_card_model(&block.model) {
        let props = extend_object(
            search_card_props(&search)?.as_ref(),
            &[("className", JsValue::from_str(CARD_BODY))],
        )?;
        let card = create_element(
            &modules.react,
            &modules.primitive("SearchBlock")?,
            Some(&props),
            &[],
        )?;
        let mut children = vec![card];
        if let Some(recovery) = search.recovery {
            children.push(tag(
                &modules.react,
                "div",
                Some(&class_props(RECOVERY)?),
                &[JsValue::from_str(&recovery)],
            )?);
        }
        return create_element(&modules.react, &modules.fragment, None, &children);
    }
    if let Some(web) = web_card_model(&block.model) {
        let props = extend_object(
            web_card_props(&web)?.as_ref(),
            &[("className", JsValue::from_str(WEB))],
        )?;
        let card = create_element(
            &modules.react,
            &modules.primitive("WebBlock")?,
            Some(&props),
            &[],
        )?;
        let mut children = vec![card];
        if matches!(block.model, ToolCallBlock::Settled { .. }) {
            let body = result_text(&block.model);
            if !body.is_empty() {
                children.push(tag(
                    &modules.react,
                    "pre",
                    Some(&class_props(CODE)?),
                    &[JsValue::from_str(&body)],
                )?);
            }
        }
        return create_element(&modules.react, &modules.fragment, None, &children);
    }
    let ToolCallBlock::Settled { is_error, .. } = &block.model else {
        return tag(
            &modules.react,
            "div",
            Some(&class_props(EMPTY)?),
            &[translated(&translate, "details.running")?],
        );
    };
    tag(
        &modules.react,
        "pre",
        Some(&object(&[
            ("className", JsValue::from_str(CODE)),
            ("data-error", bool_or_undefined(*is_error)),
        ])?),
        &[JsValue::from_str(&result_text(&block.model))],
    )
}
