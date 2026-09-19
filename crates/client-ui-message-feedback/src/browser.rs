//! Browser plugin assembly and compiled React message controls.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    FEEDBACK_EN, FEEDBACK_STYLES, FEEDBACK_ZH, INJECT, LOCALE_NAMESPACE,
    WasmMessageFeedbackController,
};

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
}

/// Configures React and the UI primitive module at Client-factory materialization.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiMessageFeedback)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_message_feedback(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, primitives });
    });
    inject_styles()
}

/// Browser Client plugin body.
///
/// # Errors
///
/// Returns missing-service, locale, slot-registration, or object-construction failures.
#[wasm_bindgen(js_name = applyClientUiMessageFeedback)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_message_feedback(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required_service(&ctx, "slots")?;
    let remote = required_service(&ctx, "remote")?;
    let message_feedback = required_property(&remote, "messageFeedback", "Remote namespace")?;
    let locale = required_service(&ctx, "locale")?;
    own_locale_dictionaries(&ctx, &locale)?;

    let controllers = Rc::new(RefCell::new(IndexMap::<String, JsValue>::new()));
    own_connection_reset(&ctx, &controllers)?;

    let registration_controllers = controllers;
    let registration_remote = message_feedback;
    let component = message_feedback_actions_component(&modules);
    let registrar = slots.clone();
    let install = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let inject_controllers = registration_controllers.clone();
        let inject_remote = registration_remote.clone();
        let inject = Closure::wrap(Box::new(
            move |session_id: JsValue| -> Result<JsValue, JsValue> {
                let session_id = session_id.as_string().ok_or_else(|| {
                    js_error("ui-message-feedback: injected Session id must be a string")
                })?;
                let controller = if let Some(controller) =
                    inject_controllers.borrow().get(&session_id).cloned()
                {
                    controller
                } else {
                    let controller: JsValue = WasmMessageFeedbackController::new(
                        inject_remote.clone(),
                        session_id.clone(),
                    )
                    .into();
                    inject_controllers
                        .borrow_mut()
                        .insert(session_id, controller.clone());
                    controller
                };
                injected_face(&controller)
            },
        )
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let options = object(&[
            (
                "name",
                JsValue::from_str("conversation.chat.assistant-actions"),
            ),
            ("id", JsValue::from_str("feedback")),
            ("order", JsValue::from_f64(10.0)),
            ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
            ("inject", inject.into_js_value()),
        ])?;
        let dispose_registration =
            call_method(&registrar, "register", &[options.into(), component.clone()])?
                .dyn_into::<Function>()?;
        let dispose_controllers = registration_controllers.clone();
        let dispose = Closure::wrap(Box::new(move || {
            let _ = dispose_registration.call0(&JsValue::UNDEFINED);
            for controller in dispose_controllers.borrow().values() {
                let _ = call_method(controller, "dispose", &[]);
            }
            dispose_controllers.borrow_mut().clear();
        }) as Box<dyn FnMut()>);
        Ok(dispose.into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.chat.assistant-actions"),
            install.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Exact Client plugin dependency list.
#[wasm_bindgen(js_name = messageFeedbackInject)]
pub fn message_feedback_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

fn injected_face(controller: &JsValue) -> Result<JsValue, JsValue> {
    let hooks = object(&[("feedback", controller.clone())])?;
    let ensure_controller = controller.clone();
    let ensure = Closure::wrap(
        Box::new(move || call_method(&ensure_controller, "ensure", &[]))
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
    );
    let rate_controller = controller.clone();
    let rate = Closure::wrap(Box::new(
        move |message_id: JsValue, rating: JsValue, note: JsValue| -> Result<JsValue, JsValue> {
            call_method(&rate_controller, "rate", &[message_id, rating, note])
        },
    )
        as Box<dyn FnMut(JsValue, JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let toggle_controller = controller.clone();
    let toggle = Closure::wrap(Box::new(
        move |message_id: JsValue, rating: JsValue| -> Result<JsValue, JsValue> {
            call_method(&toggle_controller, "toggle", &[message_id, rating])
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    let clear_note_controller = controller.clone();
    let clear_note = Closure::wrap(Box::new(move |message_id: JsValue| {
        call_method(&clear_note_controller, "clearNote", &[message_id])
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let clear_controller = controller.clone();
    let clear = Closure::wrap(Box::new(move |message_id: JsValue| {
        call_method(&clear_controller, "clear", &[message_id])
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    object(&[
        ("hooks", hooks.into()),
        ("ensure", ensure.into_js_value()),
        ("rate", rate.into_js_value()),
        ("toggle", toggle.into_js_value()),
        ("clearNote", clear_note.into_js_value()),
        ("clear", clear.into_js_value()),
    ])
    .map(Into::into)
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = object(&[
        ("zh", dictionary(FEEDBACK_ZH)?),
        ("en", dictionary(FEEDBACK_EN)?),
    ])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(LOCALE_NAMESPACE),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-message-feedback: dictionaries"),
        ],
    )?;
    Ok(())
}

fn own_connection_reset(
    ctx: &JsValue,
    controllers: &Rc<RefCell<IndexMap<String, JsValue>>>,
) -> Result<(), JsValue> {
    let controllers = controllers.clone();
    let reset = Closure::wrap(Box::new(move || {
        for controller in controllers.borrow().values() {
            let snapshot = match call_method(controller, "getSnapshot", &[]) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    log_error("[ui-message-feedback] reset snapshot failed:", &error);
                    continue;
                }
            };
            let status = Reflect::get(&snapshot, &JsValue::from_str("status"))
                .ok()
                .and_then(|value| value.as_string());
            if status.as_deref() != Some("cold")
                && let Err(error) = call_method(controller, "resync", &[])
            {
                log_error("[ui-message-feedback] reset resync failed:", &error);
            }
        }
    }) as Box<dyn FnMut()>);
    call_method(
        ctx,
        "on",
        &[JsValue::from_str("connection/reset"), reset.into_js_value()],
    )?;
    Ok(())
}

fn message_feedback_actions_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    Closure::wrap(
        Box::new(move |props: JsValue| render_message_feedback_actions(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_message_feedback_actions(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let message_id = required_property(props, "messageId", "feedback action props")?;
    let ensure = function(props, "ensure")?;
    let rate = function(props, "rate")?;
    let toggle = function(props, "toggle")?;
    let clear_note = function(props, "clearNote")?;
    let use_feedback = function(props, "useFeedback")?;
    let translate = function(props, "t")?;

    let selected_message = message_id.clone();
    let item_selector = Closure::wrap(Box::new(move |view: JsValue| {
        let items = required_property(&view, "items", "feedback view")?;
        call_method(&items, "get", std::slice::from_ref(&selected_message))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let item = use_feedback.call1(props, &item_selector.into_js_value())?;
    let load_selector = Closure::wrap(Box::new(move |view: JsValue| {
        Ok(Reflect::get(&view, &JsValue::from_str("status"))?
            .as_string()
            .as_deref()
            == Some("error"))
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    let load_failed = use_feedback
        .call1(props, &load_selector.into_js_value())?
        .as_bool()
        .unwrap_or(false);
    let rating = optional_string(&item, "rating")?;
    let item_note = optional_string(&item, "note")?;

    let (note_open, set_note_open) = use_state(&ui.react, &JsValue::FALSE)?;
    let note_open = note_open.as_bool().unwrap_or(false);
    let (draft, set_draft) = use_state(&ui.react, &JsValue::from_str(""))?;
    let draft = draft.as_string().unwrap_or_default();
    let (pending, set_pending) = use_state(&ui.react, &JsValue::FALSE)?;
    let pending = pending.as_bool().unwrap_or(false);
    let (failure, set_failure) = use_state(&ui.react, &JsValue::NULL)?;

    let seeded = use_ref(&ui.react, &JsValue::FALSE)?;
    let seed_ref = seeded.clone();
    let seed_ensure = ensure.clone();
    let seed = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if Reflect::get(&seed_ref, &JsValue::from_str("current"))?
            .as_bool()
            .unwrap_or(false)
        {
            return Ok(());
        }
        Reflect::set(&seed_ref, &JsValue::from_str("current"), &JsValue::TRUE)?;
        seed_ensure.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let seed = use_callback(
        &ui.react,
        &seed.into_js_value(),
        &Array::of1(ensure.as_ref()),
    )?;

    let alive = use_ref(&ui.react, &JsValue::TRUE)?;
    let cleanup_alive = alive.clone();
    let alive_effect = Closure::wrap(Box::new(move || -> JsValue {
        let alive = cleanup_alive.clone();
        Closure::wrap(Box::new(move || {
            let _ = Reflect::set(&alive, &JsValue::from_str("current"), &JsValue::FALSE);
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    function(&ui.react, "useEffect")?.call2(
        &ui.react,
        &alive_effect.into_js_value(),
        &Array::new(),
    )?;

    let settle_alive = alive.clone();
    let settle_pending = set_pending.clone();
    let settle_failure = set_failure.clone();
    let settle_translate = translate.clone();
    let settle = Closure::wrap(Box::new(move |result: JsValue| -> Result<(), JsValue> {
        if !current_bool(&settle_alive)? {
            return Ok(());
        }
        set_state(&settle_pending, &JsValue::FALSE)?;
        if property_bool(&result, "ok")? {
            set_state(&settle_failure, &JsValue::NULL)?;
            return Ok(());
        }
        let error = required_property(&result, "error", "feedback action result")?;
        let code = required_string(&error, "code", "feedback action error")?;
        let key = if code == "version-conflict" {
            "error.conflict"
        } else {
            "error.generic"
        };
        let message = settle_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))?;
        set_state(&settle_failure, &message)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let settle = use_callback(
        &ui.react,
        &settle.into_js_value(),
        &Array::of1(translate.as_ref()),
    )?;

    let on_rate_message = message_id.clone();
    let on_rate_toggle = toggle.clone();
    let on_rate_settle = settle.clone();
    let on_rate_pending = set_pending.clone();
    let on_rate_failure = set_failure.clone();
    let on_rate_note = set_note_open.clone();
    let on_rate = Closure::wrap(Box::new(move |next: JsValue| -> Result<(), JsValue> {
        set_state(&on_rate_pending, &JsValue::TRUE)?;
        set_state(&on_rate_failure, &JsValue::NULL)?;
        set_state(&on_rate_note, &JsValue::FALSE)?;
        let pending = on_rate_toggle.call2(&JsValue::UNDEFINED, &on_rate_message, &next)?;
        settle_promise(&pending, &on_rate_settle)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let on_rate = use_callback(
        &ui.react,
        &on_rate.into_js_value(),
        &Array::of3(&message_id, settle.as_ref(), toggle.as_ref()),
    )?;

    let save_dependencies = Array::new();
    save_dependencies.push(clear_note.as_ref());
    save_dependencies.push(&JsValue::from_str(&draft));
    save_dependencies.push(&message_id);
    save_dependencies.push(rate.as_ref());
    save_dependencies.push(settle.as_ref());
    let save_message = message_id.clone();
    let save_rate = rate;
    let save_clear_note = clear_note;
    let save_settle = settle;
    let save_alive = alive;
    let save_pending = set_pending;
    let save_failure = set_failure;
    let save_note_open = set_note_open.clone();
    let save_draft = draft.clone();
    let on_save = Closure::wrap(Box::new(move |current: JsValue| -> Result<(), JsValue> {
        let trimmed = save_draft.trim().to_owned();
        set_state(&save_pending, &JsValue::TRUE)?;
        set_state(&save_failure, &JsValue::NULL)?;
        let pending = if trimmed.is_empty() {
            save_clear_note.call1(&JsValue::UNDEFINED, &save_message)?
        } else {
            save_rate.call3(
                &JsValue::UNDEFINED,
                &save_message,
                &current,
                &JsValue::from_str(&trimmed),
            )?
        };
        let settled = save_settle.clone();
        let alive = save_alive.clone();
        let note_open = save_note_open.clone();
        let then = Closure::wrap(Box::new(move |result: JsValue| -> Result<(), JsValue> {
            settled.call1(&JsValue::UNDEFINED, &result)?;
            if property_bool(&result, "ok")? && current_bool(&alive)? {
                set_state(&note_open, &JsValue::FALSE)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let then = then.into_js_value().unchecked_into::<Function>();
        settle_promise(&pending, &then)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let on_save = use_callback(&ui.react, &on_save.into_js_value(), &save_dependencies)?;

    let open_note_value = item_note.clone().unwrap_or_default();
    let open_note_dependencies = Array::of1(&JsValue::from_str(&open_note_value));
    let open_note_draft = set_draft.clone();
    let open_note_state = set_note_open.clone();
    let open_note = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        set_state(&open_note_draft, &JsValue::from_str(&open_note_value))?;
        set_state(&open_note_state, &JsValue::TRUE)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let open_note = use_callback(
        &ui.react,
        &open_note.into_js_value(),
        &open_note_dependencies,
    )?;

    let like_active = rating.as_deref() == Some("positive");
    let dislike_active = rating.as_deref() == Some("negative");
    let like_label = translated(
        &translate,
        if like_active {
            "action.likeActive"
        } else {
            "action.like"
        },
    )?;
    let dislike_label = translated(
        &translate,
        if dislike_active {
            "action.dislikeActive"
        } else {
            "action.dislike"
        },
    )?;
    let like_rate = on_rate.clone();
    let like_click = Closure::wrap(Box::new(move || {
        like_rate.call1(&JsValue::UNDEFINED, &JsValue::from_str("positive"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dislike_rate = on_rate;
    let dislike_click = Closure::wrap(Box::new(move || {
        dislike_rate.call1(&JsValue::UNDEFINED, &JsValue::from_str("negative"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);

    let like_button = rating_button(
        ui,
        "IconLikeOutline16",
        &like_label,
        like_active,
        pending,
        &seed,
        like_click.into_js_value(),
    )?;
    let dislike_button = rating_button(
        ui,
        "IconDislikeOutline16",
        &dislike_label,
        dislike_active,
        pending,
        &seed,
        dislike_click.into_js_value(),
    )?;
    let mut children = vec![
        tooltip(ui, &like_label, like_button)?,
        tooltip(ui, &dislike_label, dislike_button)?,
    ];

    if let Some(current_rating) = rating.as_deref() {
        if note_open {
            children.push(note_editor(
                ui,
                &translate,
                &draft,
                pending,
                &on_save,
                current_rating,
                &set_note_open,
                &set_draft,
            )?);
        } else {
            let label = match item_note {
                Some(note) => JsValue::from_str(&note),
                None => translated(&translate, "note.open")?,
            };
            children.push(ui.tag(
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-feedback-note-open"),
                    ),
                    ("onClick", open_note.into()),
                ])?),
                &[label],
            )?);
        }
    }

    let failure_text = if let Some(failure) = failure.as_string() {
        Some(JsValue::from_str(&failure))
    } else if load_failed {
        Some(translated(&translate, "error.load")?)
    } else {
        None
    };
    if let Some(failure) = failure_text {
        children.push(ui.tag(
            "span",
            Some(&object(&[
                ("className", JsValue::from_str("seekdeep-feedback-failure")),
                ("role", JsValue::from_str("status")),
            ])?),
            &[failure],
        )?);
    }
    ui.fragment(&children)
}

#[allow(clippy::too_many_arguments)]
fn rating_button(
    ui: &ReactUi,
    icon: &str,
    label: &JsValue,
    active: bool,
    pending: bool,
    seed: &Function,
    click: JsValue,
) -> Result<JsValue, JsValue> {
    let icon = ui.primitive(icon, None, &[])?;
    ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-feedback-action")),
            ("aria-label", label.clone()),
            ("aria-pressed", JsValue::from_bool(active)),
            (
                "data-active",
                if active {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("disabled", JsValue::from_bool(pending)),
            ("onFocus", seed.clone().into()),
            ("onPointerEnter", seed.clone().into()),
            ("onClick", click),
        ])?),
        &[icon],
    )
}

fn tooltip(ui: &ReactUi, label: &JsValue, child: JsValue) -> Result<JsValue, JsValue> {
    ui.primitive(
        "Tooltip",
        Some(&object(&[
            ("label", label.clone()),
            ("side", JsValue::from_str("bottom")),
        ])?),
        &[child],
    )
}

#[allow(clippy::too_many_arguments)]
fn note_editor(
    ui: &ReactUi,
    translate: &Function,
    draft: &str,
    pending: bool,
    on_save: &Function,
    rating: &str,
    set_note_open: &Function,
    set_draft: &Function,
) -> Result<JsValue, JsValue> {
    let draft_setter = set_draft.clone();
    let textarea_change = Closure::wrap(Box::new(move |event: JsValue| {
        let target = required_property(&event, "target", "textarea change event")?;
        let value = required_property(&target, "value", "textarea target")?;
        set_state(&draft_setter, &value)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let on_save = on_save.clone();
    let rating = rating.to_owned();
    let save = Closure::wrap(Box::new(move || {
        on_save.call1(&JsValue::UNDEFINED, &JsValue::from_str(&rating))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let close = set_note_open.clone();
    let cancel = Closure::wrap(Box::new(move || set_state(&close, &JsValue::FALSE))
        as Box<dyn FnMut() -> Result<(), JsValue>>);
    let textarea = ui.tag(
        "textarea",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-feedback-note-input"),
            ),
            ("aria-label", translated(translate, "note.aria")?),
            ("placeholder", translated(translate, "note.placeholder")?),
            ("value", JsValue::from_str(draft)),
            ("rows", JsValue::from_f64(2.0)),
            ("onChange", textarea_change.into_js_value()),
        ])?),
        &[],
    )?;
    let save = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-feedback-note-save"),
            ),
            ("disabled", JsValue::from_bool(pending)),
            ("onClick", save.into_js_value()),
        ])?),
        &[translated(translate, "note.save")?],
    )?;
    let cancel = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-feedback-note-cancel"),
            ),
            ("onClick", cancel.into_js_value()),
        ])?),
        &[translated(translate, "note.cancel")?],
    )?;
    ui.tag(
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-feedback-note-editor"),
        )])?),
        &[textarea, save, cancel],
    )
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into::<Function>()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef")?.call1(react, initial)
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &Array,
) -> Result<Function, JsValue> {
    function(react, "useCallback")?
        .call2(react, callback, dependencies)?
        .dyn_into::<Function>()
}

fn current_bool(reference: &JsValue) -> Result<bool, JsValue> {
    Ok(Reflect::get(reference, &JsValue::from_str("current"))?
        .as_bool()
        .unwrap_or(false))
}

fn property_bool(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?
        .as_bool()
        .unwrap_or(false))
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn settle_promise(value: &JsValue, callback: &Function) -> Result<(), JsValue> {
    let promise: JsValue = Promise::resolve(value).into();
    call_method(&promise, "then", &[callback.clone().into()]).map(|_| ())
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn dictionary(entries: &[(&str, &str)]) -> Result<JsValue, JsValue> {
    let dictionary = Object::new();
    for (key, value) in entries {
        set(&dictionary, key, &JsValue::from_str(value))?;
    }
    Ok(dictionary.into())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_error("client-ui-message-feedback module factory did not configure browser modules")
        })
    })
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-message-feedback";
    const TAG_ID: &str =
        "@seekdeep-ai/seekdeep-client-ui-message-feedback/MessageFeedbackActions.module.css";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_undefined() || document.is_null() {
        return Ok(());
    }
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
    {
        let selector = format!(
            "style[data-plugin-css={}]",
            serde_json::to_string(TAG_ID).expect("static selector")
        );
        if !query
            .call1(&document, &JsValue::from_str(&selector))?
            .is_null()
        {
            return Ok(());
        }
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin"), JsValue::from_str(PACKAGE)],
    )?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin-css"),
            JsValue::from_str(TAG_ID),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(FEEDBACK_STYLES),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() || service.is_null() {
        Err(js_error(&format!(
            "client-ui-message-feedback requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_error(&format!(
            "ui-message-feedback: {owner} omitted required property {key:?}"
        )))
    } else {
        Ok(property)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| {
            js_error(&format!(
                "ui-message-feedback: {owner} {key:?} must be a string"
            ))
        })
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_error("ui-message-feedback: expected an optional string"))
    }
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required_property(value, key, "object")?.dyn_into::<Function>()
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        set(&object, key, value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn log_error(message: &str, error: &JsValue) {
    let global = js_sys::global();
    if let Ok(console) = Reflect::get(&global, &JsValue::from_str("console"))
        && let Ok(log) = Reflect::get(&console, &JsValue::from_str("error"))
            .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
    {
        let _ = log.call2(&console, &JsValue::from_str(message), error);
    }
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
    primitives: JsValue,
}

impl ReactUi {
    fn element(
        &self,
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
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }

    fn tag(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        self.element(&JsValue::from_str(name), props, children)
    }

    fn primitive(
        &self,
        name: &str,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        self.element(
            &required_property(&self.primitives, name, "UI primitives")?,
            props,
            children,
        )
    }

    fn fragment(&self, children: &[JsValue]) -> Result<JsValue, JsValue> {
        self.element(
            &required_property(&self.react, "Fragment", "React")?,
            None,
            children,
        )
    }
}
