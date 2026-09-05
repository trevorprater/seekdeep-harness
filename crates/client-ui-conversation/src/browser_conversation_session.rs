//! Compiled strict-session header and active-view body.

use std::{cell::RefCell, collections::BTreeSet};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;

const SESSION_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/ConversationRoot.module.css"
);
const DEFAULT_VIEW_ID: &str = "chat";

thread_local! {
    static COMPONENTS: RefCell<Option<SessionComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
}

#[derive(Clone)]
struct SessionComponents {
    header: JsValue,
    body: JsValue,
}

/// Configures the compiled strict-session header/body family.
///
/// # Errors
///
/// Returns on missing React faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationSession)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_session(react: JsValue) -> Result<(), JsValue> {
    for method in ["createElement", "useEffect", "useSyncExternalStore"] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        react,
    };
    inject_style(
        "ConversationRoot",
        SESSION_CSS,
        &[
            ("Tab", "seekdeep-conversation-session-Tab"),
            ("agents", "seekdeep-conversation-session-agents"),
            ("composerHero", "seekdeep-conversation-session-composerHero"),
            ("composerSeat", "seekdeep-conversation-session-composerSeat"),
            (
                "composerStack",
                "seekdeep-conversation-session-composerStack",
            ),
            ("crumb", "seekdeep-conversation-session-crumb"),
            ("crumbCurrent", "seekdeep-conversation-session-crumbCurrent"),
            ("crumbs", "seekdeep-conversation-session-crumbs"),
            ("crumbSeg", "seekdeep-conversation-session-crumbSeg"),
            ("crumbSep", "seekdeep-conversation-session-crumbSep"),
            ("header", "seekdeep-conversation-session-header"),
            (
                "headerActions",
                "seekdeep-conversation-session-headerActions",
            ),
            ("headerHidden", "seekdeep-conversation-session-headerHidden"),
            (
                "headerUtilities",
                "seekdeep-conversation-session-headerUtilities",
            ),
            ("heroGlow", "seekdeep-conversation-session-heroGlow"),
            (
                "heroWorkspaceRow",
                "seekdeep-conversation-session-heroWorkspaceRow",
            ),
            ("md", "seekdeep-conversation-session-md"),
            ("root", "seekdeep-conversation-session-root"),
            ("scrollBody", "seekdeep-conversation-session-scrollBody"),
            ("session", "seekdeep-conversation-session-session"),
            ("tab", "seekdeep-conversation-session-tab"),
            ("tabActive", "seekdeep-conversation-session-tabActive"),
            ("tabs", "seekdeep-conversation-session-tabs"),
            ("titleCluster", "seekdeep-conversation-session-titleCluster"),
            ("titleRow", "seekdeep-conversation-session-titleRow"),
            ("viewArea", "seekdeep-conversation-session-viewArea"),
        ],
    )?;
    let header_modules = modules.clone();
    let header = raw_component(move |props| render_header(&header_modules, props));
    let body_modules = modules;
    let body = raw_component(move |props| render_body(&body_modules, props));
    COMPONENTS
        .with(|configured| *configured.borrow_mut() = Some(SessionComponents { header, body }));
    Ok(())
}

fn raw_component<F>(renderer: F) -> JsValue
where
    F: 'static + FnMut(&JsValue) -> Result<JsValue, JsValue>,
{
    let mut renderer = renderer;
    Closure::wrap(Box::new(move |props: JsValue| renderer(&props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

/// Returns the compiled Session header.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = conversationSessionHeaderComponent)]
pub fn conversation_session_header_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.header)
}

/// Returns the compiled active Session body.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = conversationSessionComponent)]
pub fn conversation_session_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.body)
}

