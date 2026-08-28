//! Compiled delayed hover-preview card with portal, copy, and placement lifecycles.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    COPIED_FEEDBACK_MS, browser_util::begin_clipboard_write, configure_client_ui_primitive_hooks,
    use_pointer_grace,
};

const HOVER_CARD_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/HoverCard.module.css");
const CARD_GAP_PX: f64 = 8.0;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    react_dom: JsValue,
}

/// Configures React/ReactDOM and installs the `HoverCard` stylesheet.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveHoverCard)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_hover_card(
    react: JsValue,
    react_dom: JsValue,
) -> Result<(), JsValue> {
    configure_client_ui_primitive_hooks(react.clone());
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, react_dom });
    });
    inject_style()
}

/// Returns the compiled `HoverCard` component.
///
/// # Errors
///
/// Returns before the browser modules are configured.
#[wasm_bindgen(js_name = hoverCardComponent)]
pub fn hover_card_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_hover_card(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

#[allow(clippy::too_many_lines)]
fn render_hover_card(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let anchor_content = required_property(props, "anchor", "HoverCard props")?;
    let card_content = required_property(props, "content", "HoverCard props")?;
    let open_delay = optional_number(props, "openDelayMs")?.unwrap_or(500.0);
    let disabled = property_truthy(props, "disabled")?;
    let copy_text = optional_string(props, "copyText")?;
    let copy_label = optional_string(props, "copyLabel")?.unwrap_or_else(|| "复制".to_owned());
    let copied_label =
        optional_string(props, "copiedLabel")?.unwrap_or_else(|| "复制成功".to_owned());
    let copyable = copy_text.is_some();

    let root_ref = use_ref(react, &JsValue::NULL)?;
    let card_ref = use_ref(react, &JsValue::NULL)?;
    let timer_ref = use_ref(react, &JsValue::NULL)?;
    let copy_timer_ref = use_ref(react, &JsValue::NULL)?;
    let copy_height_ref = use_ref(react, &JsValue::NULL)?;
    let copy_epoch_ref = use_ref(react, &JsValue::from_f64(0.0))?;
    let copying_ref = use_ref(react, &JsValue::FALSE)?;
    let mounted_ref = use_ref(react, &JsValue::TRUE)?;
    let (open_value, set_open) = use_state(react, &JsValue::FALSE)?;
    let open = open_value.as_bool().unwrap_or(false);
    let (position, set_position) = use_state(react, &JsValue::NULL)?;
    let (copied_value, set_copied) = use_state(react, &JsValue::FALSE)?;
    let copied = copied_value.as_bool().unwrap_or(false);

    let clear_copy_timer = copy_timer_ref.clone();
    let clear_copy_height = copy_height_ref.clone();
    let clear_copy_state = set_copied.clone();
    let clear_copied = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        clear_timeout_ref(&clear_copy_timer)?;
        set_current(&clear_copy_height, &JsValue::NULL)?;
        set_state(&clear_copy_state, &JsValue::FALSE)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let clear_copied = use_callback(react, &clear_copied.into_js_value(), &Array::new())?;

    let close_epoch = copy_epoch_ref.clone();
    let close_clear_copied = clear_copied.clone();
    let close_set_open = set_open.clone();
    let close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        increment_epoch(&close_epoch)?;
        close_clear_copied.call0(&JsValue::UNDEFINED)?;
        set_state(&close_set_open, &JsValue::FALSE)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let close = use_callback(
        react,
        &close.into_js_value(),
        &Array::of1(clear_copied.as_ref()),
    )?;
    let grace = use_pointer_grace(close.clone())?;
    let arm_close = required_function(&grace, "arm", "pointer grace")?;
    let cancel_close = required_function(&grace, "cancel", "pointer grace")?;

    install_disabled_effect(react, disabled, &timer_ref, &cancel_close, &close)?;
    install_mount_effect(
        react,
        &mounted_ref,
        &copy_epoch_ref,
        &timer_ref,
        &copy_timer_ref,
    )?;
    install_placement_effect(react, open, &root_ref, &card_ref, &set_position)?;
    install_correction_effect(react, open, &position, &card_ref, &set_position)?;

    let copy_action = copy_text.as_ref().map(|text| {
        let text = text.clone();
        let copying_ref = copying_ref.clone();
        let mounted_ref = mounted_ref.clone();
        let copy_epoch_ref = copy_epoch_ref.clone();
        let copy_timer_ref = copy_timer_ref.clone();
        let copy_height_ref = copy_height_ref.clone();
        let card_ref = card_ref.clone();
        let set_copied = set_copied.clone();
        let clear_copied = clear_copied.clone();
        Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            start_copy(
                copied,
                &text,
                &copying_ref,
                &mounted_ref,
                &copy_epoch_ref,
                &copy_timer_ref,
                &copy_height_ref,
                &card_ref,
                &set_copied,
                &clear_copied,
            )
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
        .dyn_into::<Function>()
        .expect("HoverCard copy callback must be callable")
    });

    let mut wrapper_children = vec![anchor_content];
    if open && copyable {
        wrapper_children.push(create_element(
            react,
            &JsValue::from_str("span"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-hover-card-status"),
                ),
                ("role", JsValue::from_str("status")),
            ])?),
            &[if copied {
                JsValue::from_str(&copied_label)
            } else {
                JsValue::from_str("")
            }],
        )?);
    }
    if open && !position.is_null() {
        let card = render_card(
            modules,
            &card_ref,
            &position,
            &copy_height_ref,
            &card_content,
            copy_text.as_deref(),
            &copy_label,
            &copied_label,
            copied,
            copy_action.as_ref(),
        )?;
        let document = required_property(&js_sys::global(), "document", "global")?;
        let body = required_property(&document, "body", "document")?;
        let portal = required_function(&modules.react_dom, "createPortal", "ReactDOM")?.call2(
            &modules.react_dom,
            &card,
            &body,
        )?;
        wrapper_children.push(portal);
    }

    let enter_timer = timer_ref.clone();
    let enter_cancel = cancel_close.clone();
    let enter_set_open = set_open;
    let pointer_enter = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if disabled {
            return Ok(());
        }
        enter_cancel.call0(&JsValue::UNDEFINED)?;
        if open {
            return Ok(());
        }
        clear_timeout_ref(&enter_timer)?;
        let setter = enter_set_open.clone();
        let callback = Closure::wrap(Box::new(move || set_state(&setter, &JsValue::TRUE))
            as Box<dyn FnMut() -> Result<(), JsValue>>);
        let handle = set_timeout(&callback.into_js_value(), open_delay)?;
        set_current(&enter_timer, &handle)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let leave_timer = timer_ref.clone();
    let leave_arm = arm_close;
    let pointer_leave = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        clear_timeout_ref(&leave_timer)?;
        if open {
            leave_arm.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let down_card = card_ref;
    let down_timer = timer_ref;
    let down_cancel = cancel_close;
    let down_close = close;
    let pointer_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let card = current(&down_card)?;
        let target = required_property(&event, "target", "pointer event")?;
        if !card.is_null()
            && call_method(&card, "contains", std::slice::from_ref(&target))?.as_bool()
                == Some(true)
        {
            return Ok(());
        }
        clear_timeout_ref(&down_timer)?;
        down_cancel.call0(&JsValue::UNDEFINED)?;
        down_close.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("ref", root_ref),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-hover-card-root"),
            ),
            ("onPointerEnter", pointer_enter.into_js_value()),
            ("onPointerLeave", pointer_leave.into_js_value()),
            ("onPointerDownCapture", pointer_down.into_js_value()),
        ])?),
        &wrapper_children,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_card(
    modules: &BrowserModules,
    card_ref: &JsValue,
    position: &JsValue,
    copy_height_ref: &JsValue,
    content: &JsValue,
    copy_text: Option<&str>,
    copy_label: &str,
    copied_label: &str,
    copied: bool,
    copy_action: Option<&Function>,
) -> Result<JsValue, JsValue> {
    let copyable = copy_text.is_some();
    let mut classes = "seekdeep-primitive-hover-card-card".to_owned();
    if copyable {
        classes.push_str(" seekdeep-primitive-hover-card-copyable");
    }
    if copied {
        classes.push_str(" seekdeep-primitive-hover-card-feedback");
    }
    let style = Object::new();
    Reflect::set(
        &style,
        &JsValue::from_str("left"),
        &required_property(position, "left", "HoverCard position")?,
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("top"),
        &required_property(position, "top", "HoverCard position")?,
    )?;
    let height = current(copy_height_ref)?;
    if copied && !height.is_null() {
        Reflect::set(&style, &JsValue::from_str("minHeight"), &height)?;
    }
    let click = copy_action.map(|copy_action| {
        let copy_action = copy_action.clone();
        Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let current_target = required_property(&event, "currentTarget", "click event")?;
            if selection_intersects(&current_target)? {
                return Ok(());
            }
            copy_action.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
    });
    let key_down = copy_action.map(|copy_action| {
        let copy_action = copy_action.clone();
        Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let key = required_property(&event, "key", "keyboard event")?
                .as_string()
                .ok_or_else(|| js_sys::TypeError::new("keyboard event key must be a string"))?;
            if key != "Enter" && key != " " {
                return Ok(());
            }
            call_method(&event, "preventDefault", &[])?;
            copy_action.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value()
    });
    let child = if copied {
        create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-hover-card-copied"),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[JsValue::from_str(copied_label)],
        )?
    } else {
        content.clone()
    };
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", card_ref.clone()),
            ("className", JsValue::from_str(&classes)),
            ("style", style.into()),
            (
                "role",
                if copyable {
                    JsValue::from_str("button")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "tabIndex",
                if copyable {
                    JsValue::from_f64(0.0)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-label",
                copy_text.map_or(JsValue::UNDEFINED, |text| {
                    JsValue::from_str(&format!("{copy_label}: {text}"))
                }),
            ),
            ("onClick", click.unwrap_or(JsValue::UNDEFINED)),
            ("onKeyDown", key_down.unwrap_or(JsValue::UNDEFINED)),
        ])?),
        &[child],
    )
}

