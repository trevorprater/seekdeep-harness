//! Compiled resident composer bar, DOM choreography, and React tree.

use std::{cell::RefCell, cmp::Ordering};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser_reasoning::inject_style;
use crate::{
    attachment_error_text_browser, attachment_rail_labels_browser,
    configure_client_ui_conversation_context_meter,
    configure_client_ui_conversation_permission_select, context_meter_component,
    derive_decorations_browser, drop_overlay_labels_browser, image_size_text_browser,
    input_text_len, lightbox_labels_browser, permission_select_component, slice_input_text,
    trim_input_text,
};

const INPUT_BAR_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/InputBar.module.css"
);

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    tooltip: JsValue,
    toast: JsValue,
    plus: JsValue,
    warning: JsValue,
    attachment_rail: JsValue,
    drop_overlay: JsValue,
    image_lightbox: JsValue,
    context_meter: JsValue,
    permission_select: JsValue,
}

#[allow(clippy::struct_excessive_bools)] // Closed projection of the source component's named gates.
struct BarFlags {
    empty: bool,
    locked: bool,
    disabled: bool,
    model_seat_locked: bool,
    machine_busy: bool,
    workspace_trigger: bool,
    textarea_disabled: bool,
    can_steer_queue: bool,
    running: bool,
    primary_stops: bool,
    interruptible: bool,
    parent_offline: bool,
}