#[allow(clippy::too_many_lines)] // Closed header selector and chrome tree stay together.
fn render_header(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let views = required_property(props, "views", "ConversationSessionHeader props")?;
    bind_view_ledger(&modules.react, &views)?;
    let tabs = call_method(&views, "list", &[])?.dyn_into::<Array>()?;
    let selected = select_store(props, "view")?;
    let active = resolve_active_view(&tabs, &selected)?;
    let session_id = required_string(props, "sessionId", "ConversationSessionHeader props")?;
    let ancestry_id = session_id.clone();
    let ancestry_selector =
        Closure::wrap(
            Box::new(move |list: JsValue| derive_ancestry(&list, &ancestry_id))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    let equality = Closure::wrap(Box::new(move |left: JsValue, right: JsValue| {
        equal_breadcrumbs(&left, &right)
    })
        as Box<dyn FnMut(JsValue, JsValue) -> Result<bool, JsValue>>);
    let ancestry = required_function(props, "useSessions", "ConversationSessionHeader props")?
        .call2(
            &JsValue::UNDEFINED,
            &ancestry_selector.into_js_value(),
            &equality.into_js_value(),
        )?
        .dyn_into::<Array>()?;
    let composer_phase = select_session(props, "composerPhase")?;
    let blank = select_session(props, "blank")?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("Session blank must be a boolean"))?;
    let hide_chrome = blank && composer_phase.as_string().as_deref() == Some("blank");
    let children = if hide_chrome {
        JsValue::FALSE
    } else {
        render_header_children(
            modules,
            props,
            &tabs,
            active.as_ref(),
            &ancestry,
            &session_id,
        )?
    };
    create_element(
        &modules.react,
        &JsValue::from_str("header"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str(if hide_chrome {
                    "seekdeep-conversation-session-header seekdeep-conversation-session-headerHidden"
                } else {
                    "seekdeep-conversation-session-header"
                }),
            ),
            (
                "aria-hidden",
                if hide_chrome {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
        ])?),
        &[children],
    )
}

fn render_header_children(
    modules: &BrowserModules,
    props: &JsValue,
    tabs: &Array,
    active: Option<&JsValue>,
    ancestry: &Array,
    session_id: &str,
) -> Result<JsValue, JsValue> {
    let translate = required_function(props, "t", "ConversationSessionHeader props")?;
    let hierarchy_label =
        translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("session.hierarchy"))?;
    let crumbs = render_crumbs(modules, props, ancestry, session_id, hierarchy_label)?;
    let render_slot = required_function(props, "renderSlot", "ConversationSessionHeader props")?;
    let actions = render_slot.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("conversation.session.header.actions"),
            Object::new().as_ref(),
        ),
    )?;
    let utilities = render_slot.apply(
        &JsValue::UNDEFINED,
        &Array::of2(
            &JsValue::from_str("conversation.session.header.utilities"),
            Object::new().as_ref(),
        ),
    )?;
    let title_row = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-session-titleRow")?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-conversation-session-titleCluster")?),
                &[
                    crumbs,
                    create_element(
                        &modules.react,
                        &JsValue::from_str("div"),
                        Some(&class_props("seekdeep-conversation-session-headerActions")?),
                        &[actions],
                    )?,
                ],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props(
                    "seekdeep-conversation-session-headerUtilities",
                )?),
                &[utilities],
            )?,
        ],
    )?;
    let tab_row = if tabs.length() > 1 {
        render_tabs(modules, props, tabs, active)?
    } else {
        JsValue::FALSE
    };
    create_element(
        &modules.react,
        &modules.fragment,
        None,
        &[title_row, tab_row],
    )
}

fn render_crumbs(
    modules: &BrowserModules,
    props: &JsValue,
    ancestry: &Array,
    session_id: &str,
    label: JsValue,
) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    let open = required_function(props, "open", "ConversationSessionHeader props")?;
    for index in 0..ancestry.length() {
        let summary = ancestry.get(index);
        let id = required_string(&summary, "id", "breadcrumb")?;
        let title = required_property(&summary, "displayTitle", "breadcrumb")?;
        let last = index == ancestry.length() - 1;
        let opener = open.clone();
        let open_id = id.clone();
        let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            opener.call1(&JsValue::UNDEFINED, &JsValue::from_str(&open_id))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let separator = if index > 0 {
            span(modules, "crumbSep", JsValue::from_str("/"))?
        } else {
            JsValue::FALSE
        };
        let button = create_element(
            &modules.react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(if last {
                        "seekdeep-conversation-session-crumb seekdeep-conversation-session-crumbCurrent"
                    } else {
                        "seekdeep-conversation-session-crumb"
                    }),
                ),
                ("disabled", JsValue::from_bool(last)),
                ("onClick", on_click),
            ])?),
            &[title],
        )?;
        children.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-session-crumbSeg"),
                ),
            ])?),
            &[separator, button],
        )?);
    }
    if ancestry.length() == 0 {
        children.push(span(
            modules,
            "crumbCurrent",
            JsValue::from_str(session_id),
        )?);
    }
    create_element(
        &modules.react,
        &JsValue::from_str("nav"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-session-crumbs"),
            ),
            ("aria-label", label),
        ])?),
        &children,
    )
}