fn install_disabled_effect(
    react: &JsValue,
    disabled: bool,
    timer_ref: &JsValue,
    cancel_close: &Function,
    close: &Function,
) -> Result<(), JsValue> {
    let timer_ref = timer_ref.clone();
    let cancel_dependency = cancel_close.clone();
    let close_dependency = close.clone();
    let cancel_close = cancel_close.clone();
    let close = close.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if disabled {
            clear_timeout_ref(&timer_ref)?;
            cancel_close.call0(&JsValue::UNDEFINED)?;
            close.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&JsValue::from_bool(disabled));
    dependencies.push(cancel_dependency.as_ref());
    dependencies.push(close_dependency.as_ref());
    use_effect(react, &effect.into_js_value(), &dependencies)
}

fn install_mount_effect(
    react: &JsValue,
    mounted_ref: &JsValue,
    copy_epoch_ref: &JsValue,
    timer_ref: &JsValue,
    copy_timer_ref: &JsValue,
) -> Result<(), JsValue> {
    let mounted_ref = mounted_ref.clone();
    let copy_epoch_ref = copy_epoch_ref.clone();
    let timer_ref = timer_ref.clone();
    let copy_timer_ref = copy_timer_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        set_current(&mounted_ref, &JsValue::TRUE)?;
        let cleanup_mounted = mounted_ref.clone();
        let cleanup_epoch = copy_epoch_ref.clone();
        let cleanup_timer = timer_ref.clone();
        let cleanup_copy_timer = copy_timer_ref.clone();
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            set_current(&cleanup_mounted, &JsValue::FALSE)?;
            increment_epoch(&cleanup_epoch)?;
            clear_timeout_ref(&cleanup_timer)?;
            clear_timeout_ref(&cleanup_copy_timer)
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(react, &effect.into_js_value(), &Array::new())
}

