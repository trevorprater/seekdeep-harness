//! Compiled optional-session conversation root and resident composer owner.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;
use crate::{
    configure_client_ui_conversation_empty_hero, hero_glow_component, hero_shell_component,
    workspace_chip_component, workspace_label_browser,
};

const ROOT_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/ConversationRoot.module.css"
);

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    workspace_chip: JsValue,
    hero_glow: JsValue,
    hero_shell: JsValue,
}

/// Configures the compiled optional-session conversation root.
///
/// # Errors
///
/// Returns on missing React/hero faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationRoot)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_root(
    react: JsValue,
    ui_primitives: JsValue,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useCallback",
        "useEffect",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    configure_client_ui_conversation_empty_hero(react.clone(), ui_primitives)?;
    inject_style(
        "ConversationRoot",
        ROOT_CSS,
        &[
            ("Tab", "seekdeep-conversation-session-Tab"),
            ("agents", "seekdeep-conversation-session-agents"),
            ("composerHero", class("composerHero")),
            ("composerSeat", class("composerSeat")),
            ("composerStack", class("composerStack")),
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
            ("heroGlow", class("heroGlow")),
            ("heroWorkspaceRow", class("heroWorkspaceRow")),
            ("md", "seekdeep-conversation-session-md"),
            ("root", class("root")),
            ("scrollBody", class("scrollBody")),
            ("session", "seekdeep-conversation-session-session"),
            ("tab", "seekdeep-conversation-session-tab"),
            ("tabActive", "seekdeep-conversation-session-tabActive"),
            ("tabs", "seekdeep-conversation-session-tabs"),
            ("titleCluster", "seekdeep-conversation-session-titleCluster"),
            ("titleRow", "seekdeep-conversation-session-titleRow"),
            ("viewArea", "seekdeep-conversation-session-viewArea"),
        ],
    )?;
    let modules = BrowserModules {
        workspace_chip: workspace_chip_component()?,
        hero_glow: hero_glow_component()?,
        hero_shell: hero_shell_component()?,
        react,
    };
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_conversation_root(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `ConversationRoot` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = conversationRootComponent)]
pub fn conversation_root_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation ConversationRoot was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed optional-session owner tree and Hook order stay together.
fn render_conversation_root(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let session_id = Reflect::get(props, &JsValue::from_str("sessionId"))?;
    let open_state = select_field(props, "useSession", "openState")?;
    let composer_phase = select_field(props, "useSession", "composerPhase")?;
    let pending = nullish_to(
        select_field(props, "useSession", "pending")?,
        Array::new().into(),
    );
    let session = select_identity(props, "useSession")?;
    let input_state = select_identity(props, "useInput")?;
    let cwd = select_session_summary(props, &session_id, "cwd")?;
    let summary_blank = select_session_summary(props, &session_id, "blank")?;
    let workspaces = select_identity(props, "useWorkspaces")?;
    let composer_block = select_identity(props, "useComposerBlock")?;
    let (picker_open_value, set_picker_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let picker_open = picker_open_value.as_bool().unwrap_or(false);
    let (pending_workspace_id, set_pending_workspace_id) =
        use_state(&modules.react, &JsValue::UNDEFINED)?;
    let picker_anchor = use_ref(&modules.react, &JsValue::NULL)?;
    let seat_observer = use_ref(&modules.react, &JsValue::NULL)?;
    let seat_resize_ref = seat_resize_callback(&modules.react, &seat_observer)?;

    let items = required(&workspaces, "items", "workspace snapshot")?.dyn_into::<Array>()?;
    let session_workspace = if session_id.is_undefined() {
        JsValue::UNDEFINED
    } else {
        find_workspace_for_session(&items, &session_id)?
    };
    let pending_workspace = if pending_workspace_id.is_undefined() {
        JsValue::UNDEFINED
    } else {
        find_workspace(&items, &pending_workspace_id)?
    };
    install_pending_workspace_effect(
        &modules.react,
        &pending_workspace_id,
        &session_workspace,
        &workspaces,
        &pending_workspace,
        &set_pending_workspace_id,
    )?;

    let session_present = !session_id.is_undefined();
    let phase = composer_phase.as_string().unwrap_or_default();
    let open = open_state.as_string().unwrap_or_default();
    let summary_proves_blank = summary_blank.as_bool() == Some(true);
    let settling =
        session_present && phase == "blank" && open == "loading" && !summary_proves_blank;
    let hero = !session_present || (phase == "blank" && (open == "open" || summary_proves_blank));
    let zone = if session.is_undefined() || input_state.is_undefined() {
        JsValue::UNDEFINED
    } else {
        object(&[("session", session.clone()), ("input", input_state.clone())])?.into()
    };
    let workspace_phase = required_string(&workspaces, "phase", "workspace snapshot")?;
    let chip_title = workspace_title(
        &pending_workspace,
        &session_id,
        &session_workspace,
        &workspace_phase,
        &cwd,
    )?;
    let translate = required_function(props, "t", "ConversationRoot props")?;
    let render_slot = required_function(props, "renderSlot", "ConversationRoot props")?;
    let hero_workspace_row = render_workspace_row(
        modules,
        props,
        &render_slot,
        &translate,
        &picker_anchor,
        picker_open,
        &chip_title,
        &pending_workspace_id,
        &session_workspace,
        &set_picker_open,
        &set_pending_workspace_id,
    )?;
    let inert = !session_present || (hero && chip_title.is_undefined());
    let blocked = !inert && !composer_block.is_undefined();
    let bar_owner = composer_bar_owner(
        &render_slot,
        &translate,
        hero,
        inert,
        blocked,
        picker_open,
        &composer_block,
        &zone,
        &set_picker_open,
    )?;
    let input_bar = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.composer.bar"),
        &bar_owner,
    )?;
    let composer_bar = render_composer_stack(
        modules,
        &render_slot,
        &translate,
        hero,
        &zone,
        hero_workspace_row,
        input_bar,
    )?;
    let phase_name = if settling {
        "settling"
    } else if hero {
        "hero"
    } else {
        "active"
    };
    let render_chain = required_function(props, "renderSlotChain", "ConversationRoot props")?;
    let composer = render_chain.apply(
        &JsValue::UNDEFINED,
        &Array::of3(
            &JsValue::from_str("conversation.composer"),
            object(&[("interactions", pending), ("session", session.clone())])?.as_ref(),
            object(&[("fallback", composer_bar), ("overlay", JsValue::TRUE)])?.as_ref(),
        ),
    )?;
    let composer_seat = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", seat_resize_ref),
            ("className", JsValue::from_str(class("composerSeat"))),
            ("data-composer-seat", JsValue::from_str("")),
        ])?),
        &[composer],
    )?;
    let header = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.session.header"),
        Object::new().as_ref(),
    )?;
    let session_body = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.session"),
        Object::new().as_ref(),
    )?;
    let scroll_body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("className", JsValue::from_str(class("scrollBody"))),
            ("data-conversation-scroll", JsValue::from_str("")),
        ])?),
        &[session_body, composer_seat],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("className", JsValue::from_str(class("root"))),
            ("data-phase", JsValue::from_str(phase_name)),
        ])?),
        &[header, scroll_body],
    )
}

