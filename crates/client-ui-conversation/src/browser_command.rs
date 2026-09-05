//! Compiled command lifecycle and compaction chat renderers.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const CHAT_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/ChatView.module.css");
const COMMAND_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/GenericCommandCard.module.css"
);
const MESSAGE_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/MessageItem.module.css");
const ACCESSIBILITY_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/chat/accessibility.module.css"
);

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<BrowserComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    disclosure_row: JsValue,
    api_icon: JsValue,
    state_dot: JsValue,
    chevron_down: JsValue,
    chevron_right: JsValue,
    markdown_text: JsValue,
}

#[derive(Clone)]
struct BrowserComponents {
    command_node_view: JsValue,
    manual_compaction_node_view: JsValue,
    generic_command_card: JsValue,
    compaction_command_card: JsValue,
    compaction_item: JsValue,
}

type Renderer = fn(&BrowserModules, &BrowserComponents, &JsValue) -> Result<JsValue, JsValue>;

/// Configures the compiled command and compaction renderer family.
///
/// # Errors
///
/// Returns on missing React hooks, missing ui-primitives faces, or stylesheet failures.
#[wasm_bindgen(js_name = configureClientUiConversationCommand)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_command(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "memo", "useMemo", "useState"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        disclosure_row: required_property(&ui_primitives, "DisclosureRow", "ui-primitives")?,
        api_icon: required_property(&ui_primitives, "IconApiOutline14", "ui-primitives")?,
        state_dot: required_property(&ui_primitives, "StateDot", "ui-primitives")?,
        chevron_down: required_property(
            &ui_primitives,
            "IconChevronDownOutline14",
            "ui-primitives",
        )?,
        chevron_right: required_property(
            &ui_primitives,
            "IconChevronRightOutline14",
            "ui-primitives",
        )?,
        markdown_text: required_property(&ui_primitives, "MarkdownText", "ui-primitives")?,
        react,
    };
    inject_command_styles()?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    COMPONENTS.with(|configured| *configured.borrow_mut() = None);
    let components = BrowserComponents {
        command_node_view: memo_component(&modules.react, render_command_node_view)?,
        manual_compaction_node_view: memo_component(
            &modules.react,
            render_manual_compaction_node_view,
        )?,
        generic_command_card: raw_component(render_generic_command_card),
        compaction_command_card: raw_component(render_compaction_command_card),
        compaction_item: memo_component(&modules.react, render_compaction_item)?,
    };
    COMPONENTS.with(|configured| *configured.borrow_mut() = Some(components));
    Ok(())
}

/// Returns the memoized compiled ordinary-command node renderer.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = commandNodeViewComponent)]
pub fn command_node_view_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.command_node_view)
}

/// Returns the memoized compiled manual-compaction node renderer.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = manualCompactionNodeViewComponent)]
pub fn manual_compaction_node_view_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.manual_compaction_node_view)
}

/// Returns the compiled generic command lifecycle card.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = genericCommandCardComponent)]
pub fn generic_command_card_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.generic_command_card)
}

/// Returns the compiled manual-compaction lifecycle adapter.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = compactionCommandCardComponent)]
pub fn compaction_command_card_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.compaction_command_card)
}

/// Returns the memoized compiled compaction checkpoint row.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = compactionItemComponent)]
pub fn compaction_item_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.compaction_item)
}