fn install_placement_effect(
    react: &JsValue,
    open: bool,
    root_ref: &JsValue,
    card_ref: &JsValue,
    set_position: &Function,
) -> Result<(), JsValue> {
    let root_ref = root_ref.clone();
    let card_ref = card_ref.clone();
    let set_position = set_position.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open {
            set_state(&set_position, &JsValue::NULL)?;
            return Ok(JsValue::UNDEFINED);
        }
        let place_root = root_ref.clone();
        let place_card = card_ref.clone();
        let place_setter = set_position.clone();
        let place = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let wrapper = current(&place_root)?;
            if wrapper.is_null() {
                return Ok(());
            }
            let bounds = call_method(&wrapper, "getBoundingClientRect", &[])?;
            let top = required_number(&bounds, "top", "HoverCard anchor DOMRect")?;
            let right = required_number(&bounds, "right", "HoverCard anchor DOMRect")?;
            let card = current(&place_card)?;
            let height = if card.is_null() {
                0.0
            } else {
                required_number(&card, "offsetHeight", "HoverCard card")?
            };
            let window = required_property(&js_sys::global(), "window", "global")?;
            let inner_height = required_number(&window, "innerHeight", "window")?;
            let fitted_top = if top + height > inner_height - CARD_GAP_PX {
                inner_height - height - CARD_GAP_PX
            } else {
                top
            };
            set_state(
                &place_setter,
                &object(&[
                    ("left", JsValue::from_f64(right + CARD_GAP_PX)),
                    ("top", JsValue::from_f64(fitted_top)),
                ])?
                .into(),
            )
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let place = place.into_js_value().dyn_into::<Function>()?;
        place.call0(&JsValue::UNDEFINED)?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[
                JsValue::from_str("scroll"),
                place.clone().into(),
                JsValue::TRUE,
            ],
        )?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("resize"), place.clone().into()],
        )?;
        let cleanup_window = window;
        let cleanup_place = place;
        Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &cleanup_window,
                "removeEventListener",
                &[
                    JsValue::from_str("scroll"),
                    cleanup_place.clone().into(),
                    JsValue::TRUE,
                ],
            )?;
            call_method(
                &cleanup_window,
                "removeEventListener",
                &[JsValue::from_str("resize"), cleanup_place.clone().into()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_layout_effect(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_bool(open)),
    )
}

