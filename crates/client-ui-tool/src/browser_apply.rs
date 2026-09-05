//! Compiled browser assembly for Tool call presentation.

use std::cell::RefCell;

use js_sys::Array;
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    browser::{call_method, object, required_function, required_property},
    browser_details::{configure_tool_details_component, tool_details_component},
    browser_row::configure_tool_row_component,
    browser_tree::{configure_tool_tree_components, tool_call_tree_component},
    browser_views::{configure_tool_view_components, tool_view_plugins},
};

pub(crate) const CONVERSATION_NS: &str = "conversation";
const INJECT: &[&str] = &["slots"];
const REQUIRED_PRIMITIVES: &[&str] = &[
    "CodeBlock",
    "DiffBlock",
    "DisclosureRow",
    "IconApiOutline14",
    "IconBrowseOutline16",
    "IconChecklistOutline14",
    "IconChevronDownOutline14",
    "IconCodeOutline16",
    "IconEditOutline16",
    "IconGlobeOutline14",
    "IconInspectOutline12",
    "IconQuestionOutline14",
    "IconSearchOutline16",
    "IconSparkle16",
    "ReadBlock",
    "SearchBlock",
    "StateDot",
    "TerminalBlock",
    "WebBlock",
];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
pub(crate) struct BrowserModules {
    pub(crate) react: JsValue,
    pub(crate) fragment: JsValue,
    primitives: JsValue,
}

impl BrowserModules {
    pub(crate) fn primitive(&self, name: &str) -> Result<JsValue, JsValue> {
        required_property(&self.primitives, name, "UI primitives")
    }
}

/// Configures React and the page-owned UI primitive face for compiled Tool renderers.
///
/// # Errors
///
/// Returns before mutation when a required React or primitive value is absent, or when a compiled
/// stylesheet cannot be installed.
#[wasm_bindgen(js_name = configureClientUiTool)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_tool(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    for method in ["createElement", "memo", "useMemo", "useState"] {
        required_function(&react, method, "React")?;
    }
    let fragment = required_property(&react, "Fragment", "React")?;
    for name in REQUIRED_PRIMITIVES {
        required_property(&primitives, name, "UI primitives")?;
    }
    let modules = BrowserModules {
        react,
        fragment,
        primitives,
    };
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules));
    configure_tool_row_component()?;
    configure_tool_view_components()?;
    configure_tool_tree_components()?;
    configure_tool_details_component()
}

/// Applies the complete Tool browser plugin to one Client Context.
///
/// # Errors
///
/// Returns on missing configuration, Slot service, declaration injection, registration, or child
/// plugin failures.
#[wasm_bindgen(js_name = applyClientUiTool)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_tool(ctx: JsValue) -> Result<(), JsValue> {
    configured_modules()?;
    let slots = required_property(&ctx, "slots", "Client Context")?;

    let chat_slots = slots.clone();
    let tree = tool_call_tree_component()?;
    let install_chat = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call_method(
            &chat_slots,
            "register",
            &[
                object(&[
                    ("name", JsValue::from_str("conversation.chat.node")),
                    ("key", JsValue::from_str("tool-call")),
                    ("locale", JsValue::from_str(CONVERSATION_NS)),
                    (
                        "children",
                        object(&[(
                            "tool.call.toolview",
                            object(&[
                                ("kind", JsValue::from_str("keyed")),
                                ("scope", JsValue::from_str("session")),
                            ])?
                            .into(),
                        )])?
                        .into(),
                    ),
                ])?
                .into(),
                tree.clone(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.chat.node"),
            install_chat.into_js_value(),
        ],
    )?;

    let details_slots = slots.clone();
    let details = tool_details_component()?;
    let install_details = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        call_method(
            &details_slots,
            "register",
            &[
                object(&[
                    ("name", JsValue::from_str("conversation.details.tool")),
                    ("locale", JsValue::from_str(CONVERSATION_NS)),
                ])?
                .into(),
                details.clone(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.details.tool"),
            install_details.into_js_value(),
        ],
    )?;

    for plugin in tool_view_plugins()? {
        call_method(&ctx, "plugin", &[plugin])?;
    }
    Ok(())
}

/// Returns the exact Client plugin dependency list.
#[wasm_bindgen(js_name = toolInject)]
pub fn tool_inject_browser() -> Array {
    let inject = Array::new();
    for dependency in INJECT {
        inject.push(&JsValue::from_str(dependency));
    }
    inject
}

pub(crate) fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-tool browser modules were not configured").into()
        })
    })
}

pub(crate) fn memo_component(component: &JsValue) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    required_function(&modules.react, "memo", "React")?.call1(&modules.react, component)
}