/// Configures the compiled resident composer bar.
///
/// # Errors
///
/// Returns on missing React, primitive, attachment, child-component, or stylesheet faces.
#[wasm_bindgen(js_name = configureClientUiConversationInputBar)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_input_bar(
    react: JsValue,
    ui_primitives: JsValue,
    ui_attachment: JsValue,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useCallback",
        "useEffect",
        "useMemo",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    configure_client_ui_conversation_context_meter(react.clone(), ui_primitives.clone())?;
    configure_client_ui_conversation_permission_select(react.clone(), ui_primitives.clone())?;
    let modules = BrowserModules {
        tooltip: required(&ui_primitives, "Tooltip", "ui-primitives")?,
        toast: required(&ui_primitives, "Toast", "ui-primitives")?,
        plus: required(&ui_primitives, "IconPlusOutline16", "ui-primitives")?,
        warning: required(&ui_primitives, "IconWarningOutline16", "ui-primitives")?,
        attachment_rail: required(&ui_attachment, "AttachmentRail", "ui-attachment")?,
        drop_overlay: required(&ui_attachment, "DropOverlay", "ui-attachment")?,
        image_lightbox: required(&ui_attachment, "ImageLightbox", "ui-attachment")?,
        context_meter: context_meter_component()?,
        permission_select: permission_select_component()?,
        react,
    };
    inject_style(
        "InputBar",
        INPUT_BAR_CSS,
        &[
            ("accessory", class("accessory")),
            ("add", class("add")),
            ("attachments", class("attachments")),
            ("backdrop", class("backdrop")),
            ("card", class("card")),
            ("cardWorkspaceTrigger", class("cardWorkspaceTrigger")),
            ("chip", class("chip")),
            ("chipInvalid", class("chipInvalid")),
            ("chipLabel", class("chipLabel")),
            ("grow", class("grow")),
            ("hero", class("hero")),
            ("hint", class("hint")),
            ("hlSegment", class("hlSegment")),
            ("hlToken", class("hlToken")),
            ("input", class("input")),
            ("mirror", class("mirror")),
            ("modes", class("modes")),
            ("notice", class("notice")),
            ("noticeError", class("noticeError")),
            ("overlayAnchor", class("overlayAnchor")),
            ("pending", class("pending")),
            ("primary", class("primary")),
            ("retry", class("retry")),
            ("root", class("root")),
            ("row", class("row")),
            ("scroll", class("scroll")),
            ("select", class("select")),
            ("textRef", class("textRef")),
            ("tools", class("tools")),
            ("trailing", class("trailing")),
        ],
    )?;
    let component = Closure::wrap(
        Box::new(move |props: JsValue| render_input_bar(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `InputBar` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = inputBarComponent)]
pub fn input_bar_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation InputBar was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Hook order and the closed composer state machine stay auditable together.
fn render_input_bar(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let input = select_identity(props, "useInput", "InputBar props")?;
    let notice = select_identity(props, "useNotices", "InputBar props")?;
    let lexicon = select_identity(props, "useLexicon", "InputBar props")?;
    let command_menu_open = select_with(props, "useMenuLauncher", move |source| {
        Ok(JsValue::from_bool(
            source.as_string().as_deref() == Some("command"),
        ))
    })?
    .as_bool()
    .unwrap_or(false);
    let prompt_error = select_field(props, "useSession", "promptError")?;
    let running = select_field(props, "useSession", "running")?
        .as_bool()
        .unwrap_or(false);
    let subagent = nullish_to_null(select_field(props, "useSession", "subagent")?);
    let removed = select_field(props, "useSession", "removed")?
        .as_bool()
        .unwrap_or(false);
    let plan_active = select_projection_with(props, "plan", move |plan| {
        if plan.is_undefined() {
            return Ok(JsValue::FALSE);
        }
        let pending = Reflect::get(&plan, &JsValue::from_str("pending"))?
            .as_bool()
            .unwrap_or(false);
        let active = Reflect::get(&plan, &JsValue::from_str("active"))?
            .as_bool()
            .unwrap_or(false);
        Ok(JsValue::from_bool(if pending { !active } else { active }))
    })?
    .as_bool()
    .unwrap_or(false);
    let has_goal = select_projection_with(props, "goal", move |goal| {
        Ok(JsValue::from_bool(!goal.is_null() && !goal.is_undefined()))
    })?
    .as_bool()
    .unwrap_or(false);
    let keyboard = optional(props, "keyboard")?;
    let input_actions = optional(props, "inputActions")?;
    let live = !input.is_undefined() && keyboard.is_some() && input_actions.is_some();
    let draft = if input.is_undefined() {
        String::new()
    } else {
        required_string(&input, "draft", "InputState")?
    };
    let draft_images = optional(props, "draftImages")?;
    let attachments = use_attachments(&modules.react, &input, draft_images.as_ref())?;
    let empty = trim_input_text(&draft).is_empty() && attachments.length() == 0;
    let (preview, set_preview) = use_state(&modules.react, &JsValue::NULL)?;
    let (drag_active_value, set_drag_active) = use_state(&modules.react, &JsValue::FALSE)?;
    let drag_active = drag_active_value.as_bool().unwrap_or(false);
    let (toast, set_toast) = use_state(&modules.react, &JsValue::NULL)?;
    let toast_seq = use_ref(&modules.react, &JsValue::from_f64(0.0))?;
    let show_toast = show_toast_callback(&modules.react, &toast_seq, &set_toast)?;
    let dismiss_toast = setter_callback(&modules.react, &set_toast, &JsValue::NULL)?;
    let image_limits = select_projection(props, "imageLimits")?;
    install_prompt_error_effect(
        &modules.react,
        props,
        &prompt_error,
        &image_limits,
        &show_toast,
    )?;
    let input_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let card_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let drag_depth = use_ref(&modules.react, &JsValue::from_f64(0.0))?;
    let scroll_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let mirror_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let composing_ref = use_ref(&modules.react, &JsValue::FALSE)?;
    let permissions = select_projection(props, "permissions")?;

    let continuable = subagent_mode(&subagent)?.as_deref() == Some("continuable");
    let parent_offline = continuable
        && !Reflect::get(&subagent, &JsValue::from_str("parentAvailable"))?
            .as_bool()
            .unwrap_or(false);
    let inert = optional_bool(props, "disabled", false)?;
    let blocked = optional(props, "blocked")?;
    let locked = removed || inert || !live || blocked.is_some() || parent_offline;
    let model_seat_locked = removed || inert || !live;
    let phase = if input.is_undefined() {
        "inert".to_owned()
    } else {
        required_string(&input, "phase", "InputState")?
    };
    let machine_busy = matches!(phase.as_str(), "adjudicating" | "submitting");
    let request_workspace = optional(props, "onRequestWorkspace")?;
    let workspace_trigger = inert && !removed && request_workspace.is_some();
    let textarea_disabled = removed || (locked && !workspace_trigger);
    let subagent_is_null = subagent.is_null();
    let can_steer_queue = !locked
        && !machine_busy
        && !command_menu_open
        && empty
        && running
        && subagent_is_null
        && queue_has_queued(&input)?;
    let primary_stops = running && subagent_is_null;
    let interruptible = running && continuable;
    let flags = BarFlags {
        empty,
        locked,
        disabled: locked,
        model_seat_locked,
        machine_busy,
        workspace_trigger,
        textarea_disabled,
        can_steer_queue,
        running,
        primary_stops,
        interruptible,
        parent_offline,
    };

    install_prune_effect(&modules.react, &input, input_actions.as_ref(), &attachments)?;
    install_preview_effect(&modules.react, &preview, &attachments, &set_preview)?;
    let focus_session_id = optional_value(props, "sessionId")?;
    install_focus_effect(
        &modules.react,
        &input_ref,
        &scroll_ref,
        &mirror_ref,
        flags.locked,
        &focus_session_id,
    )?;
    install_restored_draft_effect(
        &modules.react,
        &input_ref,
        &scroll_ref,
        &mirror_ref,
        flags.locked,
        &draft,
    )?;
    install_wheel_effect(&modules.react, &scroll_ref)?;

    let intake_images = intake_images_callback(
        &modules.react,
        props,
        &attachments,
        &image_limits,
        &show_toast,
    )?;
    let can_accept_drop =
        !flags.locked && !flags.machine_busy && optional(props, "addImages")?.is_some();
    install_drop_effect(
        &modules.react,
        can_accept_drop,
        &drag_depth,
        &set_drag_active,
        &intake_images,
    )?;
    let close_preview = setter_callback(&modules.react, &set_preview, &JsValue::NULL)?;
    let translate = required_function(props, "t", "InputBar props")?;
    let rail_items = use_rail_items(&modules.react, &attachments, &translate)?;

    let restore = RestoreCaret {
        scroll_ref: scroll_ref.clone(),
        mirror_ref: mirror_ref.clone(),
    };
    let handlers = input_handlers(
        props,
        &input,
        keyboard.as_ref(),
        input_actions.as_ref(),
        &draft,
        &composing_ref,
        &intake_images,
        &restore,
        &flags,
        subagent_is_null,
    )?;
    let keep_focus = keep_focus_callback(&input_ref);
    let toggle_command = toggle_command_callback(props, &input_ref)?;
    let primary = primary_callback(
        props,
        input_actions.as_ref(),
        flags.primary_stops,
        flags.empty,
        flags.disabled,
        flags.machine_busy,
    )?;
    let primary_label = translate_key(
        &translate,
        if flags.primary_stops {
            "input.stop"
        } else {
            "input.send"
        },
    )?;
    let decorations = if input.is_undefined() {
        inert_decorations()?
    } else {
        derive_decorations_browser(input.clone(), lexicon.clone())?
    };
    let backdrop = render_backdrop(modules, &draft, &input, &decorations, has_goal, &translate)?;
    let tree = render_tree(
        modules,
        props,
        &notice,
        &permissions,
        &preview,
        &toast,
        &image_limits,
        &rail_items,
        &backdrop,
        &draft,
        &phase,
        plan_active,
        command_menu_open,
        drag_active,
        can_accept_drop,
        &flags,
        &input_ref,
        &card_ref,
        &scroll_ref,
        &mirror_ref,
        &set_preview,
        &close_preview,
        &dismiss_toast,
        &keep_focus,
        &toggle_command,
        &primary,
        &primary_label,
        request_workspace.as_ref(),
        &handlers,
        &translate,
    )?;
    Ok(tree)
}

#[derive(Clone)]
struct RestoreCaret {
    scroll_ref: JsValue,
    mirror_ref: JsValue,
}

struct InputHandlers {
    key_down: JsValue,
    change: JsValue,
    select: JsValue,
    copy: JsValue,
    cut: JsValue,
    paste: JsValue,
    composition_start: JsValue,
    composition_end: JsValue,
}

#[derive(Clone)]
enum Boundary {
    Chip {
        at: u32,
        occurrence_id: f64,
        label: String,
        invalid: bool,
    },
    TextRef {
        at: u32,
        end: u32,
    },
}

impl Boundary {
    const fn at(&self) -> u32 {
        match self {
            Self::Chip { at, .. } | Self::TextRef { at, .. } => *at,
        }
    }
}

fn class(name: &'static str) -> &'static str {
    match name {
        "accessory" => "seekdeep-conversation-inputBar-accessory",
        "add" => "seekdeep-conversation-inputBar-add",
        "attachments" => "seekdeep-conversation-inputBar-attachments",
        "backdrop" => "seekdeep-conversation-inputBar-backdrop",
        "card" => "seekdeep-conversation-inputBar-card",
        "cardWorkspaceTrigger" => "seekdeep-conversation-inputBar-cardWorkspaceTrigger",
        "chip" => "seekdeep-conversation-inputBar-chip",
        "chipInvalid" => "seekdeep-conversation-inputBar-chipInvalid",
        "chipLabel" => "seekdeep-conversation-inputBar-chipLabel",
        "grow" => "seekdeep-conversation-inputBar-grow",
        "hero" => "seekdeep-conversation-inputBar-hero",
        "hint" => "seekdeep-conversation-inputBar-hint",
        "hlSegment" => "seekdeep-conversation-inputBar-hlSegment",
        "hlToken" => "seekdeep-conversation-inputBar-hlToken",
        "input" => "seekdeep-conversation-inputBar-input",
        "mirror" => "seekdeep-conversation-inputBar-mirror",
        "modes" => "seekdeep-conversation-inputBar-modes",
        "notice" => "seekdeep-conversation-inputBar-notice",
        "noticeError" => "seekdeep-conversation-inputBar-noticeError",
        "overlayAnchor" => "seekdeep-conversation-inputBar-overlayAnchor",
        "pending" => "seekdeep-conversation-inputBar-pending",
        "primary" => "seekdeep-conversation-inputBar-primary",
        "retry" => "seekdeep-conversation-inputBar-retry",
        "root" => "seekdeep-conversation-inputBar-root",
        "row" => "seekdeep-conversation-inputBar-row",
        "scroll" => "seekdeep-conversation-inputBar-scroll",
        "select" => "seekdeep-conversation-inputBar-select",
        "textRef" => "seekdeep-conversation-inputBar-textRef",
        "tools" => "seekdeep-conversation-inputBar-tools",
        "trailing" => "seekdeep-conversation-inputBar-trailing",
        _ => "",
    }
}

fn select_identity(props: &JsValue, hook: &str, owner: &str) -> Result<JsValue, JsValue> {
    let selector =
        Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    required_function(props, hook, owner)?.call1(&JsValue::UNDEFINED, &selector.into_js_value())
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
    required_function(props, hook, "InputBar props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn select_projection(props: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    required_function(props, "useProjection", "InputBar props")?
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn select_projection_with<F>(props: &JsValue, key: &str, selector: F) -> Result<JsValue, JsValue>
where
    F: 'static + FnMut(JsValue) -> Result<JsValue, JsValue>,
{
    let selector =
        Closure::wrap(Box::new(selector) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    required_function(props, "useProjection", "InputBar props")?.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(key),
        &selector.into_js_value(),
    )
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

#[allow(clippy::needless_pass_by_value)] // Callers transfer one freshly-created Hook callback.
fn use_effect(react: &JsValue, setup: JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?.call2(react, &setup, dependencies)?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)] // Callers transfer one freshly-created Hook callback.
fn use_callback(
    react: &JsValue,
    callback: JsValue,
    dependencies: &Array,
) -> Result<JsValue, JsValue> {
    required_function(react, "useCallback", "React")?.call2(react, &callback, dependencies)
}

#[allow(clippy::needless_pass_by_value)] // Callers transfer one freshly-created Hook factory.
fn use_memo(react: &JsValue, factory: JsValue, dependencies: &Array) -> Result<JsValue, JsValue> {
    required_function(react, "useMemo", "React")?.call2(react, &factory, dependencies)
}

fn use_attachments(
    react: &JsValue,
    input: &JsValue,
    draft_images: Option<&JsValue>,
) -> Result<Array, JsValue> {
    let ids = if input.is_undefined() {
        JsValue::UNDEFINED
    } else {
        required(input, "imageIds", "InputState")?
    };
    let face = draft_images.cloned().unwrap_or(JsValue::UNDEFINED);
    let factory_face = face.clone();
    let factory_ids = ids.clone();
    let factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if factory_face.is_undefined() || factory_ids.is_undefined() {
            return Ok(Array::new().into());
        }
        factory_face
            .clone()
            .dyn_into::<Function>()?
            .call1(&JsValue::UNDEFINED, &factory_ids)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_memo(react, factory.into_js_value(), &Array::of2(&face, &ids))?.dyn_into()
}

fn show_toast_callback(
    react: &JsValue,
    sequence: &JsValue,
    setter: &Function,
) -> Result<JsValue, JsValue> {
    let sequence = sequence.clone();
    let setter = setter.clone();
    let callback = Closure::wrap(Box::new(move |text: String| -> Result<(), JsValue> {
        let current = Reflect::get(&sequence, &JsValue::from_str("current"))?
            .as_f64()
            .unwrap_or(0.0)
            + 1.0;
        Reflect::set(
            &sequence,
            &JsValue::from_str("current"),
            &JsValue::from_f64(current),
        )?;
        setter.call1(
            &JsValue::UNDEFINED,
            object(&[
                ("seq", JsValue::from_f64(current)),
                ("text", JsValue::from_str(&text)),
            ])?
            .as_ref(),
        )?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    use_callback(react, callback.into_js_value(), &Array::new())
}

fn setter_callback(
    react: &JsValue,
    setter: &Function,
    value: &JsValue,
) -> Result<JsValue, JsValue> {
    let setter = setter.clone();
    let value = value.clone();
    let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        setter.call1(&JsValue::UNDEFINED, &value)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_callback(react, callback.into_js_value(), &Array::new())
}

fn install_prompt_error_effect(
    react: &JsValue,
    props: &JsValue,
    prompt_error: &JsValue,
    image_limits: &JsValue,
    show_toast: &JsValue,
) -> Result<(), JsValue> {
    let prompt_error = prompt_error.clone();
    let image_limits = image_limits.clone();
    let translate = required_function(props, "t", "InputBar props")?;
    let show_toast = show_toast.clone().dyn_into::<Function>()?;
    let dependency_prompt = prompt_error.clone();
    let dependency_limits = image_limits.clone();
    let dependency_translate = translate.clone();
    let dependency_toast = show_toast.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if prompt_error.is_null() || prompt_error.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let error = required(&prompt_error, "error", "prompt error")?;
        let code = required_string(&error, "code", "prompt error")?;
        let text = if code == "attachment-error" {
            let details = required(&error, "details", "prompt error")?;
            attachment_error_text_browser(
                translate.clone(),
                required_string(&details, "reason", "attachment error details")?,
                image_limits.clone(),
            )?
        } else {
            let message = required_string(&error, "message", "prompt error")?;
            JsValue::from_str(&format!("{message} ({code})"))
        };
        show_toast.call1(&JsValue::UNDEFINED, &text)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        setup.into_js_value(),
        &Array::of4(
            &dependency_prompt,
            dependency_toast.as_ref(),
            dependency_translate.as_ref(),
            &dependency_limits,
        ),
    )
}

fn install_prune_effect(
    react: &JsValue,
    input: &JsValue,
    input_actions: Option<&JsValue>,
    attachments: &Array,
) -> Result<(), JsValue> {
    let input = input.clone();
    let actions = input_actions.cloned().unwrap_or(JsValue::UNDEFINED);
    let attachments = attachments.clone();
    let dependency_input = input.clone();
    let dependency_actions = actions.clone();
    let dependency_attachments = attachments.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if input.is_undefined() || actions.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let ids = required(&input, "imageIds", "InputState")?.dyn_into::<Array>()?;
        if attachments.length() != ids.length() {
            let live_ids = Array::new();
            for attachment in attachments.iter() {
                live_ids.push(&required(&attachment, "id", "ComposerAttachment")?);
            }
            call_method(&actions, "pruneImages", &[live_ids.into()])?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let image_ids = if dependency_input.is_undefined() {
        JsValue::UNDEFINED
    } else {
        required(&dependency_input, "imageIds", "InputState")?
    };
    use_effect(
        react,
        setup.into_js_value(),
        &Array::of3(
            dependency_attachments.as_ref(),
            &image_ids,
            &dependency_actions,
        ),
    )
}

fn install_preview_effect(
    react: &JsValue,
    preview: &JsValue,
    attachments: &Array,
    set_preview: &Function,
) -> Result<(), JsValue> {
    let preview = preview.clone();
    let attachments = attachments.clone();
    let setter = set_preview.clone();
    let dependency_preview = preview.clone();
    let dependency_attachments = attachments.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if preview.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let id = required(&preview, "id", "ComposerAttachment")?;
        let present = attachments.iter().any(|attachment| {
            Reflect::get(&attachment, &JsValue::from_str("id"))
                .is_ok_and(|candidate| Object::is(&candidate, &id))
        });
        if !present {
            setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        setup.into_js_value(),
        &Array::of2(dependency_attachments.as_ref(), &dependency_preview),
    )
}

fn nullish_to_null(value: JsValue) -> JsValue {
    if value.is_null() || value.is_undefined() {
        JsValue::NULL
    } else {
        value
    }
}

fn subagent_mode(subagent: &JsValue) -> Result<Option<String>, JsValue> {
    if subagent.is_null() || subagent.is_undefined() {
        return Ok(None);
    }
    let address = required(subagent, "address", "subagent projection")?;
    Ok(Reflect::get(&address, &JsValue::from_str("mode"))?.as_string())
}

fn queue_has_queued(input: &JsValue) -> Result<bool, JsValue> {
    if input.is_undefined() {
        return Ok(false);
    }
    let queue = required(input, "queue", "InputState")?.dyn_into::<Array>()?;
    Ok(queue.iter().any(|row| {
        Reflect::get(&row, &JsValue::from_str("placement"))
            .is_ok_and(|placement| placement.as_string().as_deref() == Some("queued"))
    }))
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!property.is_null() && !property.is_undefined()).then_some(property))
}

fn optional_value(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))
}

fn optional_bool(value: &JsValue, key: &str, default: bool) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?
        .as_bool()
        .unwrap_or(default))
}

fn install_focus_effect(
    react: &JsValue,
    input_ref: &JsValue,
    scroll_ref: &JsValue,
    mirror_ref: &JsValue,
    locked: bool,
    session_id: &JsValue,
) -> Result<(), JsValue> {
    let input_ref = input_ref.clone();
    let scroll_ref = scroll_ref.clone();
    let mirror_ref = mirror_ref.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let input = ref_current(&input_ref)?;
        if locked || input.is_null() || input.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        call_method(
            &input,
            "focus",
            &[object(&[("preventScroll", JsValue::TRUE)])?.into()],
        )?;
        reveal_selection_focus(&input, &scroll_ref, &mirror_ref)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        setup.into_js_value(),
        &Array::of2(&JsValue::from_bool(locked), session_id),
    )
}