fn install_correction_effect(
    react: &JsValue,
    open: bool,
    position: &JsValue,
    card_ref: &JsValue,
    set_position: &Function,
) -> Result<(), JsValue> {
    let position = position.clone();
    let position_dependency = position.clone();
    let card_ref = card_ref.clone();
    let set_position = set_position.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !open || position.is_null() {
            return Ok(());
        }
        let card = current(&card_ref)?;
        if card.is_null() {
            return Ok(());
        }
        let height = required_number(&card, "offsetHeight", "HoverCard card")?;
        let top = required_number(&position, "top", "HoverCard position")?;
        let window = required_property(&js_sys::global(), "window", "global")?;
        let inner_height = required_number(&window, "innerHeight", "window")?;
        if top + height > inner_height - CARD_GAP_PX {
            set_state(
                &set_position,
                &object(&[
                    (
                        "left",
                        required_property(&position, "left", "HoverCard position")?,
                    ),
                    (
                        "top",
                        JsValue::from_f64(inner_height - height - CARD_GAP_PX),
                    ),
                ])?
                .into(),
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_layout_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(open), &position_dependency),
    )
}

#[allow(clippy::too_many_arguments)]
fn start_copy(
    copied: bool,
    text: &str,
    copying_ref: &JsValue,
    mounted_ref: &JsValue,
    copy_epoch_ref: &JsValue,
    copy_timer_ref: &JsValue,
    copy_height_ref: &JsValue,
    card_ref: &JsValue,
    set_copied: &Function,
    clear_copied: &Function,
) -> Result<(), JsValue> {
    if copied || current(copying_ref)?.as_bool() == Some(true) {
        return Ok(());
    }
    set_current(copying_ref, &JsValue::TRUE)?;
    let copy_epoch = current(copy_epoch_ref)?.as_f64().unwrap_or(0.0);
    let copying_ref = copying_ref.clone();
    let mounted_ref = mounted_ref.clone();
    let copy_epoch_ref = copy_epoch_ref.clone();
    let copy_timer_ref = copy_timer_ref.clone();
    let copy_height_ref = copy_height_ref.clone();
    let card_ref = card_ref.clone();
    let set_copied = set_copied.clone();
    let clear_copied = clear_copied.clone();
    let pending = begin_clipboard_write(text);
    spawn_local(async move {
        let accepted = JsFuture::from(Promise::resolve(&pending))
            .await
            .ok()
            .and_then(|value| value.as_bool())
            == Some(true);
        let _ = set_current(&copying_ref, &JsValue::FALSE);
        let mounted = current(&mounted_ref).ok().and_then(|value| value.as_bool()) == Some(true);
        let epoch_matches = current(&copy_epoch_ref)
            .ok()
            .and_then(|value| value.as_f64())
            == Some(copy_epoch);
        let card = current(&card_ref).unwrap_or(JsValue::NULL);
        if !accepted || !mounted || !epoch_matches || card.is_null() {
            return;
        }
        let height = required_number(&card, "offsetHeight", "HoverCard card").unwrap_or(0.0);
        let _ = set_current(
            &copy_height_ref,
            &if height > 0.0 {
                JsValue::from_f64(height)
            } else {
                JsValue::NULL
            },
        );
        let _ = set_state(&set_copied, &JsValue::TRUE);
        let reset = clear_copied;
        let callback = Closure::wrap(Box::new(move || {
            let _ = reset.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut()>);
        if let Ok(handle) = set_timeout(&callback.into_js_value(), f64::from(COPIED_FEEDBACK_MS)) {
            let _ = set_current(&copy_timer_ref, &handle);
        }
    });
    Ok(())
}

fn selection_intersects(card: &JsValue) -> Result<bool, JsValue> {
    let window = required_property(&js_sys::global(), "window", "global")?;
    let selection = call_method(&window, "getSelection", &[])?;
    if selection.is_null()
        || required_property(&selection, "isCollapsed", "Selection")?.as_bool() == Some(true)
    {
        return Ok(false);
    }
    let count = required_number(&selection, "rangeCount", "Selection")?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = count as u32;
    for index in 0..count {
        let range = call_method(
            &selection,
            "getRangeAt",
            &[JsValue::from_f64(f64::from(index))],
        )?;
        if call_method(&range, "intersectsNode", std::slice::from_ref(card))?.as_bool()
            == Some(true)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = "@seekdeep-ai/seekdeep-client-ui-primitives/HoverCard.module.css";
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
    let css = HOVER_CARD_CSS
        .replace(".root", ".seekdeep-primitive-hover-card-root")
        .replace(".card", ".seekdeep-primitive-hover-card-card")
        .replace(".copyable", ".seekdeep-primitive-hover-card-copyable")
        .replace(".feedback", ".seekdeep-primitive-hover-card-feedback")
        .replace(".copied", ".seekdeep-primitive-hover-card-copied")
        .replace(".status", ".seekdeep-primitive-hover-card-status");
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin-css"), JsValue::from_str(tag)],
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
        &JsValue::from_str(&css),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives HoverCard module was not configured").into()
        })
    })
}