fn raw_component(renderer: Renderer) -> JsValue {
    Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        renderer(&configured_modules()?, &configured_components()?, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn memo_component(react: &JsValue, renderer: Renderer) -> Result<JsValue, JsValue> {
    required_function(react, "memo", "React")?.call1(react, &raw_component(renderer))
}

fn render_command_node_view(
    modules: &BrowserModules,
    components: &BrowserComponents,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "CommandNodeView props")?;
    let command = required_property(&node, "data", "command chat node")?;
    let owner_command = command.clone();
    let owner_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(object(&[("node", owner_command.clone())])?.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let owner = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &owner_factory.into_js_value(),
        &Array::of1(&command),
    )?;
    let translate = required_property(props, "t", "CommandNodeView props")?;
    let fallback = create_element(
        &modules.react,
        &components.generic_command_card,
        Some(&object(&[("node", command.clone()), ("t", translate)])?),
        &[],
    )?;
    let name = Reflect::get(&command, &JsValue::from_str("name"))?;
    let entry_key = if is_nullish(&name) {
        String::new()
    } else {
        name.as_string().ok_or_else(|| {
            JsValue::from(js_sys::TypeError::new(
                "command node name must be a string or null",
            ))
        })?
    };
    let options = object(&[
        ("entryKey", JsValue::from_str(&entry_key)),
        ("fallback", fallback),
    ])?;
    let content = required_function(props, "renderSlot", "CommandNodeView props")?.apply(
        &JsValue::UNDEFINED,
        &Array::of3(
            &JsValue::from_str("conversation.chat.commandview"),
            &owner,
            options.as_ref(),
        ),
    )?;
    call_row(modules, content)
}

fn render_manual_compaction_node_view(
    modules: &BrowserModules,
    components: &BrowserComponents,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "ManualCompactionNodeView props")?;
    let data = required_property(&node, "data", "manual compaction chat node")?;
    let command = required_property(&data, "command", "manual compaction data")?;
    let compaction = Reflect::get(&data, &JsValue::from_str("compaction"))?;
    let mut card_props = vec![
        ("node", command),
        (
            "t",
            required_property(props, "t", "ManualCompactionNodeView props")?,
        ),
    ];
    if !compaction.is_null() {
        card_props.push(("compaction", compaction));
    }
    let card = create_element(
        &modules.react,
        &components.compaction_command_card,
        Some(&object(&card_props)?),
        &[],
    )?;
    call_row(modules, card)
}

fn call_row(modules: &BrowserModules, child: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-conversation-chat-callRow"),
        )])?),
        &[child],
    )
}

#[allow(clippy::too_many_lines)] // Closed source component tree stays auditable in one renderer.
fn render_generic_command_card(
    modules: &BrowserModules,
    _components: &BrowserComponents,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "GenericCommandCard props")?;
    let translate = required_function(props, "t", "GenericCommandCard props")?;
    let outcome = Reflect::get(&node, &JsValue::from_str("outcome"))?;
    let text = if outcome.is_null() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(&outcome, &JsValue::from_str("text"))?
    };
    let state = if outcome.is_null() {
        "running"
    } else if required_string(&outcome, "kind", "command outcome")? == "error" {
        "error"
    } else {
        "ok"
    };
    let summary = if outcome.is_null() {
        let running = Reflect::get(props, &JsValue::from_str("runningSummary"))?;
        if is_nullish(&running) {
            translate_value(&translate, "command.running", None)?
        } else {
            running
        }
    } else if !is_nullish(&text) {
        text.clone()
    } else if state == "error" {
        translate_value(&translate, "command.failed", None)?
    } else {
        translate_value(&translate, "command.done", None)?
    };
    let name = Reflect::get(&node, &JsValue::from_str("name"))?;
    let title = if is_nullish(&name) {
        translate_value(&translate, "command.title", None)?
    } else {
        name
    };
    let body = text
        .as_string()
        .filter(|value| value.contains('\n'))
        .map_or(JsValue::NULL, |value| JsValue::from_str(&value));
    let (expanded, setter) = use_bool_state(&modules.react)?;
    let open = expanded && !body.is_null();
    let error_data = if state == "error" {
        JsValue::TRUE
    } else {
        JsValue::UNDEFINED
    };
    let leading = if state == "error" {
        create_element(
            &modules.react,
            &modules.state_dot,
            Some(&object(&[("state", JsValue::from_str("error"))])?),
            &[],
        )?
    } else {
        create_element(
            &modules.react,
            &modules.api_icon,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?
    };
    let collapsed = create_element(
        &modules.react,
        &modules.fragment,
        None,
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-command-separator"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-command-summary"),
                    ),
                    ("data-error", error_data.clone()),
                ])?),
                &[summary],
            )?,
        ],
    )?;
    let disclosure = create_element(
        &modules.react,
        &modules.disclosure_row,
        Some(&object(&[
            (
                "rowClassName",
                JsValue::from_str("seekdeep-conversation-command-row"),
            ),
            (
                "leadingClassName",
                JsValue::from_str("seekdeep-conversation-command-leading"),
            ),
            (
                "titleClassName",
                JsValue::from_str("seekdeep-conversation-command-title"),
            ),
            (
                "chevronClassName",
                JsValue::from_str("seekdeep-conversation-command-chevron"),
            ),
            ("icon", leading),
            ("title", title),
            ("open", JsValue::from_bool(open)),
            ("expandable", JsValue::from_bool(!body.is_null())),
            ("expandOnRowClick", JsValue::TRUE),
            ("keepContentWhenOpen", JsValue::TRUE),
            ("onToggle", toggle_handler(setter)),
            ("collapsedContent", collapsed),
        ])?),
        &[create_element(
            &modules.react,
            &JsValue::from_str("pre"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-command-body"),
                ),
                ("data-error", error_data),
            ])?),
            &[body],
        )?],
    )?;
    let mut children = Vec::new();
    if state == "running" {
        children.push(hidden_status(
            modules,
            translate_value(&translate, "row.running", None)?,
        )?);
    }
    if state == "error" {
        children.push(hidden_status(
            modules,
            translate_value(&translate, "row.failed", None)?,
        )?);
    }
    children.push(disclosure);
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-command-root"),
            ),
            ("data-variant", JsValue::from_str("others")),
            ("data-state", JsValue::from_str(state)),
        ])?),
        &children,
    )
}