fn render_tabs(
    modules: &BrowserModules,
    props: &JsValue,
    tabs: &Array,
    active: Option<&JsValue>,
) -> Result<JsValue, JsValue> {
    let active_id = if let Some(value) = active {
        Reflect::get(value, &JsValue::from_str("id"))?.as_string()
    } else {
        None
    };
    let actions = required_property(props, "actions", "ConversationSessionHeader props")?;
    let mut buttons = Vec::new();
    for index in 0..tabs.length() {
        let tab = tabs.get(index);
        let id = required_string(&tab, "id", "view tab")?;
        let selected = active_id.as_deref() == Some(id.as_str());
        let setter_actions = actions.clone();
        let view_id = id.clone();
        let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            required_function(&setter_actions, "setView", "chat actions")?
                .call1(&setter_actions, &JsValue::from_str(&view_id))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        buttons.push(create_element(
            &modules.react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                ("type", JsValue::from_str("button")),
                ("role", JsValue::from_str("tab")),
                ("aria-selected", JsValue::from_bool(selected)),
                (
                    "className",
                    JsValue::from_str(if selected {
                        "seekdeep-conversation-session-tab seekdeep-conversation-session-tabActive"
                    } else {
                        "seekdeep-conversation-session-tab"
                    }),
                ),
                ("onClick", on_click),
            ])?),
            &[required_property(&tab, "label", "view tab")?],
        )?);
    }
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-session-tabs"),
            ),
            ("role", JsValue::from_str("tablist")),
        ])?),
        &buttons,
    )
}

#[allow(clippy::too_many_lines)] // Mount effects and active-view dispatch form one lifecycle boundary.
fn render_body(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let views = required_property(props, "views", "ConversationSession props")?;
    bind_view_ledger(&modules.react, &views)?;
    let tabs = call_method(&views, "list", &[])?.dyn_into::<Array>()?;
    let selected = select_store(props, "view")?;
    let active = resolve_active_view(&tabs, &selected)?;
    let composer_phase = select_session(props, "composerPhase")?;
    let blank = select_session(props, "blank")?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("Session blank must be a boolean"))?;
    let input_state = select_with_identity(props, "useInput", "ConversationSession props")?;
    let stored_draft = select_store(props, "draft")?;
    let inspect = select_store_nullish(props, "inspect")?;
    install_draft_effect(&modules.react, props, &input_state, &stored_draft)?;
    install_image_cleanup(&modules.react, props)?;
    if blank && composer_phase.as_string().as_deref() == Some("blank") {
        return Ok(JsValue::NULL);
    }
    let content = if let Some(active) = active {
        let actions = required_property(props, "actions", "ConversationSession props")?;
        let inspect_actions = actions.clone();
        let on_done = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            required_function(&inspect_actions, "setInspect", "chat actions")?
                .call1(&inspect_actions, &JsValue::NULL)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let only = required_property(&active, "id", "active view tab")?;
        required_function(props, "renderSlot", "ConversationSession props")?.apply(
            &JsValue::UNDEFINED,
            &Array::of3(
                &JsValue::from_str("conversation.view"),
                object(&[("inspect", inspect), ("onInspectDone", on_done)])?.as_ref(),
                object(&[("only", only)])?.as_ref(),
            ),
        )?
    } else {
        JsValue::FALSE
    };
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-session-viewArea")?),
        &[content],
    )
}

fn bind_view_ledger(react: &JsValue, views: &JsValue) -> Result<(), JsValue> {
    let subscribe = required_function(views, "subscribe", "conversation views")?;
    let version = required_function(views, "version", "conversation views")?;
    required_function(react, "useSyncExternalStore", "React")?.call2(
        react,
        subscribe.as_ref(),
        version.as_ref(),
    )?;
    Ok(())
}

fn resolve_active_view(tabs: &Array, selected: &JsValue) -> Result<Option<JsValue>, JsValue> {
    let requested = if selected.is_null() || selected.is_undefined() {
        DEFAULT_VIEW_ID.to_owned()
    } else {
        selected
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("selected view id must be a string or null"))?
    };
    for wanted in [requested.as_str(), DEFAULT_VIEW_ID] {
        for index in 0..tabs.length() {
            let tab = tabs.get(index);
            if required_string(&tab, "id", "view tab")? == wanted {
                return Ok(Some(tab));
            }
        }
    }
    Ok(None)
}