fn install_restored_draft_effect(
    react: &JsValue,
    input_ref: &JsValue,
    scroll_ref: &JsValue,
    mirror_ref: &JsValue,
    locked: bool,
    draft: &str,
) -> Result<(), JsValue> {
    let non_empty = !draft.is_empty();
    let input_ref = input_ref.clone();
    let scroll_ref = scroll_ref.clone();
    let mirror_ref = mirror_ref.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let input = ref_current(&input_ref)?;
        if locked || !non_empty || input.is_null() || input.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        reveal_selection_focus(&input, &scroll_ref, &mirror_ref)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        setup.into_js_value(),
        &Array::of1(&JsValue::from_bool(!draft.is_empty())),
    )
}

fn install_wheel_effect(react: &JsValue, scroll_ref: &JsValue) -> Result<(), JsValue> {
    let scroll_ref = scroll_ref.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let element = ref_current(&scroll_ref)?;
        if element.is_null() || element.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let target = element.clone();
        let on_wheel = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let host = call_method(
                &target,
                "closest",
                &[JsValue::from_str("[data-conversation-scroll]")],
            )?;
            let delta = numeric(&event, "deltaY")?.unwrap_or(0.0);
            if host.is_null() || host.is_undefined() || delta == 0.0 {
                return Ok(());
            }
            let scroll_top = numeric_required(&target, "scrollTop", "input scrollport")?;
            let client_height = numeric_required(&target, "clientHeight", "input scrollport")?;
            let scroll_height = numeric_required(&target, "scrollHeight", "input scrollport")?;
            let at_top = scroll_top <= 0.0;
            let at_end = scroll_top + client_height >= scroll_height - 1.0;
            if (delta < 0.0 && !at_top) || (delta > 0.0 && !at_end) {
                return Ok(());
            }
            call_method(&event, "preventDefault", &[])?;
            let host_top = numeric_required(&host, "scrollTop", "conversation scrollport")?;
            Reflect::set(
                &host,
                &JsValue::from_str("scrollTop"),
                &JsValue::from_f64(host_top + delta),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        call_method(
            &element,
            "addEventListener",
            &[
                JsValue::from_str("wheel"),
                on_wheel.clone(),
                object(&[("passive", JsValue::FALSE)])?.into(),
            ],
        )?;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &element,
                "removeEventListener",
                &[JsValue::from_str("wheel"), on_wheel.clone()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(react, setup.into_js_value(), &Array::new())
}

fn reveal_selection_focus(
    input: &JsValue,
    scroll_ref: &JsValue,
    mirror_ref: &JsValue,
) -> Result<(), JsValue> {
    let backward = Reflect::get(input, &JsValue::from_str("selectionDirection"))?
        .as_string()
        .as_deref()
        == Some("backward");
    let key = if backward {
        "selectionStart"
    } else {
        "selectionEnd"
    };
    let caret = numeric(input, key)?.unwrap_or_else(|| {
        Reflect::get(input, &JsValue::from_str("value"))
            .ok()
            .and_then(|value| value.as_string())
            .map_or(0.0, |value| f64::from(input_text_len(&value)))
    });
    reveal_caret(scroll_ref, mirror_ref, caret)
}

#[allow(clippy::too_many_lines)] // Mirrors the source's cross-engine newline caret normalization.
fn reveal_caret(scroll_ref: &JsValue, mirror_ref: &JsValue, caret: f64) -> Result<(), JsValue> {
    let scroll = ref_current(scroll_ref)?;
    let mirror = ref_current(mirror_ref)?;
    if scroll.is_null() || scroll.is_undefined() || mirror.is_null() || mirror.is_undefined() {
        return Ok(());
    }
    let text = Reflect::get(&mirror, &JsValue::from_str("firstChild"))?;
    if text.is_null() || text.is_undefined() {
        return Ok(());
    }
    let data = Reflect::get(&text, &JsValue::from_str("data"))?
        .as_string()
        .unwrap_or_default();
    let scroll_height = numeric_required(&scroll, "scrollHeight", "input scrollport")?;
    let client_height = numeric_required(&scroll, "clientHeight", "input scrollport")?;
    if scroll_height <= client_height {
        return Ok(());
    }
    let at = caret.max(0.0).min(f64::from(input_text_len(&data)));
    let at_u32 = number_to_u32(at.trunc())?;
    let after_newline =
        at_u32 > 0 && slice_input_text(&data, at_u32.saturating_sub(1), at_u32) == "\n";
    let document = required(&js_sys::global(), "document", "global")?;
    let range = call_method(&document, "createRange", &[])?;
    let start = if after_newline {
        at_u32.saturating_sub(1)
    } else {
        at_u32
    };
    call_method(
        &range,
        "setStart",
        &[text.clone(), JsValue::from_f64(f64::from(start))],
    )?;
    if after_newline {
        call_method(
            &range,
            "setEnd",
            &[text, JsValue::from_f64(f64::from(at_u32))],
        )?;
    } else {
        call_method(&range, "collapse", &[JsValue::TRUE])?;
    }
    let line = if after_newline {
        let style = global_function("getComputedStyle")?.call1(&JsValue::UNDEFINED, &mirror)?;
        global_function("parseFloat")?
            .call1(
                &JsValue::UNDEFINED,
                &Reflect::get(&style, &JsValue::from_str("lineHeight"))?,
            )?
            .as_f64()
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let rect = call_method(&range, "getBoundingClientRect", &[])?;
    let bounds = call_method(&scroll, "getBoundingClientRect", &[])?;
    let rect_bottom = numeric_required(&rect, "bottom", "caret rect")?;
    let rect_top = numeric_required(&rect, "top", "caret rect")?;
    let box_bottom = numeric_required(&bounds, "bottom", "scroll rect")?;
    let box_top = numeric_required(&bounds, "top", "scroll rect")?;
    let scroll_top = numeric_required(&scroll, "scrollTop", "input scrollport")?;
    let next = if rect_bottom + line > box_bottom {
        Some(scroll_top + rect_bottom + line - box_bottom)
    } else if rect_top + line < box_top {
        Some(scroll_top - (box_top - rect_top - line))
    } else {
        None
    };
    if let Some(next) = next {
        Reflect::set(
            &scroll,
            &JsValue::from_str("scrollTop"),
            &JsValue::from_f64(next),
        )?;
    }
    Ok(())
}

fn restore_caret(restore: &RestoreCaret, input: &JsValue, caret: u32) -> Result<(), JsValue> {
    let input = input.clone();
    let scroll_ref = restore.scroll_ref.clone();
    let mirror_ref = restore.mirror_ref.clone();
    let callback = Closure::once_into_js(move || -> Result<(), JsValue> {
        call_method(
            &input,
            "setSelectionRange",
            &[
                JsValue::from_f64(f64::from(caret)),
                JsValue::from_f64(f64::from(caret)),
            ],
        )?;
        reveal_caret(&scroll_ref, &mirror_ref, f64::from(caret))
    });
    global_function("requestAnimationFrame")?.call1(&JsValue::UNDEFINED, &callback)?;
    Ok(())
}

fn ref_current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn intake_images_callback(
    react: &JsValue,
    props: &JsValue,
    attachments: &Array,
    image_limits: &JsValue,
    show_toast: &JsValue,
) -> Result<JsValue, JsValue> {
    let add_images = optional(props, "addImages")?.unwrap_or(JsValue::UNDEFINED);
    let translate = required_function(props, "t", "InputBar props")?;
    let attachments = attachments.clone();
    let dependency_attachments = attachments.clone();
    let limits = image_limits.clone();
    let show_toast = show_toast.clone().dyn_into::<Function>()?;
    let dependency_show_toast = show_toast.clone();
    let add_for_callback = add_images.clone();
    let translate_for_callback = translate.clone();
    let callback = Closure::wrap(Box::new(move |files: JsValue| -> Result<(), JsValue> {
        let files = files.dyn_into::<Array>()?;
        if add_for_callback.is_undefined() || files.length() == 0 {
            return Ok(());
        }
        let add = add_for_callback.clone().dyn_into::<Function>()?;
        let rejected = if limits.is_undefined() {
            add.call1(&JsValue::UNDEFINED, files.as_ref())?
        } else {
            let media_types =
                required(&limits, "mediaTypes", "image limits")?.dyn_into::<Array>()?;
            let bad_format = files.iter().any(|file| {
                let media_type =
                    Reflect::get(&file, &JsValue::from_str("type")).unwrap_or(JsValue::UNDEFINED);
                !media_types.includes(&media_type, 0)
            });
            if bad_format {
                add.call1(&JsValue::UNDEFINED, files.as_ref())?
            } else {
                let max_count = numeric_required(&limits, "maxImagesPerMessage", "image limits")?;
                if f64::from(attachments.length() + files.length()) > max_count {
                    translate_values(
                        &translate_for_callback,
                        "image.tooMany",
                        &[("count", JsValue::from_f64(max_count))],
                    )?
                } else {
                    let max_file = numeric_required(&limits, "maxImageBytes", "image limits")?;
                    if files.iter().any(|file| {
                        numeric_required(&file, "size", "image file")
                            .is_ok_and(|size| size > max_file)
                    }) {
                        translate_values(
                            &translate_for_callback,
                            "image.fileTooLarge",
                            &[(
                                "size",
                                JsValue::from_str(&image_size_text_browser(max_file)?),
                            )],
                        )?
                    } else {
                        let mut total = 0.0;
                        for attachment in attachments.iter() {
                            total += numeric_required(
                                &required(&attachment, "file", "ComposerAttachment")?,
                                "size",
                                "attachment file",
                            )?;
                        }
                        for file in files.iter() {
                            total += numeric_required(&file, "size", "image file")?;
                        }
                        let max_total =
                            numeric_required(&limits, "maxMessageImageBytes", "image limits")?;
                        if total > max_total {
                            translate_values(
                                &translate_for_callback,
                                "image.totalTooLarge",
                                &[(
                                    "size",
                                    JsValue::from_str(&image_size_text_browser(max_total)?),
                                )],
                            )?
                        } else {
                            add.call1(&JsValue::UNDEFINED, files.as_ref())?
                        }
                    }
                }
            }
        };
        if !rejected.is_null() {
            let text = rejected.as_string().ok_or_else(|| {
                js_sys::TypeError::new("image intake rejection must be a string or null")
            })?;
            show_toast.call1(&JsValue::UNDEFINED, &JsValue::from_str(&text))?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    use_callback(
        react,
        callback.into_js_value(),
        &Array::of5(
            &add_images,
            dependency_attachments.as_ref(),
            image_limits,
            dependency_show_toast.as_ref(),
            translate.as_ref(),
        ),
    )
}

#[allow(clippy::too_many_lines)] // One document-listener lifetime owns the balanced drag-depth protocol.
fn install_drop_effect(
    react: &JsValue,
    can_accept: bool,
    drag_depth: &JsValue,
    set_drag_active: &Function,
    intake_images: &JsValue,
) -> Result<(), JsValue> {
    let drag_depth = drag_depth.clone();
    let set_drag_active = set_drag_active.clone();
    let intake_images = intake_images.clone().dyn_into::<Function>()?;
    let dependency_intake = intake_images.clone();
    let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let global = js_sys::global();
        let document = required(&global, "document", "global")?;
        let window = required(&global, "window", "global")?;
        let reset_depth = drag_depth.clone();
        let reset_setter = set_drag_active.clone();
        let reset = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            set_ref_current(&reset_depth, &JsValue::from_f64(0.0))?;
            reset_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();

        let enter_depth = drag_depth.clone();
        let enter_setter = set_drag_active.clone();
        let drag_enter = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if !drag_has_files(&event)? {
                return Ok(());
            }
            call_method(&event, "preventDefault", &[])?;
            let depth = ref_current(&enter_depth)?.as_f64().unwrap_or(0.0) + 1.0;
            set_ref_current(&enter_depth, &JsValue::from_f64(depth))?;
            enter_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            Ok(())
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();

        let drag_over = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if !drag_has_files(&event)? {
                return Ok(());
            }
            let transfer = Reflect::get(&event, &JsValue::from_str("dataTransfer"))?;
            if transfer.is_null() {
                return Ok(());
            }
            call_method(&event, "preventDefault", &[])?;
            Reflect::set(
                &transfer,
                &JsValue::from_str("dropEffect"),
                &JsValue::from_str(if can_accept { "copy" } else { "none" }),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();

        let leave_depth = drag_depth.clone();
        let leave_setter = set_drag_active.clone();
        let leave_reset = reset.clone().dyn_into::<Function>()?;
        let leave_document = document.clone();
        let leave_window = window.clone();
        let drag_leave = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if !drag_has_files(&event)? {
                return Ok(());
            }
            let depth = (ref_current(&leave_depth)?.as_f64().unwrap_or(0.0) - 1.0).max(0.0);
            set_ref_current(&leave_depth, &JsValue::from_f64(depth))?;
            if depth == 0.0 {
                leave_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            }
            let x = numeric(&event, "clientX")?.unwrap_or(0.0);
            let y = numeric(&event, "clientY")?.unwrap_or(0.0);
            let width = numeric_required(&leave_window, "innerWidth", "window")?;
            let height = numeric_required(&leave_window, "innerHeight", "window")?;
            let leaving = x <= 0.0 || y <= 0.0 || x >= width || y >= height;
            let target = Reflect::get(&event, &JsValue::from_str("target"))?;
            let html = Reflect::get(&leave_document, &JsValue::from_str("documentElement"))?;
            let body = Reflect::get(&leave_document, &JsValue::from_str("body"))?;
            if leaving && (Object::is(&target, &html) || Object::is(&target, &body)) {
                leave_reset.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();

        let drop_reset = reset.clone().dyn_into::<Function>()?;
        let drop_intake = intake_images.clone();
        let drop = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if !drag_has_files(&event)? {
                return Ok(());
            }
            call_method(&event, "preventDefault", &[])?;
            drop_reset.call0(&JsValue::UNDEFINED)?;
            if !can_accept {
                return Ok(());
            }
            let transfer = Reflect::get(&event, &JsValue::from_str("dataTransfer"))?;
            let files = if transfer.is_null() || transfer.is_undefined() {
                Array::new()
            } else {
                Array::from(&Reflect::get(&transfer, &JsValue::from_str("files"))?)
            };
            drop_intake.call1(&JsValue::UNDEFINED, files.as_ref())?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();

        for (event, handler) in [
            ("dragenter", &drag_enter),
            ("dragover", &drag_over),
            ("dragleave", &drag_leave),
            ("drop", &drop),
        ] {
            call_method(
                &document,
                "addEventListener",
                &[JsValue::from_str(event), handler.clone()],
            )?;
        }
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("dragend"), reset.clone()],
        )?;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            for (event, handler) in [
                ("dragenter", &drag_enter),
                ("dragover", &drag_over),
                ("dragleave", &drag_leave),
                ("drop", &drop),
            ] {
                call_method(
                    &document,
                    "removeEventListener",
                    &[JsValue::from_str(event), handler.clone()],
                )?;
            }
            call_method(
                &window,
                "removeEventListener",
                &[JsValue::from_str("dragend"), reset.clone()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        setup.into_js_value(),
        &Array::of2(&JsValue::from_bool(can_accept), dependency_intake.as_ref()),
    )
}

fn drag_has_files(event: &JsValue) -> Result<bool, JsValue> {
    let transfer = Reflect::get(event, &JsValue::from_str("dataTransfer"))?;
    if transfer.is_null() || transfer.is_undefined() {
        return Ok(false);
    }
    let types = Reflect::get(&transfer, &JsValue::from_str("types"))?;
    Ok(call_method(&types, "includes", &[JsValue::from_str("Files")])?.as_bool() == Some(true))
}

fn use_rail_items(
    react: &JsValue,
    attachments: &Array,
    translate: &Function,
) -> Result<Array, JsValue> {
    let factory_attachments = attachments.clone();
    let factory_translate = translate.clone();
    let factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let items = Array::new();
        for attachment in factory_attachments.iter() {
            let file = required(&attachment, "file", "ComposerAttachment")?;
            let name = Reflect::get(&file, &JsValue::from_str("name"))?
                .as_string()
                .unwrap_or_default();
            let alt = if name.is_empty() {
                translate_key(&factory_translate, "image.pending")?
            } else {
                JsValue::from_str(&name)
            };
            let remove = translate_values(
                &factory_translate,
                "image.remove",
                &[("name", JsValue::from_str(&name))],
            )?;
            items.push(
                object(&[
                    ("id", required(&attachment, "id", "ComposerAttachment")?),
                    (
                        "previewUrl",
                        required(&attachment, "previewUrl", "ComposerAttachment")?,
                    ),
                    ("alt", alt),
                    ("removeLabel", remove),
                    ("attachment", attachment),
                ])?
                .as_ref(),
            );
        }
        Ok(items.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_memo(
        react,
        factory.into_js_value(),
        &Array::of2(attachments.as_ref(), translate.as_ref()),
    )?
    .dyn_into()
}

fn set_ref_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // Closed keyboard/clipboard event plane.
fn input_handlers(
    props: &JsValue,
    input: &JsValue,
    keyboard: Option<&JsValue>,
    input_actions: Option<&JsValue>,
    draft: &str,
    composing_ref: &JsValue,
    intake_images: &JsValue,
    restore: &RestoreCaret,
    flags: &BarFlags,
    subagent_is_null: bool,
) -> Result<InputHandlers, JsValue> {
    let workspace_trigger = flags.workspace_trigger;
    let locked = flags.locked;
    let machine_busy = flags.machine_busy;
    let can_steer_queue = flags.can_steer_queue;
    let running = flags.running;
    let keyboard_value = keyboard.cloned().unwrap_or(JsValue::UNDEFINED);
    let actions_value = input_actions.cloned().unwrap_or(JsValue::UNDEFINED);
    let workspace = optional(props, "onRequestWorkspace")?.unwrap_or(JsValue::UNDEFINED);
    let resolve_mode = required_function(props, "resolveSubmitMode", "InputBar props")?;
    let composing = composing_ref.clone();
    let key_keyboard = keyboard_value.clone();
    let key_actions = actions_value.clone();
    let key_workspace = workspace.clone();
    let key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required_string(&event, "key", "keyboard event")?;
        if workspace_trigger {
            if matches!(key.as_str(), "Enter" | " ") {
                prevent_default(&event)?;
                if !key_workspace.is_undefined() {
                    key_workspace
                        .clone()
                        .dyn_into::<Function>()?
                        .call0(&JsValue::UNDEFINED)?;
                }
            }
            return Ok(());
        }
        if key_keyboard.is_undefined() || key_actions.is_undefined() {
            return Ok(());
        }
        let shift = event_bool(&event, "shiftKey");
        if key == "Enter" && shift {
            return Ok(());
        }
        let native = Reflect::get(&event, &JsValue::from_str("nativeEvent"))?;
        let composition = ref_current(&composing)?.as_bool().unwrap_or(false)
            || event_bool(&native, "isComposing")
            || numeric(&native, "keyCode")? == Some(229.0);
        if matches!(key.as_str(), "ArrowUp" | "ArrowDown") {
            let outcome = call_method(
                &key_keyboard,
                "arbitrate",
                &[
                    JsValue::from_str(if key == "ArrowUp" { "up" } else { "down" }),
                    JsValue::from_bool(composition),
                ],
            )?;
            if outcome.as_string().as_deref() == Some("consumed") {
                prevent_default(&event)?;
            }
            return Ok(());
        }
        if key == "Escape" {
            call_method(&key_keyboard, "dismissPopup", &[])?;
            let outcome = call_method(
                &key_keyboard,
                "arbitrate",
                &[JsValue::from_str("escape"), JsValue::from_bool(composition)],
            )?;
            if outcome.as_string().as_deref() == Some("consumed") {
                prevent_default(&event)?;
            }
            return Ok(());
        }
        let meta = event_bool(&event, "metaKey");
        let control = event_bool(&event, "ctrlKey");
        if (meta || control) && matches!(key.as_str(), "z" | "Z" | "y") {
            prevent_default(&event)?;
            if machine_busy || locked {
                return Ok(());
            }
            if key == "y" || shift {
                call_method(&key_keyboard, "redo", &[])?;
            } else {
                call_method(&key_keyboard, "undo", &[])?;
            }
            return Ok(());
        }
        if key == " " {
            if !composition && call_method(&key_keyboard, "space", &[])?.as_bool() == Some(true) {
                prevent_default(&event)?;
            }
            return Ok(());
        }
        if key != "Enter" || composition {
            return Ok(());
        }
        let arbitrated = call_method(
            &key_keyboard,
            "arbitrate",
            &[JsValue::from_str("enter"), JsValue::from_bool(composition)],
        )?;
        if arbitrated.as_string().as_deref() != Some("pass") {
            prevent_default(&event)?;
            return Ok(());
        }
        prevent_default(&event)?;
        if event_bool(&event, "repeat") || locked || machine_busy {
            return Ok(());
        }
        let accelerated = control || meta;
        if accelerated && can_steer_queue {
            call_method(&key_keyboard, "steerQueue", &[])?;
            return Ok(());
        }
        let mode = resolve_mode.apply(
            &JsValue::UNDEFINED,
            &Array::of3(
                &JsValue::from_bool(running),
                &JsValue::from_str(if accelerated { "accelerated" } else { "enter" }),
                &JsValue::from_bool(subagent_is_null),
            ),
        )?;
        call_method(&key_keyboard, "submit", &[mode])?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();

    let change_keyboard = keyboard_value.clone();
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if change_keyboard.is_undefined() || locked || machine_busy {
            return Ok(());
        }
        let target = required(&event, "target", "change event")?;
        let next = required_string(&target, "value", "textarea")?;
        call_method(&change_keyboard, "setDraft", &[JsValue::from_str(&next)])?;
        let caret =
            numeric(&target, "selectionStart")?.unwrap_or_else(|| f64::from(input_text_len(&next)));
        call_method(
            &change_keyboard,
            "track",
            &[JsValue::from_str(&next), JsValue::from_f64(caret)],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();

    let select_keyboard = keyboard_value.clone();
    let select = Closure::wrap(Box::new(move |_event: JsValue| -> Result<(), JsValue> {
        if select_keyboard.is_undefined() {
            return Ok(());
        }
        let snapshot = Reflect::get(&select_keyboard, &JsValue::from_str("snapshot"))?;
        if !Reflect::get(&snapshot, &JsValue::from_str("paste"))?.is_undefined() {
            call_method(&select_keyboard, "invalidatePaste", &[])?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();

    let copy_input = input.clone();
    let copy_keyboard = keyboard_value.clone();
    let copy_draft = draft.to_owned();
    let copy_restore = restore.clone();
    let copy = Closure::wrap(Box::new(move |event: JsValue| {
        copy_or_cut(
            &event,
            false,
            &copy_input,
            &copy_keyboard,
            &copy_draft,
            &copy_restore,
            machine_busy,
            locked,
        )
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();
    let cut_input = input.clone();
    let cut_keyboard = keyboard_value.clone();
    let cut_draft = draft.to_owned();
    let cut_restore = restore.clone();
    let cut = Closure::wrap(Box::new(move |event: JsValue| {
        copy_or_cut(
            &event,
            true,
            &cut_input,
            &cut_keyboard,
            &cut_draft,
            &cut_restore,
            machine_busy,
            locked,
        )
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();

    let paste_keyboard = keyboard_value.clone();
    let paste_intake = intake_images.clone().dyn_into::<Function>()?;
    let paste_restore = restore.clone();
    let paste = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if paste_keyboard.is_undefined() || machine_busy || locked {
            return Ok(());
        }
        let clipboard = required(&event, "clipboardData", "paste event")?;
        let items = Array::from(&required(&clipboard, "items", "clipboard data")?);
        let files = Array::new();
        for item in items.iter() {
            if Reflect::get(&item, &JsValue::from_str("kind"))?
                .as_string()
                .as_deref()
                == Some("file")
            {
                let file = call_method(&item, "getAsFile", &[])?;
                if !file.is_null() {
                    files.push(&file);
                }
            }
        }
        if files.length() > 0 {
            paste_intake.call1(&JsValue::UNDEFINED, files.as_ref())?;
        }
        let text = call_method(&clipboard, "getData", &[JsValue::from_str("text/plain")])?
            .as_string()
            .unwrap_or_default();
        if text.is_empty() {
            if files.length() > 0 {
                prevent_default(&event)?;
            }
            return Ok(());
        }
        prevent_default(&event)?;
        let input = required(&event, "currentTarget", "paste event")?;
        let (start, end) = selection_of(&input)?;
        call_method(
            &paste_keyboard,
            "pasteBegin",
            &[
                JsValue::from_str(&text),
                selection_value(start, end)?.into(),
            ],
        )?;
        let caret = start.saturating_add(input_text_len(&text));
        restore_caret(&paste_restore, &input, caret)?;
        let snapshot = Reflect::get(&paste_keyboard, &JsValue::from_str("snapshot"))?;
        call_method(
            &paste_keyboard,
            "track",
            &[
                required(&snapshot, "draft", "InputState")?,
                JsValue::from_f64(f64::from(caret)),
            ],
        )?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value();

    let start_ref = composing_ref.clone();
    let composition_start = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        set_ref_current(&start_ref, &JsValue::TRUE)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let end_ref = composing_ref.clone();
    let composition_end = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let end_ref = end_ref.clone();
        let callback = Closure::once_into_js(move || set_ref_current(&end_ref, &JsValue::FALSE));
        global_function("setTimeout")?.call2(
            &JsValue::UNDEFINED,
            &callback,
            &JsValue::from_f64(10.0),
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();

    Ok(InputHandlers {
        key_down,
        change,
        select,
        copy,
        cut,
        paste,
        composition_start,
        composition_end,
    })
}

#[allow(clippy::too_many_arguments)]
fn copy_or_cut(
    event: &JsValue,
    cut: bool,
    input: &JsValue,
    keyboard: &JsValue,
    draft: &str,
    restore: &RestoreCaret,
    machine_busy: bool,
    locked: bool,
) -> Result<(), JsValue> {
    if input.is_undefined() || keyboard.is_undefined() {
        return Ok(());
    }
    let textarea = required(event, "currentTarget", "clipboard event")?;
    let (start, end) = selection_of(&textarea)?;
    if start == end {
        return Ok(());
    }
    let occurrences = required(input, "occurrences", "InputState")?.dyn_into::<Array>()?;
    let mut touched = Vec::new();
    for occurrence in occurrences.iter() {
        let offset = number_to_u32(numeric_required(&occurrence, "offset", "input occurrence")?)?;
        if offset >= start && offset < end {
            touched.push((offset, occurrence));
        }
    }
    if touched.is_empty() && !cut {
        return Ok(());
    }
    prevent_default(event)?;
    touched.sort_by_key(|(offset, _)| *offset);
    let mut text = String::new();
    let mut cursor = start;
    for (offset, occurrence) in touched {
        text.push_str(&slice_input_text(draft, cursor, offset));
        text.push_str(&required_string(
            &occurrence,
            "clipboardText",
            "input occurrence",
        )?);
        cursor = offset.saturating_add(1);
    }
    text.push_str(&slice_input_text(draft, cursor, end));
    let clipboard = required(event, "clipboardData", "clipboard event")?;
    call_method(
        &clipboard,
        "setData",
        &[JsValue::from_str("text/plain"), JsValue::from_str(&text)],
    )?;
    if cut && !machine_busy && !locked {
        let next = format!(
            "{}{}",
            slice_input_text(draft, 0, start),
            slice_input_text(draft, end, input_text_len(draft))
        );
        call_method(
            keyboard,
            "setDraft",
            &[
                JsValue::from_str(&next),
                object(&[
                    ("start", JsValue::from_f64(f64::from(start))),
                    ("end", JsValue::from_f64(f64::from(end))),
                    ("insertedLength", JsValue::from_f64(0.0)),
                ])?
                .into(),
            ],
        )?;
        restore_caret(restore, &textarea, start)?;
    }
    Ok(())
}

fn keep_focus_callback(input_ref: &JsValue) -> JsValue {
    let input_ref = input_ref.clone();
    Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        prevent_default(&event)?;
        let input = ref_current(&input_ref)?;
        if !input.is_null() && !input.is_undefined() {
            call_method(
                &input,
                "focus",
                &[object(&[("preventScroll", JsValue::TRUE)])?.into()],
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value()
}

fn toggle_command_callback(props: &JsValue, input_ref: &JsValue) -> Result<JsValue, JsValue> {
    let toggle = optional(props, "toggleCommandMenu")?.unwrap_or(JsValue::UNDEFINED);
    let input_ref = input_ref.clone();
    Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let input = ref_current(&input_ref)?;
        if input.is_null() || input.is_undefined() || toggle.is_undefined() {
            return Ok(());
        }
        let (start, end) = selection_of(&input)?;
        toggle
            .clone()
            .dyn_into::<Function>()?
            .call1(&JsValue::UNDEFINED, selection_value(start, end)?.as_ref())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value())
}

#[allow(clippy::fn_params_excessive_bools)] // Mirrors the source's primary-button gate tuple.
fn primary_callback(
    props: &JsValue,
    input_actions: Option<&JsValue>,
    primary_stops: bool,
    empty: bool,
    disabled: bool,
    machine_busy: bool,
) -> Result<JsValue, JsValue> {
    let stop = optional(props, "stop")?.unwrap_or(JsValue::UNDEFINED);
    let actions = input_actions.cloned().unwrap_or(JsValue::UNDEFINED);
    Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if primary_stops {
            if !stop.is_undefined() {
                stop.clone()
                    .dyn_into::<Function>()?
                    .call0(&JsValue::UNDEFINED)?;
            }
            return Ok(());
        }
        if !actions.is_undefined() && !empty && !disabled && !machine_busy {
            call_method(&actions, "submit", &[])?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value())
}

fn selection_of(textarea: &JsValue) -> Result<(u32, u32), JsValue> {
    let start = number_to_u32(numeric(textarea, "selectionStart")?.unwrap_or(0.0))?;
    let end = number_to_u32(numeric(textarea, "selectionEnd")?.unwrap_or(f64::from(start)))?;
    Ok((start, end))
}

fn selection_value(start: u32, end: u32) -> Result<Object, JsValue> {
    object(&[
        ("start", JsValue::from_f64(f64::from(start))),
        ("end", JsValue::from_f64(f64::from(end))),
    ])
}

fn event_bool(event: &JsValue, key: &str) -> bool {
    Reflect::get(event, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn prevent_default(event: &JsValue) -> Result<(), JsValue> {
    call_method(event, "preventDefault", &[]).map(|_| ())
}

fn inert_decorations() -> Result<JsValue, JsValue> {
    Ok(object(&[
        ("token", JsValue::NULL),
        ("chips", Array::new().into()),
        ("textRefs", Array::new().into()),
        ("hint", JsValue::NULL),
    ])?
    .into())
}

#[allow(clippy::too_many_lines)] // Ordered token/chip/text-ref segmentation stays one pass.
fn render_backdrop(
    modules: &BrowserModules,
    draft: &str,
    input: &JsValue,
    decorations: &JsValue,
    has_goal: bool,
    translate: &Function,
) -> Result<Vec<JsValue>, JsValue> {
    let mut output = Vec::new();
    let mut cursor = 0_u32;
    let token = Reflect::get(decorations, &JsValue::from_str("token"))?;
    if !token.is_null() {
        let start = number_to_u32(numeric_required(&token, "start", "token decoration")?)?;
        let end = number_to_u32(numeric_required(&token, "end", "token decoration")?)?;
        if start > cursor {
            output.push(JsValue::from_str(&slice_input_text(draft, cursor, start)));
        }
        output.push(create_element(
            &modules.react,
            &JsValue::from_str("mark"),
            Some(&object(&[
                ("key", JsValue::from_str("token")),
                ("className", JsValue::from_str(class("hlToken"))),
                ("data-decoration", JsValue::from_str("token")),
            ])?),
            &[JsValue::from_str(&slice_input_text(draft, start, end))],
        )?);
        cursor = end;
    }
    let mut boundaries = Vec::new();
    let chips = required(decorations, "chips", "draft decorations")?.dyn_into::<Array>()?;
    for chip in chips.iter() {
        boundaries.push(Boundary::Chip {
            at: number_to_u32(numeric_required(&chip, "offset", "chip decoration")?)?,
            occurrence_id: numeric_required(&chip, "occurrenceId", "chip decoration")?,
            label: required_string(&chip, "label", "chip decoration")?,
            invalid: Reflect::get(&chip, &JsValue::from_str("invalid"))?.as_bool() == Some(true),
        });
    }
    let references = required(decorations, "textRefs", "draft decorations")?.dyn_into::<Array>()?;
    for reference in references.iter() {
        boundaries.push(Boundary::TextRef {
            at: number_to_u32(numeric_required(
                &reference,
                "start",
                "text-ref decoration",
            )?)?,
            end: number_to_u32(numeric_required(&reference, "end", "text-ref decoration")?)?,
        });
    }
    boundaries.sort_by(|left, right| {
        left.at()
            .partial_cmp(&right.at())
            .unwrap_or(Ordering::Equal)
    });
    for boundary in boundaries {
        if boundary.at() < cursor {
            continue;
        }
        if boundary.at() > cursor {
            output.push(JsValue::from_str(&slice_input_text(
                draft,
                cursor,
                boundary.at(),
            )));
        }
        match boundary {
            Boundary::Chip {
                at,
                occurrence_id,
                label,
                invalid,
            } => {
                let class_name =
                    class_names(&[(class("chip"), true), (class("chipInvalid"), invalid)]);
                let label_node = create_element(
                    &modules.react,
                    &JsValue::from_str("span"),
                    Some(&object(&[(
                        "className",
                        JsValue::from_str(class("chipLabel")),
                    )])?),
                    &[JsValue::from_str(&label)],
                )?;
                output.push(create_element(
                    &modules.react,
                    &JsValue::from_str("span"),
                    Some(&object(&[
                        (
                            "key",
                            JsValue::from_str(&format!("chip-{}", number_string(occurrence_id)?)),
                        ),
                        ("className", JsValue::from_str(&class_name)),
                        ("data-decoration", JsValue::from_str("chip")),
                        ("data-occurrence", JsValue::from_f64(occurrence_id)),
                        (
                            "data-invalid",
                            if invalid {
                                JsValue::TRUE
                            } else {
                                JsValue::UNDEFINED
                            },
                        ),
                        ("title", JsValue::from_str(&label)),
                    ])?),
                    &[label_node],
                )?);
                cursor = at.saturating_add(1);
            }
            Boundary::TextRef { at, end } => {
                output.push(create_element(
                    &modules.react,
                    &JsValue::from_str("mark"),
                    Some(&object(&[
                        ("key", JsValue::from_str(&format!("ref-{at}"))),
                        ("className", JsValue::from_str(class("textRef"))),
                        ("data-decoration", JsValue::from_str("text-ref")),
                    ])?),
                    &[JsValue::from_str(&slice_input_text(draft, at, end))],
                )?);
                cursor = end;
            }
        }
    }
    if cursor < input_text_len(draft) {
        output.push(JsValue::from_str(&slice_input_text(
            draft,
            cursor,
            input_text_len(draft),
        )));
    }
    let hint = Reflect::get(decorations, &JsValue::from_str("hint"))?;
    if !hint.is_null() {
        let raw_hint = hint.as_string().unwrap_or_default();
        let command_name = if input.is_undefined() {
            String::new()
        } else {
            let claim = Reflect::get(input, &JsValue::from_str("claim"))?;
            if claim.is_null() || claim.is_undefined() {
                String::new()
            } else {
                required_string(&claim, "token", "input claim")?
                    .strip_prefix('/')
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            }
        };
        let key = if command_name == "goal" && has_goal {
            "hint.goal.active".to_owned()
        } else {
            format!("hint.{command_name}")
        };
        let translated = translate_key(translate, &key)?;
        let display = if translated.as_string().as_deref() == Some(key.as_str()) {
            JsValue::from_str(&raw_hint)
        } else {
            translated
        };
        output.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                ("key", JsValue::from_str("hint")),
                ("className", JsValue::from_str(class("hint"))),
                ("data-decoration", JsValue::from_str("hint")),
            ])?),
            &[display],
        )?);
    }
    Ok(output)
}

#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines
)] // Closed React tree mirrors the source component.
fn render_tree(
    modules: &BrowserModules,
    props: &JsValue,
    notice: &JsValue,
    permissions: &JsValue,
    preview: &JsValue,
    toast: &JsValue,
    image_limits: &JsValue,
    rail_items: &Array,
    backdrop: &[JsValue],
    draft: &str,
    phase: &str,
    plan_active: bool,
    command_menu_open: bool,
    drag_active: bool,
    can_accept_drop: bool,
    flags: &BarFlags,
    input_ref: &JsValue,
    card_ref: &JsValue,
    scroll_ref: &JsValue,
    mirror_ref: &JsValue,
    set_preview: &Function,
    close_preview: &JsValue,
    dismiss_toast: &JsValue,
    keep_focus: &JsValue,
    toggle_command: &JsValue,
    primary: &JsValue,
    primary_label: &JsValue,
    request_workspace: Option<&JsValue>,
    handlers: &InputHandlers,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let variant = Reflect::get(props, &JsValue::from_str("variant"))?.as_string();
    let root_class = class_names(&[
        (class("root"), true),
        (class("hero"), variant.as_deref() == Some("hero")),
    ]);
    let mut root_children = Vec::new();
    if drag_active {
        let limits = if image_limits.is_undefined() {
            JsValue::UNDEFINED
        } else {
            object(&[
                (
                    "count",
                    required(image_limits, "maxImagesPerMessage", "image limits")?,
                ),
                (
                    "size",
                    JsValue::from_str(&image_size_text_browser(numeric_required(
                        image_limits,
                        "maxImageBytes",
                        "image limits",
                    )?)?),
                ),
            ])?
            .into()
        };
        root_children.push(create_element(
            &modules.react,
            &modules.drop_overlay,
            Some(&object(&[
                ("disabled", JsValue::from_bool(!can_accept_drop)),
                (
                    "labels",
                    drop_overlay_labels_browser(translate.clone(), can_accept_drop, limits)?,
                ),
            ])?),
            &[],
        )?);
    }
    if !toast.is_null() {
        let icon = create_element(&modules.react, &modules.warning, None, &[])?;
        root_children.push(create_element(
            &modules.react,
            &modules.toast,
            Some(&object(&[
                ("key", required(toast, "seq", "toast")?),
                ("text", required(toast, "text", "toast")?),
                ("icon", icon),
                ("anchor", ref_current(card_ref)?),
                ("onDone", dismiss_toast.clone()),
            ])?),
            &[],
        )?);
    }
    if !notice.is_null() && !notice.is_undefined() {
        let error = required_string(notice, "level", "input notice")? == "error";
        root_children.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class_names(&[
                        (class("notice"), true),
                        (class("noticeError"), error),
                    ])),
                ),
                ("role", JsValue::from_str("status")),
            ])?),
            &[required(notice, "text", "input notice")?],
        )?);
    }

    let mut card_children = Vec::new();
    for (key, class_name) in [("overlay", "overlayAnchor"), ("accessory", "accessory")] {
        let value = optional_value(props, key)?;
        if !value.is_undefined() {
            card_children.push(create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props(class(class_name))?),
                &[value],
            )?);
        }
    }
    if rail_items.length() > 0 {
        let open_setter = set_preview.clone();
        let on_open = Closure::wrap(Box::new(move |item: JsValue| -> Result<(), JsValue> {
            open_setter.call1(
                &JsValue::UNDEFINED,
                &required(&item, "attachment", "attachment rail item")?,
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let remove = optional(props, "removeImage")?.unwrap_or(JsValue::UNDEFINED);
        let on_remove = Closure::wrap(Box::new(move |item: JsValue| -> Result<(), JsValue> {
            if !remove.is_undefined() {
                let attachment = required(&item, "attachment", "attachment rail item")?;
                remove.clone().dyn_into::<Function>()?.call1(
                    &JsValue::UNDEFINED,
                    &required(&attachment, "id", "ComposerAttachment")?,
                )?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        let rail = create_element(
            &modules.react,
            &modules.attachment_rail,
            Some(&object(&[
                ("items", rail_items.clone().into()),
                ("labels", attachment_rail_labels_browser(translate.clone())?),
                ("onOpen", on_open),
                ("onRemove", on_remove),
            ])?),
            &[],
        )?;
        card_children.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&class_props(class("attachments"))?),
            &[rail],
        )?);
    }
    let backdrop_node = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("aria-hidden", JsValue::TRUE),
            ("className", JsValue::from_str(class("backdrop"))),
            ("data-input-backdrop", JsValue::from_str("")),
        ])?),
        backdrop,
    )?;
    let placeholder = placeholder(props, flags, plan_active, translate)?;
    let workspace_open = optional_bool(props, "workspacePickerOpen", false)?;
    let textarea = create_element(
        &modules.react,
        &JsValue::from_str("textarea"),
        Some(&object(&[
            ("ref", input_ref.clone()),
            ("className", JsValue::from_str(class("input"))),
            ("value", JsValue::from_str(draft)),
            ("disabled", JsValue::from_bool(flags.textarea_disabled)),
            (
                "readOnly",
                JsValue::from_bool(flags.machine_busy || flags.workspace_trigger),
            ),
            (
                "aria-label",
                if flags.workspace_trigger {
                    translate_key(translate, "hero.chooseWorkspace")?
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-haspopup",
                if flags.workspace_trigger {
                    JsValue::from_str("menu")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-expanded",
                if flags.workspace_trigger {
                    JsValue::from_bool(workspace_open)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("data-phase", JsValue::from_str(phase)),
            ("placeholder", placeholder),
            ("rows", JsValue::from_f64(2.0)),
            ("onChange", handlers.change.clone()),
            ("onKeyDown", handlers.key_down.clone()),
            ("onSelect", handlers.select.clone()),
            ("onCopy", handlers.copy.clone()),
            ("onCut", handlers.cut.clone()),
            ("onPaste", handlers.paste.clone()),
            ("onCompositionStart", handlers.composition_start.clone()),
            ("onCompositionEnd", handlers.composition_end.clone()),
        ])?),
        &[],
    )?;
    let mirror = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", mirror_ref.clone()),
            ("aria-hidden", JsValue::TRUE),
            ("className", JsValue::from_str(class("mirror"))),
            ("data-input-mirror", JsValue::from_str("")),
        ])?),
        &[JsValue::from_str(&format!("{draft}\n"))],
    )?;
    let grow = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class("grow"))?),
        &[backdrop_node, textarea, mirror],
    )?;
    card_children.push(create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", scroll_ref.clone()),
            ("className", JsValue::from_str(class("scroll"))),
            ("data-input-scroll", JsValue::from_str("")),
        ])?),
        &[grow],
    )?);

    let render_slot = required_function(props, "renderSlot", "InputBar props")?;
    let access = if let Some(command) = optional(props, "command")? {
        create_element(
            &modules.react,
            &modules.permission_select,
            Some(&object(&[
                ("key", optional_value(props, "sessionId")?),
                ("value", permissions.clone()),
                ("locked", JsValue::from_bool(flags.locked)),
                ("command", command),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?
    } else {
        JsValue::FALSE
    };
    let plan = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.input.plan"),
        object(&[("locked", JsValue::from_bool(flags.locked))])?.as_ref(),
    )?;
    let command_button = tooltip_button(
        modules,
        translate_key(translate, "input.commands")?,
        create_element(
            &modules.react,
            &modules.plus,
            Some(&object(&[("size", JsValue::from_f64(14.0))])?),
            &[],
        )?,
        object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(class("add"))),
            ("aria-label", translate_key(translate, "input.commands")?),
            ("aria-haspopup", JsValue::from_str("listbox")),
            ("aria-expanded", JsValue::from_bool(command_menu_open)),
            (
                "disabled",
                JsValue::from_bool(flags.locked || optional(props, "toggleCommandMenu")?.is_none()),
            ),
            ("onMouseDown", keep_focus.clone()),
            ("onClick", toggle_command.clone()),
        ])?,
    )?;
    let modes = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class("modes"))?),
        &[access, plan],
    )?;
    let tools = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class("tools"))?),
        &[command_button, modes, optional_value(props, "leftItems")?],
    )?;
    let model_slot = render_slot.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str("conversation.input.model"),
        object(&[("locked", JsValue::from_bool(flags.model_seat_locked))])?.as_ref(),
    )?;
    let context_meter = create_element(
        &modules.react,
        &modules.context_meter,
        Some(&object(&[
            (
                "useProjection",
                required(props, "useProjection", "InputBar props")?,
            ),
            ("t", translate.clone().into()),
        ])?),
        &[],
    )?;
    let stop = optional(props, "stop")?;
    let interrupt = if flags.interruptible {
        tooltip_button(
            modules,
            translate_key(translate, "input.stop")?,
            stop_glyph(&modules.react)?,
            object(&[
                ("type", JsValue::from_str("button")),
                ("className", JsValue::from_str(class("primary"))),
                ("aria-label", translate_key(translate, "input.stop")?),
                ("disabled", JsValue::from_bool(stop.is_none())),
                ("onMouseDown", keep_focus.clone()),
                ("onClick", stop.clone().unwrap_or(JsValue::UNDEFINED)),
            ])?,
        )?
    } else {
        JsValue::FALSE
    };
    let primary_glyph = if flags.primary_stops {
        stop_glyph(&modules.react)?
    } else {
        send_glyph(&modules.react)?
    };
    let primary_button = tooltip_button(
        modules,
        primary_label.clone(),
        primary_glyph,
        object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(class("primary"))),
            ("aria-label", primary_label.clone()),
            (
                "disabled",
                JsValue::from_bool(if flags.primary_stops {
                    stop.is_none()
                } else {
                    flags.empty || flags.disabled || flags.machine_busy
                }),
            ),
            ("onMouseDown", keep_focus.clone()),
            ("onClick", primary.clone()),
        ])?,
    )?;
    let trailing = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class("trailing"))?),
        &[
            optional_value(props, "rightItems")?,
            model_slot,
            context_meter,
            interrupt,
            primary_button,
        ],
    )?;
    card_children.push(create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class("row"))?),
        &[tools, trailing],
    )?);
    let card_click = request_workspace.cloned().unwrap_or(JsValue::UNDEFINED);
    let pointer_down = if flags.workspace_trigger {
        Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            call_method(&event, "stopPropagation", &[])?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
    } else {
        JsValue::UNDEFINED
    };
    let card = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", card_ref.clone()),
            (
                "className",
                JsValue::from_str(&class_names(&[
                    (class("card"), true),
                    (class("cardWorkspaceTrigger"), flags.workspace_trigger),
                ])),
            ),
            ("data-composer-card", JsValue::from_str("")),
            (
                "onClick",
                if flags.workspace_trigger {
                    card_click
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("onPointerDown", pointer_down),
        ])?),
        &card_children,
    )?;
    root_children.push(card);
    if !preview.is_null() {
        let file = required(preview, "file", "ComposerAttachment")?;
        let name = Reflect::get(&file, &JsValue::from_str("name"))?
            .as_string()
            .unwrap_or_default();
        root_children.push(create_element(
            &modules.react,
            &modules.image_lightbox,
            Some(&object(&[
                (
                    "src",
                    required(preview, "previewUrl", "ComposerAttachment")?,
                ),
                (
                    "alt",
                    if name.is_empty() {
                        translate_key(translate, "image.original")?
                    } else {
                        JsValue::from_str(&name)
                    },
                ),
                ("labels", lightbox_labels_browser(translate.clone())?),
                ("onClose", close_preview.clone()),
            ])?),
            &[],
        )?);
    }
    root_children.push(optional_value(props, "footer")?);
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[("className", JsValue::from_str(&root_class))])?),
        &root_children,
    )
}