fn hidden_status(modules: &BrowserModules, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-conversation-accessibility-visuallyHidden"),
        )])?),
        &[text],
    )
}

fn render_compaction_command_card(
    modules: &BrowserModules,
    components: &BrowserComponents,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "CompactionCommandCard props")?;
    let translate = required_property(props, "t", "CompactionCommandCard props")?;
    let compaction = Reflect::get(props, &JsValue::from_str("compaction"))?;
    if !compaction.is_undefined() {
        let outcome = Reflect::get(&node, &JsValue::from_str("outcome"))?;
        let outcome_text = if outcome.is_null() {
            JsValue::NULL
        } else {
            let text = Reflect::get(&outcome, &JsValue::from_str("text"))?;
            if is_nullish(&text) {
                JsValue::NULL
            } else {
                text
            }
        };
        return create_element(
            &modules.react,
            &components.compaction_item,
            Some(&object(&[
                ("node", compaction),
                ("title", JsValue::from_str("compact")),
                ("fallbackSummary", outcome_text),
                ("t", translate),
            ])?),
            &[],
        );
    }
    let outcome = Reflect::get(&node, &JsValue::from_str("outcome"))?;
    let mut generic_props = vec![("node", node), ("t", translate.clone())];
    if outcome.is_null() {
        generic_props.push((
            "runningSummary",
            translate_value(
                &translate.dyn_into::<Function>()?,
                "message.compaction.running",
                None,
            )?,
        ));
    }
    create_element(
        &modules.react,
        &components.generic_command_card,
        Some(&object(&generic_props)?),
        &[],
    )
}