fn derive_ancestry(list: &JsValue, id: &str) -> Result<JsValue, JsValue> {
    let by_id = required_property(list, "byId", "session list")?;
    let mut chain = Vec::<JsValue>::new();
    let mut seen = BTreeSet::new();
    let mut cursor = Some(id.to_owned());
    while let Some(current) = cursor {
        if !seen.insert(current.clone()) {
            break;
        }
        let summary = Reflect::get(&by_id, &JsValue::from_str(&current))?;
        if summary.is_undefined() {
            break;
        }
        chain.insert(
            0,
            object(&[
                ("id", required_property(&summary, "id", "session summary")?),
                (
                    "displayTitle",
                    required_property(&summary, "displayTitle", "session summary")?,
                ),
            ])?
            .into(),
        );
        if Reflect::get(&summary, &JsValue::from_str("origin"))?
            .as_string()
            .as_deref()
            != Some("subagent")
        {
            break;
        }
        let parent = Reflect::get(&summary, &JsValue::from_str("parentId"))?;
        cursor = parent.as_string();
    }
    Ok(chain.iter().collect::<Array>().into())
}

fn equal_breadcrumbs(left: &JsValue, right: &JsValue) -> Result<bool, JsValue> {
    let left = left.clone().dyn_into::<Array>()?;
    let right = right.clone().dyn_into::<Array>()?;
    if left.length() != right.length() {
        return Ok(false);
    }
    for index in 0..left.length() {
        let left_item = left.get(index);
        let right_item = right.get(index);
        for field in ["id", "displayTitle"] {
            if Reflect::get(&left_item, &JsValue::from_str(field))?.as_string()
                != Reflect::get(&right_item, &JsValue::from_str(field))?.as_string()
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn select_store(props: &JsValue, field: &'static str) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(Box::new(move |state: JsValue| {
        Reflect::get(&state, &JsValue::from_str(field))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    required_function(props, "useStore", "conversation store props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn select_store_nullish(props: &JsValue, field: &'static str) -> Result<JsValue, JsValue> {
    let value = select_store(props, field)?;
    Ok(if value.is_null() || value.is_undefined() {
        JsValue::NULL
    } else {
        value
    })
}

fn select_session(props: &JsValue, field: &'static str) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(Box::new(move |state: JsValue| {
        Reflect::get(&state, &JsValue::from_str(field))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    required_function(props, "useSession", "session props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn select_with_identity(props: &JsValue, hook: &str, owner: &str) -> Result<JsValue, JsValue> {
    let selector =
        Closure::wrap(Box::new(move |state: JsValue| state) as Box<dyn FnMut(JsValue) -> JsValue>);
    required_function(props, hook, owner)?.call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn install_draft_effect(
    react: &JsValue,
    props: &JsValue,
    input_state: &JsValue,
    stored_draft: &JsValue,
) -> Result<(), JsValue> {
    let input_actions = required_property(props, "inputActions", "ConversationSession props")?;
    let effect_actions = input_actions.clone();
    let draft = Reflect::get(input_state, &JsValue::from_str("draft"))?;
    let stored = stored_draft.clone();
    let bind = required_function(props, "bindDraftMirror", "ConversationSession props")?;
    let actions = required_property(props, "actions", "ConversationSession props")?;
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if draft.as_string().as_deref() == Some("")
            && stored.as_string().is_some_and(|value| !value.is_empty())
        {
            required_function(&effect_actions, "setDraft", "input actions")?
                .call1(&effect_actions, &stored)?;
        }
        let set_draft = required_function(&actions, "setDraft", "chat actions")?;
        let unmirror = bind.call1(&JsValue::UNDEFINED, set_draft.as_ref())?;
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            unmirror
                .clone()
                .dyn_into::<Function>()?
                .call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        Ok(cleanup)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(&input_actions),
    )?;
    Ok(())
}

fn install_image_cleanup(react: &JsValue, props: &JsValue) -> Result<(), JsValue> {
    let release = required_function(props, "releaseSessionImages", "ConversationSession props")?;
    let session_id = required_property(props, "sessionId", "ConversationSession props")?;
    let effect_release = release.clone();
    let effect_session_id = session_id.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        let release = effect_release.clone();
        let session_id = effect_session_id.clone();
        Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            release.call1(&JsValue::UNDEFINED, &session_id)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of2(release.as_ref(), &session_id),
    )?;
    Ok(())
}

fn span(modules: &BrowserModules, class: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("span"),
        Some(&class_props(&format!(
            "seekdeep-conversation-session-{class}"
        ))?),
        &[text],
    )
}

fn configured_components() -> Result<SessionComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation Session was not configured").into()
        })
    })
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
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