fn seat_resize_callback(react: &JsValue, observer_ref: &JsValue) -> Result<JsValue, JsValue> {
    let observer_ref = observer_ref.clone();
    let callback = Closure::wrap(Box::new(move |seat: JsValue| -> Result<(), JsValue> {
        let observer = ref_current(&observer_ref)?;
        if !observer.is_null() && !observer.is_undefined() {
            call_method(&observer, "disconnect", &[])?;
        }
        set_ref_current(&observer_ref, &JsValue::NULL)?;
        if seat.is_null() {
            return Ok(());
        }
        let scroller = Reflect::get(&seat, &JsValue::from_str("parentElement"))?;
        if scroller.is_null() || scroller.is_undefined() {
            return Ok(());
        }
        let callback_scroller = scroller.clone();
        let callback_seat = seat.clone();
        let resize = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let style = required(&callback_scroller, "style", "composer scroller")?;
            let height = numeric_required(&callback_seat, "offsetHeight", "composer seat")?;
            call_method(
                &style,
                "setProperty",
                &[
                    JsValue::from_str("--seekdeep-composer-height"),
                    JsValue::from_str(&format!("{}px", number_string(height)?)),
                ],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let constructor =
            required(&js_sys::global(), "ResizeObserver", "global")?.dyn_into::<Function>()?;
        let observer = Reflect::construct(&constructor, &Array::of1(&resize))?;
        call_method(&observer, "observe", std::slice::from_ref(&seat))?;
        set_ref_current(&observer_ref, &observer)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    use_callback(react, callback.into_js_value(), &Array::new())
}

fn install_pending_workspace_effect(
    react: &JsValue,
    pending_id: &JsValue,
    session_workspace: &JsValue,
    workspaces: &JsValue,
    pending_workspace: &JsValue,
    setter: &Function,
) -> Result<(), JsValue> {
    let pending_id = pending_id.clone();
    let session_workspace = session_workspace.clone();
    let workspaces = workspaces.clone();
    let pending_workspace = pending_workspace.clone();
    let setter = setter.clone();
    let session_workspace_id = if session_workspace.is_null() || session_workspace.is_undefined() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(&session_workspace, &JsValue::from_str("workspaceId"))?
    };
    let deps = Array::of4(
        &pending_id,
        &session_workspace_id,
        &required(&workspaces, "phase", "workspace snapshot")?,
        &pending_workspace,
    );
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if pending_id.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let landed = if session_workspace.is_null() || session_workspace.is_undefined() {
            false
        } else {
            Object::is(
                &Reflect::get(&session_workspace, &JsValue::from_str("workspaceId"))?,
                &pending_id,
            )
        };
        let vanished = required_string(&workspaces, "phase", "workspace snapshot")? == "ready"
            && pending_workspace.is_undefined();
        if landed || vanished {
            setter.call1(&JsValue::UNDEFINED, &JsValue::UNDEFINED)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(react, setup.into_js_value(), &deps)
}

#[allow(clippy::too_many_arguments)]
fn render_workspace_row(
    modules: &BrowserModules,
    props: &JsValue,
    render_slot: &Function,
    translate: &Function,
    picker_anchor: &JsValue,
    picker_open: bool,
    chip_title: &JsValue,
    pending_workspace_id: &JsValue,
    session_workspace: &JsValue,
    set_picker_open: &Function,
    set_pending_workspace_id: &Function,
) -> Result<JsValue, JsValue> {
    let toggle_setter = set_picker_open.clone();
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater = Closure::once_into_js(move |open: bool| !open);
        toggle_setter.call1(&JsValue::UNDEFINED, &updater)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let chip = create_element(
        &modules.react,
        &modules.workspace_chip,
        Some(&object(&[
            ("buttonRef", picker_anchor.clone()),
            ("label", chip_title.clone()),
            ("menuOpen", JsValue::from_bool(picker_open)),
            ("onClick", toggle),
            ("t", translate.clone().into()),
        ])?),
        &[],
    )?;
    let selected = if pending_workspace_id.is_undefined() {
        if session_workspace.is_null() || session_workspace.is_undefined() {
            JsValue::UNDEFINED
        } else {
            Reflect::get(session_workspace, &JsValue::from_str("workspaceId"))?
        }
    } else {
        pending_workspace_id.clone()
    };
    let select = required_function(props, "selectWorkspace", "ConversationRoot props")?;
    let pick_open = set_picker_open.clone();
    let pick_pending = set_pending_workspace_id.clone();
    let on_pick = Closure::wrap(
        Box::new(move |workspace_id: JsValue| -> Result<(), JsValue> {
            pick_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            pick_pending.call1(&JsValue::UNDEFINED, &workspace_id)?;
            let returned = select.call1(&JsValue::UNDEFINED, &workspace_id)?;
            let promise = Promise::resolve(&returned);
            let rollback_setter = pick_pending.clone();
            let rollback_id = workspace_id.clone();
            let rollback = Closure::wrap(Box::new(move |_error: JsValue| {
                let rollback_id = rollback_id.clone();
                let updater = Closure::once_into_js(move |current: JsValue| {
                    if Object::is(&current, &rollback_id) {
                        JsValue::UNDEFINED
                    } else {
                        current
                    }
                });
                if let Err(error) = rollback_setter.call1(&JsValue::UNDEFINED, &updater) {
                    wasm_bindgen::throw_val(error);
                }
            }) as Box<dyn FnMut(JsValue)>);
            let _ = promise.catch(&rollback);
            drop(rollback.into_js_value());
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>,
    )
    .into_js_value();
    let close_setter = set_picker_open.clone();
    let on_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let workspace_slot = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.hero.workspace"),
        object(&[
            ("open", JsValue::from_bool(picker_open)),
            ("anchorRef", picker_anchor.clone()),
            ("selectedId", selected),
            ("onPick", on_pick),
            ("onClose", on_close),
        ])?
        .as_ref(),
    )?;
    let preset = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.hero.agentPreset"),
        Object::new().as_ref(),
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class("heroWorkspaceRow"))?),
        &[chip, workspace_slot, preset],
    )
}

#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)] // Mirrors the source's hero/inert/block posture tuple.
fn composer_bar_owner(
    render_slot: &Function,
    translate: &Function,
    hero: bool,
    inert: bool,
    blocked: bool,
    picker_open: bool,
    composer_block: &JsValue,
    zone: &JsValue,
    set_picker_open: &Function,
) -> Result<JsValue, JsValue> {
    let mut values = vec![(
        "variant",
        JsValue::from_str(if hero { "hero" } else { "composer" }),
    )];
    if inert {
        let setter = set_picker_open.clone();
        let request = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        values.extend([
            ("disabled", JsValue::TRUE),
            (
                "placeholder",
                translate.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str("placeholder.workspace"),
                )?,
            ),
            ("workspacePickerOpen", JsValue::from_bool(picker_open)),
            ("onRequestWorkspace", request),
        ]);
    } else if blocked {
        values.push(("blocked", composer_block.clone()));
        values.push((
            "placeholder",
            required(composer_block, "reason", "composer block")?,
        ));
    } else if hero {
        values.push((
            "placeholder",
            translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("placeholder.hero"))?,
        ));
    }
    values.push((
        "overlay",
        render_slot.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("conversation.input.overlay"),
            Object::new().as_ref(),
        )?,
    ));
    let zone_present = !zone.is_undefined();
    values.push((
        "leftItems",
        if zone_present {
            render_slot.call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str("conversation.input.left"),
                zone,
            )?
        } else {
            JsValue::NULL
        },
    ));
    values.push((
        "rightItems",
        if zone_present {
            render_slot.call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str("conversation.input.right"),
                zone,
            )?
        } else {
            JsValue::NULL
        },
    ));
    values.push((
        "footer",
        if !hero && zone_present {
            render_slot.call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str("conversation.composer.dock"),
                zone,
            )?
        } else {
            JsValue::NULL
        },
    ));
    Ok(object(&values)?.into())
}