#[allow(clippy::too_many_lines)] // Closed checkpoint disclosure tree stays auditable in one renderer.
fn render_compaction_item(
    modules: &BrowserModules,
    _components: &BrowserComponents,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required_property(props, "node", "CompactionItem props")?;
    let translate = required_function(props, "t", "CompactionItem props")?;
    let summary_body = Reflect::get(&node, &JsValue::from_str("summary"))?;
    let expandable = !summary_body.is_null();
    let (expanded, setter) = use_bool_state(&modules.react)?;
    let open = expandable && expanded;
    let shadowed_items = Reflect::get(&node, &JsValue::from_str("shadowedItemCount"))?;
    let shadowed_tokens = Reflect::get(&node, &JsValue::from_str("shadowedTokenCount"))?;
    let summary = if !shadowed_items.is_null() && !shadowed_tokens.is_null() {
        translate_value(
            &translate,
            "message.compaction.completed",
            Some(&object(&[
                ("items", shadowed_items),
                ("tokens", shadowed_tokens),
            ])?),
        )?
    } else {
        let fallback = Reflect::get(props, &JsValue::from_str("fallbackSummary"))?;
        if !is_nullish(&fallback) {
            fallback
        } else if expandable {
            translate_value(&translate, "message.compaction.expand", None)?
        } else {
            translate_value(&translate, "message.compaction.unavailable", None)?
        }
    };
    let title = Reflect::get(props, &JsValue::from_str("title"))?;
    let title = if is_nullish(&title) {
        translate_value(&translate, "message.compaction", None)?
    } else {
        title
    };
    let context_icon = create_element(&modules.react, &modules.api_icon, None, &[])?;
    let disclosure_icon = create_element(
        &modules.react,
        if open {
            &modules.chevron_down
        } else {
            &modules.chevron_right
        },
        None,
        &[],
    )?;
    let button = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-message-compactionButton"),
            ),
            ("disabled", JsValue::from_bool(!expandable)),
            (
                "aria-expanded",
                if expandable {
                    JsValue::from_bool(open)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("onClick", toggle_handler(setter)),
        ])?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-message-compactionLeading"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[
                    create_element(
                        &modules.react,
                        &JsValue::from_str("span"),
                        Some(&object(&[
                            (
                                "className",
                                JsValue::from_str(
                                    "seekdeep-conversation-message-compactionContextIcon",
                                ),
                            ),
                            ("data-compaction-icon", JsValue::from_str("context")),
                        ])?),
                        &[context_icon],
                    )?,
                    create_element(
                        &modules.react,
                        &JsValue::from_str("span"),
                        Some(&object(&[
                            (
                                "className",
                                JsValue::from_str(
                                    "seekdeep-conversation-message-compactionDisclosureIcon",
                                ),
                            ),
                            (
                                "data-compaction-disclosure",
                                JsValue::from_str(if open { "expanded" } else { "collapsed" }),
                            ),
                        ])?),
                        &[disclosure_icon],
                    )?,
                ],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-conversation-message-compactionTitle"),
                )])?),
                &[title],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-message-compactionSep"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("span"),
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-conversation-message-compactionSummary"),
                )])?),
                &[summary],
            )?,
        ],
    )?;
    let mut children = vec![button];
    if open && !summary_body.is_null() {
        children.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-conversation-message-compactionBody"),
            )])?),
            &[create_element(
                &modules.react,
                &modules.markdown_text,
                Some(&object(&[("text", summary_body)])?),
                &[],
            )?],
        )?);
    }
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-conversation-message-compactionRow"),
        )])?),
        &children,
    )
}

fn use_bool_state(react: &JsValue) -> Result<(bool, Function), JsValue> {
    let state = required_function(react, "useState", "React")?
        .call1(react, &JsValue::FALSE)?
        .dyn_into::<Array>()?;
    let value = state
        .get(0)
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("React useState boolean value was not a boolean"))?;
    let setter = state.get(1).dyn_into::<Function>()?;
    Ok((value, setter))
}

fn toggle_handler(setter: Function) -> JsValue {
    let updater = Closure::wrap(
        Box::new(move |value: JsValue| !value.as_bool().unwrap_or(false))
            as Box<dyn FnMut(JsValue) -> bool>,
    )
    .into_js_value();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        setter.call1(&JsValue::UNDEFINED, &updater)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
}

fn translate_value(
    translate: &Function,
    key: &str,
    parameters: Option<&Object>,
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(&JsValue::from_str(key));
    if let Some(parameters) = parameters {
        arguments.push(parameters);
    }
    translate.apply(&JsValue::UNDEFINED, &arguments)
}