fn increment_epoch(reference: &JsValue) -> Result<(), JsValue> {
    let epoch = current(reference)?.as_f64().unwrap_or(0.0);
    set_current(reference, &JsValue::from_f64(epoch + 1.0))
}

fn clear_timeout_ref(reference: &JsValue) -> Result<(), JsValue> {
    let handle = current(reference)?;
    if handle.is_null() {
        return Ok(());
    }
    let global = js_sys::global();
    required_function(&global, "clearTimeout", "global")?.call1(&global, &handle)?;
    set_current(reference, &JsValue::NULL)
}

fn set_timeout(callback: &JsValue, delay: f64) -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    required_function(&global, "setTimeout", "global")?.call2(
        &global,
        callback,
        &JsValue::from_f64(delay),
    )
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &Array,
) -> Result<Function, JsValue> {
    required_function(react, "useCallback", "React")?
        .call2(react, callback, dependencies)?
        .dyn_into()
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn use_layout_effect(
    react: &JsValue,
    effect: &JsValue,
    dependencies: &Array,
) -> Result<(), JsValue> {
    required_function(react, "useLayoutEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
    }
}

fn optional_number(value: &JsValue, key: &str) -> Result<Option<f64>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_f64()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a number")).into())
    }
}

fn property_truthy(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?.is_truthy())
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
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
