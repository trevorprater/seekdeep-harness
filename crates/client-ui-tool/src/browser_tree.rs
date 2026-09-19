//! Compiled root/subcall Tool tree and keyed atomic dispatch.

use std::cell::RefCell;

use js_sys::Array;
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser::{
        bool_or_undefined, create_element, extend_object, inject_style, object, optional_property,
        required_function, required_property, tag,
    },
    browser_apply::{BrowserModules, configured_modules, memo_component},
    browser_model::BrowserToolBlock,
    browser_views::generic_tool_card_component,
};

const TREE_CSS: &str =
    include_str!("../../../packages/client/ui-tool/src/client/tool/ToolCallTree.module.css");
const CALL_ROW: &str = "seekdeep-tool-tree-callRow";
const SUB_CALLS: &str = "seekdeep-tool-tree-subCalls";

thread_local! {
    static TREE: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static BRANCH: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

pub(crate) fn configure_tool_tree_components() -> Result<(), JsValue> {
    let modules = configured_modules()?;
    inject_style(
        "ToolCallTree",
        TREE_CSS,
        &[("callRow", CALL_ROW), ("subCalls", SUB_CALLS)],
    )?;
    let branch_modules = modules.clone();
    let raw_branch =
        Closure::wrap(
            Box::new(move |props: JsValue| render_branch(&branch_modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    let branch = memo_component(&raw_branch)?;
    BRANCH.with(|configured| *configured.borrow_mut() = Some(branch));
    let tree_modules = modules;
    let raw_tree = Closure::wrap(
        Box::new(move |props: JsValue| render_tree(&tree_modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    TREE.with(|configured| *configured.borrow_mut() = Some(raw_tree));
    Ok(())
}

/// Returns the compiled whole-Tool tree renderer.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = toolCallTreeComponent)]
pub fn tool_call_tree_component() -> Result<JsValue, JsValue> {
    TREE.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-tool ToolCallTree was not configured").into()
        })
    })
}

fn branch_component() -> Result<JsValue, JsValue> {
    BRANCH.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-tool ToolCallBranch was not configured").into()
        })
    })
}

fn render_tree(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "ToolCallTree props")?;
    let data = required_property(&node, "data", "tool-call Chat node")?;
    let root = required_property(&data, "root", "tool-call Chat data")?;
    let branch_props = object(&[
        (
            "renderSlot",
            required_property(props, "renderSlot", "ToolCallTree props")?,
        ),
        ("block", root),
        (
            "selectedCallId",
            optional_property(props, "selectedCallId")?.unwrap_or(JsValue::UNDEFINED),
        ),
        (
            "cwd",
            optional_property(props, "cwd")?.unwrap_or(JsValue::UNDEFINED),
        ),
        (
            "openFile",
            required_property(props, "openFile", "ToolCallTree props")?,
        ),
        (
            "inspectCall",
            required_property(props, "inspectCall", "ToolCallTree props")?,
        ),
        ("t", required_property(props, "t", "ToolCallTree props")?),
    ])?;
    create_element(
        &modules.react,
        &branch_component()?,
        Some(&branch_props),
        &[],
    )
}

#[allow(clippy::too_many_lines)] // Recursive owner construction and dispatch stay co-located.
fn render_branch(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let raw = required_property(props, "block", "ToolCallBranch props")?;
    let block = BrowserToolBlock::parse(&raw)?;
    let call_id = block.model.call_id().to_owned();
    let tool_name = block.tool_name.clone();
    let render_slot = required_function(props, "renderSlot", "ToolCallBranch props")?;
    let open_file = required_property(props, "openFile", "ToolCallBranch props")?;
    let inspect_call = required_function(props, "inspectCall", "ToolCallBranch props")?;
    let cwd = optional_property(props, "cwd")?.unwrap_or(JsValue::UNDEFINED);
    let inspect_id = call_id.clone();
    let inspect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        inspect_call.call1(&JsValue::UNDEFINED, &JsValue::from_str(&inspect_id))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let owner_call_id = call_id.clone();
    let owner_tool_name = tool_name.clone();
    let owner_block = raw.clone();
    let owner_open_file = open_file.clone();
    let owner_cwd = cwd.clone();
    let owner_inspect = inspect.into_js_value();
    let owner_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(object(&[
            ("callId", JsValue::from_str(&owner_call_id)),
            ("toolName", JsValue::from_str(&owner_tool_name)),
            ("block", owner_block.clone()),
            ("openFile", owner_open_file.clone()),
            ("cwd", owner_cwd.clone()),
            ("inspect", owner_inspect.clone()),
        ])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::new();
    for value in [
        JsValue::from_str(&call_id),
        JsValue::from_str(&tool_name),
        raw,
        open_file,
        cwd,
        required_property(props, "inspectCall", "ToolCallBranch props")?,
    ] {
        dependencies.push(&value);
    }
    let owner = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &owner_factory.into_js_value(),
        &dependencies,
    )?;
    let fallback_props = extend_object(
        &owner,
        &[("t", required_property(props, "t", "ToolCallBranch props")?)],
    )?;
    let fallback = create_element(
        &modules.react,
        &generic_tool_card_component()?,
        Some(&fallback_props),
        &[],
    )?;
    let rendered = render_slot.apply(
        &JsValue::UNDEFINED,
        &Array::of3(
            &JsValue::from_str("tool.call.toolview"),
            &owner,
            object(&[
                ("entryKey", JsValue::from_str(&tool_name)),
                ("fallback", fallback),
            ])?
            .as_ref(),
        ),
    )?;
    let mut children = vec![rendered];
    if block.sub_calls.length() > 0 {
        let mut descendants = Vec::with_capacity(block.sub_calls.length() as usize);
        for index in 0..block.sub_calls.length() {
            let child = block.sub_calls.get(index);
            let child_id = required_property(&child, "callId", "Tool subcall")?;
            descendants.push(create_element(
                &modules.react,
                &branch_component()?,
                Some(&object(&[
                    ("key", child_id),
                    ("renderSlot", render_slot.clone().into()),
                    ("block", child),
                    (
                        "selectedCallId",
                        optional_property(props, "selectedCallId")?.unwrap_or(JsValue::UNDEFINED),
                    ),
                    (
                        "cwd",
                        optional_property(props, "cwd")?.unwrap_or(JsValue::UNDEFINED),
                    ),
                    (
                        "openFile",
                        required_property(props, "openFile", "ToolCallBranch props")?,
                    ),
                    (
                        "inspectCall",
                        required_property(props, "inspectCall", "ToolCallBranch props")?,
                    ),
                    ("t", required_property(props, "t", "ToolCallBranch props")?),
                ])?),
                &[],
            )?);
        }
        children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(SUB_CALLS)),
                ("data-subcalls", JsValue::TRUE),
            ])?),
            &descendants,
        )?);
    }
    let selected = optional_property(props, "selectedCallId")?
        .and_then(|value| value.as_string())
        .as_deref()
        == Some(call_id.as_str());
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            ("className", JsValue::from_str(CALL_ROW)),
            (
                "data-chat-anchor-key",
                JsValue::from_str(&format!("call:{call_id}")),
            ),
            ("data-chat-call-id", JsValue::from_str(&call_id)),
            ("data-selected", bool_or_undefined(selected)),
        ])?),
        &children,
    )
}