fn placeholder(
    props: &JsValue,
    flags: &BarFlags,
    plan_active: bool,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    if let Some(value) = optional(props, "placeholder")? {
        return Ok(value);
    }
    let key = if flags.parent_offline {
        "placeholder.parentOffline"
    } else if flags.disabled {
        "placeholder.unavailable"
    } else if flags.can_steer_queue {
        "placeholder.steerQueue"
    } else if plan_active {
        "placeholder.plan"
    } else {
        "placeholder.default"
    };
    translate_key(translate, key)
}

#[allow(clippy::needless_pass_by_value)] // Button props are created for and consumed by this element.
fn tooltip_button(
    modules: &BrowserModules,
    label: JsValue,
    child: JsValue,
    button_props: Object,
) -> Result<JsValue, JsValue> {
    let button = create_element(
        &modules.react,
        &JsValue::from_str("button"),
        Some(&button_props),
        &[child],
    )?;
    create_element(
        &modules.react,
        &modules.tooltip,
        Some(&object(&[
            ("label", label),
            ("side", JsValue::from_str("top")),
            ("delayMs", JsValue::from_f64(500.0)),
        ])?),
        &[button],
    )
}

fn stop_glyph(react: &JsValue) -> Result<JsValue, JsValue> {
    let stop_shape = create_element(
        react,
        &JsValue::from_str("rect"),
        Some(&object(&[
            ("x", JsValue::from_str("3")),
            ("y", JsValue::from_str("3")),
            ("width", JsValue::from_str("10")),
            ("height", JsValue::from_str("10")),
            ("rx", JsValue::from_str("3")),
            ("fill", JsValue::from_str("currentColor")),
        ])?),
        &[],
    )?;
    create_element(
        react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("width", JsValue::from_str("16")),
            ("height", JsValue::from_str("16")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[stop_shape],
    )
}

fn send_glyph(react: &JsValue) -> Result<JsValue, JsValue> {
    let path = create_element(
        react,
        &JsValue::from_str("path"),
        Some(&object(&[
            (
                "d",
                JsValue::from_str(
                    "M8.3125 0.980183C8.66767 1.0531 8.97902 1.20418 9.2627 1.43233C9.48724 1.61297 9.73029 1.85793 9.97949 2.10714L14.707 6.83468L13.293 8.24874L9 3.95577V15.0417H7V3.95577L2.70703 8.24874L1.29297 6.83468L6.02051 2.10714C6.26971 1.85793 6.51277 1.61297 6.7373 1.43233C6.97662 1.23986 7.28445 1.04402 7.6875 0.980183C7.8973 0.947006 8.1031 0.95516 8.3125 0.980183Z",
                ),
            ),
            ("fill", JsValue::from_str("currentColor")),
        ])?),
        &[],
    )?;
    create_element(
        react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("width", JsValue::from_str("16")),
            ("height", JsValue::from_str("16")),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[path],
    )
}

fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, Object::as_ref));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn class_names(values: &[(&str, bool)]) -> String {
    values
        .iter()
        .filter_map(|(value, enabled)| enabled.then_some(*value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn translate_key(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn translate_values(
    translate: &Function,
    key: &str,
    values: &[(&str, JsValue)],
) -> Result<JsValue, JsValue> {
    translate.apply(
        &JsValue::UNDEFINED,
        &Array::of2(&JsValue::from_str(key), object(values)?.as_ref()),
    )
}

fn global_function(name: &str) -> Result<Function, JsValue> {
    required(&js_sys::global(), name, "global")?.dyn_into()
}

fn call_method(value: &JsValue, key: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required_function(value, key, "object")?;
    let arguments: Array = arguments.iter().collect();
    function.apply(value, &arguments)
}

fn numeric(value: &JsValue, key: &str) -> Result<Option<f64>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_f64()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a number")).into())
    }
}

fn numeric_required(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn number_to_u32(value: f64) -> Result<u32, JsValue> {
    number_string(value)?
        .parse::<u32>()
        .map_err(|_| js_sys::RangeError::new("number must be a u32").into())
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