fn render_composer_stack(
    modules: &BrowserModules,
    render_slot: &Function,
    translate: &Function,
    hero: bool,
    zone: &JsValue,
    workspace_row: JsValue,
    input_bar: JsValue,
) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    if hero {
        children.push(create_element(
            &modules.react,
            &modules.hero_glow,
            Some(&class_props(class("heroGlow"))?),
            &[],
        )?);
        children.push(create_element(
            &modules.react,
            &modules.hero_shell,
            Some(&object(&[("t", translate.clone().into())])?),
            &[],
        )?);
        children.push(workspace_row);
    }
    if !zone.is_undefined() {
        children.push(render_slot.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("conversation.input.dock"),
            zone,
        )?);
    }
    children.push(input_bar);
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[(
            "className",
            JsValue::from_str(&class_names(&[
                (class("composerStack"), true),
                (class("composerHero"), hero),
            ])),
        )])?),
        &children,
    )
}

fn find_workspace_for_session(items: &Array, session_id: &JsValue) -> Result<JsValue, JsValue> {
    for workspace in items.iter() {
        let ids = required(&workspace, "sessionIds", "workspace")?.dyn_into::<Array>()?;
        if ids.includes(session_id, 0) {
            return Ok(workspace);
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn find_workspace(items: &Array, workspace_id: &JsValue) -> Result<JsValue, JsValue> {
    for workspace in items.iter() {
        let id = required(&workspace, "workspaceId", "workspace")?;
        if Object::is(&id, workspace_id) {
            return Ok(workspace);
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn workspace_title(
    pending_workspace: &JsValue,
    session_id: &JsValue,
    session_workspace: &JsValue,
    workspaces_phase: &str,
    cwd: &JsValue,
) -> Result<JsValue, JsValue> {
    if !pending_workspace.is_undefined() {
        return required(pending_workspace, "title", "workspace");
    }
    if session_id.is_undefined() {
        return Ok(JsValue::UNDEFINED);
    }
    if !session_workspace.is_undefined() {
        return required(session_workspace, "title", "workspace");
    }
    let cwd = cwd.as_string().unwrap_or_default();
    if workspaces_phase == "ready" || cwd.is_empty() {
        Ok(JsValue::UNDEFINED)
    } else {
        Ok(JsValue::from_str(&workspace_label_browser(&cwd)))
    }
}

fn select_session_summary(
    props: &JsValue,
    session_id: &JsValue,
    field: &str,
) -> Result<JsValue, JsValue> {
    let session_id = session_id.clone();
    let field = field.to_owned();
    select_with(props, "useSessions", move |sessions| {
        if session_id.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let by_id = required(&sessions, "byId", "sessions snapshot")?;
        let row = Reflect::get(&by_id, &session_id)?;
        if row.is_null() || row.is_undefined() {
            Ok(JsValue::UNDEFINED)
        } else {
            Reflect::get(&row, &JsValue::from_str(&field))
        }
    })
}

fn select_identity(props: &JsValue, hook: &str) -> Result<JsValue, JsValue> {
    let selector =
        Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    required_function(props, hook, "ConversationRoot props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn select_field(props: &JsValue, hook: &str, field: &str) -> Result<JsValue, JsValue> {
    let field = field.to_owned();
    select_with(props, hook, move |value| {
        if value.is_null() || value.is_undefined() {
            Ok(JsValue::UNDEFINED)
        } else {
            Reflect::get(&value, &JsValue::from_str(&field))
        }
    })
}

fn select_with<F>(props: &JsValue, hook: &str, selector: F) -> Result<JsValue, JsValue>
where
    F: 'static + FnMut(JsValue) -> Result<JsValue, JsValue>,
{
    let selector =
        Closure::wrap(Box::new(selector) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    required_function(props, hook, "ConversationRoot props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let state = required_function(react, "useState", "React")?
        .call1(react, initial)?
        .dyn_into::<Array>()?;
    Ok((state.get(0), state.get(1).dyn_into()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

#[allow(clippy::needless_pass_by_value)] // Transfers one freshly-created Hook callback.
fn use_effect(react: &JsValue, setup: JsValue, deps: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?.call2(react, &setup, deps)?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)] // Transfers one freshly-created Hook callback.
fn use_callback(react: &JsValue, callback: JsValue, deps: &Array) -> Result<JsValue, JsValue> {
    required_function(react, "useCallback", "React")?.call2(react, &callback, deps)
}

fn nullish_to(value: JsValue, fallback: JsValue) -> JsValue {
    if value.is_null() || value.is_undefined() {
        fallback
    } else {
        value
    }
}

fn ref_current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_ref_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    args.push(kind);
    args.push(props.map_or(&JsValue::NULL, Object::as_ref));
    for child in children {
        args.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &args)
}

fn class(name: &str) -> &'static str {
    match name {
        "composerHero" => "seekdeep-conversation-session-composerHero",
        "composerSeat" => "seekdeep-conversation-session-composerSeat",
        "composerStack" => "seekdeep-conversation-session-composerStack",
        "heroGlow" => "seekdeep-conversation-session-heroGlow",
        "heroWorkspaceRow" => "seekdeep-conversation-session-heroWorkspaceRow",
        "root" => "seekdeep-conversation-session-root",
        "scrollBody" => "seekdeep-conversation-session-scrollBody",
        _ => "",
    }
}

fn class_names(values: &[(&str, bool)]) -> String {
    values
        .iter()
        .filter_map(|(value, enabled)| enabled.then_some(*value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn class_props(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
}

fn call_method(value: &JsValue, key: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required_function(value, key, "object")?;
    let args: Array = args.iter().collect();
    function.apply(value, &args)
}

fn numeric_required(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn number_string(value: f64) -> Result<String, JsValue> {
    js_sys::Number::from(value)
        .to_string_with_radix(10)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Number.toString() returned non-string").into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
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