#[allow(clippy::too_many_lines)] // Complete CSS-module maps must remain identical across assembly order.
fn inject_command_styles() -> Result<(), JsValue> {
    inject_style(
        "ChatView",
        CHAT_CSS,
        &[
            ("callRow", "seekdeep-conversation-chat-callRow"),
            ("column", "seekdeep-conversation-chat-column"),
            ("flowItem", "seekdeep-conversation-chat-flowItem"),
            ("hint", "seekdeep-conversation-chat-hint"),
            ("older", "seekdeep-conversation-chat-older"),
            ("openError", "seekdeep-conversation-chat-openError"),
            ("root", "seekdeep-conversation-chat-root"),
            ("scroll", "seekdeep-conversation-chat-scroll"),
            ("toBottom", "seekdeep-conversation-chat-toBottom"),
            ("toBottomSlot", "seekdeep-conversation-chat-toBottomSlot"),
            ("turnStatus", "seekdeep-conversation-chat-turnStatus"),
            (
                "turnStatusClock",
                "seekdeep-conversation-chat-turnStatusClock",
            ),
        ],
    )?;
    inject_style(
        "GenericCommandCard",
        COMMAND_CSS,
        &[
            ("body", "seekdeep-conversation-command-body"),
            ("chevron", "seekdeep-conversation-command-chevron"),
            ("leading", "seekdeep-conversation-command-leading"),
            ("root", "seekdeep-conversation-command-root"),
            ("row", "seekdeep-conversation-command-row"),
            ("separator", "seekdeep-conversation-command-separator"),
            ("summary", "seekdeep-conversation-command-summary"),
            ("title", "seekdeep-conversation-command-title"),
        ],
    )?;
    inject_style(
        "MessageItem",
        MESSAGE_CSS,
        &[
            ("bubble", "seekdeep-conversation-message-bubble"),
            (
                "compactionBody",
                "seekdeep-conversation-message-compactionBody",
            ),
            (
                "compactionButton",
                "seekdeep-conversation-message-compactionButton",
            ),
            (
                "compactionContextIcon",
                "seekdeep-conversation-message-compactionContextIcon",
            ),
            (
                "compactionDisclosureIcon",
                "seekdeep-conversation-message-compactionDisclosureIcon",
            ),
            (
                "compactionLeading",
                "seekdeep-conversation-message-compactionLeading",
            ),
            (
                "compactionRow",
                "seekdeep-conversation-message-compactionRow",
            ),
            (
                "compactionSep",
                "seekdeep-conversation-message-compactionSep",
            ),
            (
                "compactionSummary",
                "seekdeep-conversation-message-compactionSummary",
            ),
            (
                "compactionTitle",
                "seekdeep-conversation-message-compactionTitle",
            ),
            ("contextRow", "seekdeep-conversation-message-contextRow"),
            (
                "maxTokensTitle",
                "seekdeep-conversation-message-maxTokensTitle",
            ),
            ("refChip", "seekdeep-conversation-message-refChip"),
            (
                "retryDetailLabel",
                "seekdeep-conversation-message-retryDetailLabel",
            ),
            ("retryDetails", "seekdeep-conversation-message-retryDetails"),
            ("retryRow", "seekdeep-conversation-message-retryRow"),
            ("retrySummary", "seekdeep-conversation-message-retrySummary"),
            ("retryText", "seekdeep-conversation-message-retryText"),
            (
                "turnErrorCode",
                "seekdeep-conversation-message-turnErrorCode",
            ),
            (
                "turnErrorCopy",
                "seekdeep-conversation-message-turnErrorCopy",
            ),
            ("turnErrorDot", "seekdeep-conversation-message-turnErrorDot"),
            (
                "turnErrorMessage",
                "seekdeep-conversation-message-turnErrorMessage",
            ),
            ("turnErrorRow", "seekdeep-conversation-message-turnErrorRow"),
            (
                "turnErrorTitle",
                "seekdeep-conversation-message-turnErrorTitle",
            ),
            ("userRow", "seekdeep-conversation-message-userRow"),
            ("userStack", "seekdeep-conversation-message-userStack"),
        ],
    )?;
    inject_style(
        "accessibility",
        ACCESSIBILITY_CSS,
        &[(
            "visuallyHidden",
            "seekdeep-conversation-accessibility-visuallyHidden",
        )],
    )
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation command renderers were not configured")
                .into()
        })
    })
}

fn configured_components() -> Result<BrowserComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation command components were not configured")
                .into()
        })
    })
}

fn is_nullish(value: &JsValue) -> bool {
    value.is_null() || value.is_undefined()
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
    if is_nullish(&property) {
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
