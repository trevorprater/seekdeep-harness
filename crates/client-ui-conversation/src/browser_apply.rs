//! Compiled browser plugin assembly for the conversation product surface.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    BrowserComposerBlockRegistry, BrowserConversationController, BrowserInputHub,
    CONVERSATION_LOCALE_NAMESPACE, configure_client_ui_conversation_chat_store,
    configure_client_ui_conversation_controller, conversation_locales_browser,
    create_chat_store_browser, register_chat_node_renderers_browser,
    register_conversation_nodes_browser,
};

const INJECT: &[&str] = &[
    "slots",
    "layout",
    "sessions",
    "workspaces",
    "locale",
    "connection",
    "remote",
    "settingsScope",
    "conversationEvents",
    "conversationViews",
];

const COMPONENTS: &[&str] = &[
    "ConversationRoot",
    "ConversationSession",
    "ConversationSessionHeader",
    "InputBar",
    "ApprovalPanel",
    "ChatView",
    "StatsLine",
    "DetailsPanel",
    "EnterBehaviorRow",
    "todoDockEntry",
    "queueDockEntry",
    "UserMessageNodeView",
    "ContextMessageNodeView",
    "AssistantNodeView",
    "CommandNodeView",
    "ManualCompactionNodeView",
    "CompactionNodeView",
    "RetryNodeView",
    "TurnErrorNodeView",
    "TurnMaxTokensNodeView",
    "TurnTailNodeView",
    "UnknownNodeView",
];

thread_local! {
    static APPLY_COMPONENTS: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Configures apply-owned component, store, and UUID dependencies.
///
/// # Errors
///
/// Returns before mutation when any required compiled component is absent.
#[wasm_bindgen(js_name = configureClientUiConversationApply)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_apply(
    components: JsValue,
    define_store: Function,
    uuid_factory: Function,
) -> Result<(), JsValue> {
    for name in COMPONENTS {
        required(&components, name, "conversation apply components")?;
    }
    configure_client_ui_conversation_chat_store(define_store);
    configure_client_ui_conversation_controller(uuid_factory);
    APPLY_COMPONENTS.with(|configured| *configured.borrow_mut() = Some(components));
    Ok(())
}

/// Mounts the complete browser conversation plugin into one Client Context.
///
/// # Errors
///
/// Returns for missing services/configuration or any registry, service, provider, locale, or Slot
/// assembly failure.
#[wasm_bindgen(js_name = applyClientUiConversation)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn apply_client_ui_conversation(ctx: JsValue) -> Result<(), JsValue> {
    let components = configured_components()?;
    for dependency in INJECT {
        required(&ctx, dependency, "Client Context")?;
    }
    let slots = required(&ctx, "slots", "Client Context")?;
    let sessions = required(&ctx, "sessions", "Client Context")?;
    let workspaces = required(&ctx, "workspaces", "Client Context")?;
    let layout = required(&ctx, "layout", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;

    register_conversation_nodes_browser(ctx.clone())?;
    register_chat_node_renderers_browser(ctx.clone(), components.clone())?;
    own_locale(&ctx, &locale)?;
    let translate = call_method(
        &locale,
        "bind",
        &[JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)],
    )?
    .dyn_into::<Function>()?;
    let chat_store = create_chat_store_browser()?;
    let settings = settings_face(&ctx)?;
    let submission_policy = submission_policy(&settings)?;
    let input: JsValue = BrowserInputHub::new(ctx.clone(), translate.clone()).into();
    let blocks: JsValue = BrowserComposerBlockRegistry::new().into();
    let controller = BrowserConversationController::new(
        ctx.clone(),
        object(&[("input", input.clone()), ("blocks", blocks.clone())])?.into(),
    )?
    .into_service_face()?;
    call_method(
        &ctx,
        "provide",
        &[JsValue::from_str("conversation"), controller.clone()],
    )?;
    own_input_provider(&ctx, &sessions, &input)?;

    let views = view_tabs_face(&slots)?;
    let scroll_positions = Rc::new(RefCell::new(BTreeMap::<String, JsValue>::new()));
    register_settings(&slots, &components, &submission_policy)?;
    register_root(&slots, &components, &sessions, &workspaces, &input, &blocks)?;
    register_session(
        &slots,
        &components,
        &chat_store,
        &views,
        &sessions,
        &input,
        &controller,
    )?;
    register_header(&slots, &components, &chat_store, &views, &sessions)?;
    register_composer(
        &slots,
        &components,
        &sessions,
        &input,
        &controller,
        &submission_policy,
        &translate,
    )?;
    register_approval(&slots, &components)?;
    register_chat(
        &ctx,
        &slots,
        &components,
        &chat_store,
        &sessions,
        &workspaces,
        &layout,
        &controller,
        &translate,
        scroll_positions,
    )?;
    register_details(&slots, &components, &chat_store, &layout)?;
    call_method(
        &slots,
        "register",
        &[
            object(&[
                ("name", JsValue::from_str("conversation.composer.dock")),
                ("id", JsValue::from_str("stats")),
                ("order", JsValue::from_f64(0.0)),
                ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
            ])?
            .into(),
            required(&components, "StatsLine", "conversation apply components")?,
        ],
    )?;
    for entry in ["todoDockEntry", "queueDockEntry"] {
        call_method(
            &ctx,
            "plugin",
            &[required(
                &components,
                entry,
                "conversation apply components",
            )?],
        )?;
    }
    Ok(())
}

/// Returns the exact browser dependency list.
#[wasm_bindgen(js_name = conversationInject)]
pub fn conversation_inject_browser() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

fn own_locale(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let locale = locale.clone();
    let dictionaries = conversation_locales_browser()?;
    let setup = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE),
                dictionaries.clone(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-conversation: dictionaries"),
        ],
    )?;
    Ok(())
}

fn own_input_provider(ctx: &JsValue, sessions: &JsValue, input: &JsValue) -> Result<(), JsValue> {
    let resolve_input = input.clone();
    let resolve = Closure::wrap(
        Box::new(move |binding: JsValue| -> Result<JsValue, JsValue> {
            let shell = call_method(&resolve_input, "shellFor", &[binding])?;
            object(&[
                (
                    "hooks",
                    object(&[("input", required(&shell, "state", "SessionInput shell")?)])?.into(),
                ),
                (
                    "props",
                    object(&[(
                        "inputActions",
                        required(&shell, "actions", "SessionInput shell")?,
                    )])?
                    .into(),
                ),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    let provider = object(&[
        ("hooks", Array::of1(&JsValue::from_str("input")).into()),
        (
            "props",
            Array::of1(&JsValue::from_str("inputActions")).into(),
        ),
        ("resolve", resolve.into_js_value()),
    ])?;
    let sessions = sessions.clone();
    let setup = Closure::wrap(Box::new(move || {
        call_method(&sessions, "provide", &[provider.clone().into()])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("ui-conversation: input standard-kit provider"),
        ],
    )?;
    Ok(())
}

fn settings_face(ctx: &JsValue) -> Result<JsValue, JsValue> {
    let settings = required(ctx, "settingsScope", "Client Context")?;
    call_method(
        &settings,
        "bind",
        &[object(&[("namespace", JsValue::from_str("ui-conversation"))])?.into()],
    )
}

fn submission_policy(host: &JsValue) -> Result<JsValue, JsValue> {
    let state = Rc::new(RefCell::new(read_busy_enter(host)?));
    let listeners = Rc::new(RefCell::new(BTreeMap::<u64, Function>::new()));
    let next_listener = Rc::new(Cell::new(0_u64));
    let store = Object::new();
    let read_state = state.clone();
    let get_snapshot =
        Closure::wrap(Box::new(move || read_state.borrow().clone()) as Box<dyn FnMut() -> String>);
    set(&store, "getSnapshot", &get_snapshot.into_js_value())?;
    let subscribe_listeners = listeners.clone();
    let subscribe_next = next_listener;
    let subscribe = Closure::wrap(Box::new(move |listener: Function| -> JsValue {
        let id = subscribe_next.get().wrapping_add(1);
        subscribe_next.set(id);
        subscribe_listeners.borrow_mut().insert(id, listener);
        let listeners = subscribe_listeners.clone();
        Closure::wrap(Box::new(move || {
            listeners.borrow_mut().remove(&id);
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    set(&store, "subscribe", &subscribe.into_js_value())?;

    let host_for_set = host.clone();
    let set_state = state.clone();
    let set_listeners = listeners.clone();
    let set_busy_enter = Closure::wrap(Box::new(move |behavior: String| -> Result<(), JsValue> {
        if !matches!(behavior.as_str(), "queue" | "steer") {
            return Err(js_sys::TypeError::new("busy Enter behavior is invalid").into());
        }
        if *set_state.borrow() == behavior {
            return Ok(());
        }
        set_state.borrow_mut().clone_from(&behavior);
        notify_listeners(&set_listeners)?;
        call_method(
            &host_for_set,
            "set",
            &[JsValue::from_str("busyEnter"), JsValue::from_str(&behavior)],
        )?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);

    let resolve_state = state.clone();
    let resolve = Closure::wrap(Box::new(
        move |running: bool, gesture: String, steering: bool| -> String {
            if !running || !steering {
                return "queue".to_owned();
            }
            let preferred = resolve_state.borrow().clone();
            if gesture == "accelerated" {
                if preferred == "queue" {
                    "steer".to_owned()
                } else {
                    "queue".to_owned()
                }
            } else {
                preferred
            }
        },
    ) as Box<dyn FnMut(bool, String, bool) -> String>);

    let adopt_host = host.clone();
    let adopt_state = state;
    let adopt_listeners = listeners;
    let adopt = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let next = read_busy_enter(&adopt_host)?;
        if *adopt_state.borrow() != next {
            *adopt_state.borrow_mut() = next;
            notify_listeners(&adopt_listeners)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    call_method(host, "subscribe", &[adopt.into_js_value()])?;

    object(&[
        ("busyEnter", store.into()),
        ("setBusyEnter", set_busy_enter.into_js_value()),
        ("resolve", resolve.into_js_value()),
    ])
    .map(Into::into)
}

fn read_busy_enter(host: &JsValue) -> Result<String, JsValue> {
    let snapshot = call_method(host, "getSnapshot", &[])?;
    let value = Reflect::get(&snapshot, &JsValue::from_str("value"))?;
    if value.is_null() || value.is_undefined() {
        return Ok("queue".to_owned());
    }
    Ok(Reflect::get(&value, &JsValue::from_str("busyEnter"))?
        .as_string()
        .filter(|behavior| matches!(behavior.as_str(), "queue" | "steer"))
        .unwrap_or_else(|| "queue".to_owned()))
}

fn notify_listeners(listeners: &Rc<RefCell<BTreeMap<u64, Function>>>) -> Result<(), JsValue> {
    for listener in listeners.borrow().values() {
        listener.call0(&JsValue::UNDEFINED)?;
    }
    Ok(())
}

fn register_settings(
    slots: &JsValue,
    components: &JsValue,
    policy: &JsValue,
) -> Result<(), JsValue> {
    let policy_for_inject = policy.clone();
    let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        object(&[
            (
                "hooks",
                object(&[(
                    "busyEnter",
                    required(&policy_for_inject, "busyEnter", "submission policy")?,
                )])?
                .into(),
            ),
            (
                "setBusyEnter",
                required(&policy_for_inject, "setBusyEnter", "submission policy")?,
            ),
        ])
        .map(Into::into)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let options = object(&[
        ("name", JsValue::from_str("settings.general.item")),
        ("id", JsValue::from_str("composer-enter")),
        ("order", JsValue::from_f64(20.0)),
        ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
        ("inject", inject.into_js_value()),
    ])?;
    let slots_for_install = slots.clone();
    let component = required(
        components,
        "EnterBehaviorRow",
        "conversation apply components",
    )?;
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &slots_for_install,
            "register",
            &[options.clone().into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        slots,
        "inject",
        &[
            JsValue::from_str("settings.general.item"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

fn register_root(
    slots: &JsValue,
    components: &JsValue,
    sessions: &JsValue,
    workspaces: &JsValue,
    input: &JsValue,
    blocks: &JsValue,
) -> Result<(), JsValue> {
    let inject_sessions = sessions.clone();
    let inject_workspaces = workspaces.clone();
    let inject_input = input.clone();
    let inject_blocks = blocks.clone();
    let inject = Closure::wrap(
        Box::new(move |session_id: JsValue| -> Result<JsValue, JsValue> {
            let composer_block = if session_id.is_undefined() {
                empty_store(JsValue::UNDEFINED)?
            } else {
                call_method(
                    &inject_blocks,
                    "storeFor",
                    std::slice::from_ref(&session_id),
                )?
            };
            let sessions = inject_sessions.clone();
            let workspaces = inject_workspaces.clone();
            let input = inject_input.clone();
            let current = session_id.as_string();
            let select_workspace =
                Closure::wrap(Box::new(move |workspace_id: JsValue| -> Promise {
                    let sessions = sessions.clone();
                    let workspaces = workspaces.clone();
                    let input = input.clone();
                    let current = current.clone();
                    future_to_promise(async move {
                        let next = JsFuture::from(Promise::resolve(&call_method(
                            &workspaces,
                            "connectWorkspace",
                            &[workspace_id],
                        )?))
                        .await?;
                        if let (Some(current), Some(next_id)) = (current, next.as_string())
                            && current != next_id
                        {
                            move_draft(&input, &current, &next_id)?;
                        }
                        call_method(&sessions, "open", std::slice::from_ref(&next))?;
                        Ok(JsValue::UNDEFINED)
                    })
                }) as Box<dyn FnMut(JsValue) -> Promise>);
            object(&[
                (
                    "hooks",
                    object(&[("composerBlock", composer_block)])?.into(),
                ),
                ("selectWorkspace", select_workspace.into_js_value()),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("conversation")),
            ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
            ("children", root_children()?.into()),
            ("inject", inject.into_js_value()),
        ])?,
        required(
            components,
            "ConversationRoot",
            "conversation apply components",
        )?,
    )
}

fn register_session(
    slots: &JsValue,
    components: &JsValue,
    store: &JsValue,
    views: &JsValue,
    _sessions: &JsValue,
    input: &JsValue,
    controller: &JsValue,
) -> Result<(), JsValue> {
    let inject_views = views.clone();
    let inject_input = input.clone();
    let inject_controller = controller.clone();
    let inject = Closure::wrap(Box::new(
        move |session_id: String, _actions: JsValue| -> Result<JsValue, JsValue> {
            let release = inject_controller.clone();
            let release_images = Closure::wrap(Box::new(move |id: String| {
                call_method(&release, "releaseSessionImages", &[JsValue::from_str(&id)])
            })
                as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
            let shell = call_method(&inject_input, "shell", &[JsValue::from_str(&session_id)])?;
            let bind_shell = shell;
            let bind = Closure::wrap(Box::new(move |write: Function| {
                call_method(&bind_shell, "bindMirror", &[write.into()])
            })
                as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
            object(&[
                ("views", inject_views.clone()),
                ("releaseSessionImages", release_images.into_js_value()),
                ("bindDraftMirror", bind.into_js_value()),
            ])
            .map(Into::into)
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<JsValue, JsValue>>);
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("conversation.session")),
            (
                "children",
                object(&[("conversation.view", child("list", "session")?.into())])?.into(),
            ),
            ("store", store.clone()),
            ("inject", inject.into_js_value()),
        ])?,
        required(
            components,
            "ConversationSession",
            "conversation apply components",
        )?,
    )
}

fn register_header(
    slots: &JsValue,
    components: &JsValue,
    store: &JsValue,
    views: &JsValue,
    sessions: &JsValue,
) -> Result<(), JsValue> {
    let inject_views = views.clone();
    let inject_sessions = sessions.clone();
    let inject = Closure::wrap(Box::new(
        move |_id: JsValue, _actions: JsValue| -> Result<JsValue, JsValue> {
            let sessions = inject_sessions.clone();
            let open =
                Closure::wrap(
                    Box::new(move |id: JsValue| call_method(&sessions, "open", &[id]))
                        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
                );
            object(&[
                ("views", inject_views.clone()),
                ("open", open.into_js_value()),
            ])
            .map(Into::into)
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("conversation.session.header")),
            ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
            (
                "children",
                object(&[
                    (
                        "conversation.session.header.actions",
                        child("list", "session")?.into(),
                    ),
                    (
                        "conversation.session.header.utilities",
                        child("list", "session")?.into(),
                    ),
                ])?
                .into(),
            ),
            ("store", store.clone()),
            ("inject", inject.into_js_value()),
        ])?,
        required(
            components,
            "ConversationSessionHeader",
            "conversation apply components",
        )?,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn register_composer(
    slots: &JsValue,
    components: &JsValue,
    sessions: &JsValue,
    input: &JsValue,
    controller: &JsValue,
    policy: &JsValue,
    translate: &Function,
) -> Result<(), JsValue> {
    let inject_sessions = sessions.clone();
    let inject_input = input.clone();
    let inject_controller = controller.clone();
    let resolve_submit = required(policy, "resolve", "submission policy")?;
    let inject_translate = translate.clone();
    let absent_notices = empty_store(JsValue::NULL)?;
    let absent_lexicon = empty_store(Map::new().into())?;
    let absent_launcher = empty_store(JsValue::NULL)?;
    let inject = Closure::wrap(
        Box::new(move |session_id: JsValue| -> Result<JsValue, JsValue> {
            let resolve = resolve_submit.clone();
            if session_id.is_undefined() {
                return object(&[
                    ("keyboard", JsValue::UNDEFINED),
                    ("addImages", JsValue::UNDEFINED),
                    ("removeImage", JsValue::UNDEFINED),
                    ("draftImages", JsValue::UNDEFINED),
                    ("resolveSubmitMode", resolve),
                    ("toggleCommandMenu", JsValue::UNDEFINED),
                    ("stop", JsValue::UNDEFINED),
                    ("command", JsValue::UNDEFINED),
                    (
                        "hooks",
                        object(&[
                            ("notices", absent_notices.clone()),
                            ("lexicon", absent_lexicon.clone()),
                            ("menuLauncher", absent_launcher.clone()),
                        ])?
                        .into(),
                    ),
                ])
                .map(Into::into);
            }
            let id = session_id
                .as_string()
                .ok_or_else(|| js_sys::TypeError::new("composer Session id must be a string"))?;
            let shell = call_method(&inject_input, "shell", &[JsValue::from_str(&id)])?;
            let add_shell = shell.clone();
            let add_controller = inject_controller.clone();
            let add_translate = inject_translate.clone();
            let add_images =
                Closure::wrap(Box::new(move |files: JsValue| -> Result<JsValue, JsValue> {
                    let images = match call_method(&add_controller, "createDraftImages", &[files]) {
                        Ok(images) => images,
                        Err(error) => {
                            if Reflect::get(&error, &JsValue::from_str("name"))?
                                .as_string()
                                .as_deref()
                                == Some("UnsupportedImageMediaTypeError")
                            {
                                return add_translate.call1(
                                    &JsValue::UNDEFINED,
                                    &JsValue::from_str("image.unsupportedType"),
                                );
                            }
                            let message = Reflect::get(&error, &JsValue::from_str("message"))?
                                .as_string()
                                .unwrap_or_else(|| "unknown attachment error".to_owned());
                            return Ok(JsValue::from_str(&message));
                        }
                    };
                    let rows = Array::from(&images);
                    let ids = Array::new();
                    for row in rows.iter() {
                        ids.push(&required(&row, "id", "draft image")?);
                    }
                    let accepted = call_method(&add_shell, "addImages", &[ids.into()])?
                        .as_bool()
                        .unwrap_or(false);
                    if !accepted {
                        call_method(&add_controller, "releaseDraftImages", &[images])?;
                    }
                    Ok(JsValue::NULL)
                })
                    as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
            let remove_shell = shell.clone();
            let remove_controller = inject_controller.clone();
            let remove = Closure::wrap(Box::new(move |id: JsValue| -> Result<(), JsValue> {
                call_method(
                    &remove_controller,
                    "releaseDraftImage",
                    std::slice::from_ref(&id),
                )?;
                call_method(&remove_shell, "removeImage", &[id])?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            let draft_controller = inject_controller.clone();
            let draft_images = Closure::wrap(Box::new(move |ids: JsValue| {
                call_method(&draft_controller, "draftImages", &[ids])
            })
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
            let stop_sessions = inject_sessions.clone();
            let stop_controller = inject_controller.clone();
            let stop_id = id.clone();
            let stop = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let scoped = scoped_controller(&stop_sessions, &stop_controller, &stop_id)?;
                let promise = call_method(&scoped, "cancel", &[])?.dyn_into::<Promise>()?;
                let swallow =
                    Closure::wrap(Box::new(move |_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
                let _ = promise.catch(&swallow);
                drop(swallow.into_js_value());
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let command_sessions = inject_sessions.clone();
            let command_id = id.clone();
            let command = Closure::wrap(Box::new(move |line: String| -> Promise {
                let sessions = command_sessions.clone();
                let id = command_id.clone();
                future_to_promise(async move {
                    let binding = call_method(&sessions, "binding", &[JsValue::from_str(&id)])?;
                    if binding.is_null() || binding.is_undefined() {
                        return Ok(JsValue::FALSE);
                    }
                    let session = required(&binding, "session", "Session binding")?;
                    let returned = call_method(&session, "command", &[JsValue::from_str(&line)])?;
                    let result = JsFuture::from(Promise::resolve(&returned)).await?;
                    let matched = Reflect::get(&result, &JsValue::from_str("ok"))?.as_bool()
                        == Some(true)
                        && Reflect::get(
                            &Reflect::get(&result, &JsValue::from_str("value"))?,
                            &JsValue::from_str("matched"),
                        )?
                        .as_bool()
                            == Some(true);
                    Ok(JsValue::from_bool(matched))
                })
            }) as Box<dyn FnMut(String) -> Promise>);
            let triggers = call_method(&inject_input, "inputTriggers", &[JsValue::from_str(&id)])?;
            let toggle = if triggers.is_undefined() || triggers.is_null() {
                JsValue::UNDEFINED
            } else {
                let toggle_shell = shell.clone();
                let toggle_triggers = triggers.clone();
                Closure::wrap(Box::new(move |selection: JsValue| -> Result<(), JsValue> {
                    call_method(&toggle_shell, "dismissPopup", &[])?;
                    let snapshot = required(&toggle_shell, "snapshot", "SessionInput shell")?;
                    let draft = required_string(&snapshot, "draft", "InputState")?;
                    let start = required_u32(&selection, "start", "selection")?;
                    let position = if crate::slice_input_text(&draft, 0, start).trim().is_empty() {
                        "leading"
                    } else {
                        "inline"
                    };
                    let occurrence = object(&[
                        ("trigger", JsValue::from_str("/")),
                        ("query", JsValue::from_str("")),
                        ("position", JsValue::from_str(position)),
                        (
                            "span",
                            object(&[
                                ("start", required(&selection, "start", "selection")?),
                                ("end", required(&selection, "end", "selection")?),
                                ("draftRev", required(&snapshot, "draftRev", "InputState")?),
                            ])?
                            .into(),
                        ),
                    ])?;
                    call_method(
                        &toggle_triggers,
                        "toggleSource",
                        &[JsValue::from_str("command"), occurrence.into()],
                    )?;
                    Ok(())
                })
                    as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
                .into_js_value()
            };
            let launcher = if triggers.is_null() || triggers.is_undefined() {
                absent_launcher.clone()
            } else {
                Reflect::get(&triggers, &JsValue::from_str("launcher"))?
            };
            object(&[
                ("keyboard", shell.clone()),
                ("addImages", add_images.into_js_value()),
                ("removeImage", remove.into_js_value()),
                ("draftImages", draft_images.into_js_value()),
                ("resolveSubmitMode", resolve),
                ("toggleCommandMenu", toggle),
                ("stop", stop.into_js_value()),
                ("command", command.into_js_value()),
                (
                    "hooks",
                    object(&[
                        (
                            "notices",
                            required(&shell, "notices", "SessionInput shell")?,
                        ),
                        (
                            "lexicon",
                            required(&shell, "lexicon", "SessionInput shell")?,
                        ),
                        ("menuLauncher", launcher),
                    ])?
                    .into(),
                ),
            ])
            .map(Into::into)
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("conversation.composer.bar")),
            ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
            (
                "children",
                object(&[
                    (
                        "conversation.input.plan",
                        child("single", "session")?.into(),
                    ),
                    (
                        "conversation.input.model",
                        child("single", "session")?.into(),
                    ),
                ])?
                .into(),
            ),
            ("inject", inject.into_js_value()),
        ])?,
        required(components, "InputBar", "conversation apply components")?,
    )
}

fn register_approval(slots: &JsValue, components: &JsValue) -> Result<(), JsValue> {
    let select = Closure::wrap(Box::new(move |owner: JsValue| -> Result<JsValue, JsValue> {
        let interactions = required(&owner, "interactions", "composer owner")?;
        for interaction in Array::from(&interactions).iter() {
            if Reflect::get(&interaction, &JsValue::from_str("kind"))?
                .as_string()
                .as_deref()
                == Some("approval")
            {
                return Ok(interaction);
            }
        }
        Ok(JsValue::NULL)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("conversation.composer")),
            ("select", select.into_js_value()),
            ("priority", JsValue::from_f64(1.0)),
            ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
        ])?,
        required(components, "ApprovalPanel", "conversation apply components")?,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn register_chat(
    ctx: &JsValue,
    slots: &JsValue,
    components: &JsValue,
    store: &JsValue,
    sessions: &JsValue,
    workspaces: &JsValue,
    layout: &JsValue,
    controller: &JsValue,
    translate: &Function,
    scroll_positions: Rc<RefCell<BTreeMap<String, JsValue>>>,
) -> Result<(), JsValue> {
    let label_translate = translate.clone();
    let label = Closure::wrap(Box::new(move || {
        label_translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("view.chat"))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let inject_ctx = ctx.clone();
    let inject_sessions = sessions.clone();
    let inject_workspaces = workspaces.clone();
    let inject_layout = layout.clone();
    let inject_controller = controller.clone();
    let inject = Closure::wrap(Box::new(
        move |session_id: String, actions: JsValue| -> Result<JsValue, JsValue> {
            let details_actions = actions.clone();
            let details_layout = inject_layout.clone();
            let open_details =
                Closure::wrap(Box::new(move |target: JsValue| -> Result<(), JsValue> {
                    call_method(&details_actions, "select", &[target])?;
                    call_method(&details_layout, "openDetails", &[])?;
                    Ok(())
                })
                    as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            let mentions_ctx = inject_ctx.clone();
            let file_mentions =
                Closure::wrap(Box::new(move |owner: JsValue| -> Result<JsValue, JsValue> {
                    let service = call_method(
                        &mentions_ctx,
                        "get",
                        &[JsValue::from_str("chatFileMentions")],
                    )?;
                    if service.is_null() || service.is_undefined() {
                        return Ok(JsValue::UNDEFINED);
                    }
                    call_method(&service, "forClosing", &[owner])
                })
                    as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
            let open_sessions = inject_sessions.clone();
            let open_workspaces = inject_workspaces.clone();
            let open_id = session_id.clone();
            let open_file = Closure::wrap(Box::new(move |path: String| -> Result<(), JsValue> {
                let cwd = session_cwd(&open_sessions, &open_id)?;
                let resolved = seekdeep_client_runtime::resolve_workspace_path_js(cwd, &path);
                let returned = call_method(
                    &open_workspaces,
                    "openPath",
                    &[JsValue::from_str(&resolved)],
                )?;
                let promise = Promise::resolve(&returned);
                let swallow =
                    Closure::wrap(Box::new(move |_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
                let _ = promise.catch(&swallow);
                drop(swallow.into_js_value());
                Ok(())
            })
                as Box<dyn FnMut(String) -> Result<(), JsValue>>);
            let older_sessions = inject_sessions.clone();
            let older_controller = inject_controller.clone();
            let older_id = session_id.clone();
            let load_older = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let scoped = scoped_controller(&older_sessions, &older_controller, &older_id)?;
                call_method(&scoped, "loadOlder", &[])?;
                Ok(())
            })
                as Box<dyn FnMut() -> Result<(), JsValue>>);
            let image_controller = inject_controller.clone();
            let image_id = session_id.clone();
            let load_image = Closure::wrap(Box::new(move |attachment: JsValue| {
                call_method(
                    &image_controller,
                    "resolveImage",
                    &[JsValue::from_str(&image_id), attachment],
                )
            })
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
            let inspect_actions = actions.clone();
            let inspect = Closure::wrap(Box::new(move |call_id: JsValue| -> Result<(), JsValue> {
                call_method(
                    &inspect_actions,
                    "setInspect",
                    &[object(&[("callId", call_id)])?.into()],
                )?;
                call_method(
                    &inspect_actions,
                    "setView",
                    &[JsValue::from_str("trajectory")],
                )?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            let save_positions = scroll_positions.clone();
            let save_id = session_id.clone();
            let save = Closure::wrap(Box::new(move |position: JsValue| {
                if position.is_null() {
                    save_positions.borrow_mut().remove(&save_id);
                } else {
                    save_positions
                        .borrow_mut()
                        .insert(save_id.clone(), position);
                }
            }) as Box<dyn FnMut(JsValue)>);
            let read_positions = scroll_positions.clone();
            let read_id = session_id.clone();
            let read = Closure::wrap(Box::new(move || {
                read_positions
                    .borrow()
                    .get(&read_id)
                    .cloned()
                    .unwrap_or(JsValue::NULL)
            }) as Box<dyn FnMut() -> JsValue>);
            let fork_sessions = inject_sessions.clone();
            let fork_id = session_id.clone();
            let fork = Closure::wrap(Box::new(move |seq: JsValue| -> Result<(), JsValue> {
                let options = object(&[
                    ("sessionId", JsValue::from_str(&fork_id)),
                    ("atSeq", seq),
                    ("increaseTitle", JsValue::TRUE),
                ])?;
                let returned = call_method(&fork_sessions, "fork", &[options.into()])?;
                let sessions = fork_sessions.clone();
                let opened = Closure::wrap(Box::new(move |id: JsValue| {
                    let _ = call_method(&sessions, "open", &[id]);
                }) as Box<dyn FnMut(JsValue)>);
                let promise = Promise::resolve(&returned);
                let _ = promise.then(&opened);
                let swallow =
                    Closure::wrap(Box::new(move |_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
                let _ = promise.catch(&swallow);
                drop(opened.into_js_value());
                drop(swallow.into_js_value());
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            object(&[
                ("openDetails", open_details.into_js_value()),
                ("fileMentions", file_mentions.into_js_value()),
                ("openFile", open_file.into_js_value()),
                ("loadOlder", load_older.into_js_value()),
                ("loadImage", load_image.into_js_value()),
                ("inspectCall", inspect.into_js_value()),
                (
                    "chatScroll",
                    object(&[
                        ("save", save.into_js_value()),
                        ("read", read.into_js_value()),
                    ])?
                    .into(),
                ),
                ("forkAt", fork.into_js_value()),
            ])
            .map(Into::into)
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<JsValue, JsValue>>);
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("conversation.view")),
            ("id", JsValue::from_str("chat")),
            ("order", JsValue::from_f64(0.0)),
            ("label", label.into_js_value()),
            ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
            (
                "children",
                object(&[("conversation.chat.node", chat_node_child_inject()?.into())])?.into(),
            ),
            ("store", store.clone()),
            ("inject", inject.into_js_value()),
        ])?,
        required(components, "ChatView", "conversation apply components")?,
    )
}

fn register_details(
    slots: &JsValue,
    components: &JsValue,
    store: &JsValue,
    layout: &JsValue,
) -> Result<(), JsValue> {
    let inject_layout = layout.clone();
    let inject = Closure::wrap(Box::new(
        move |_id: JsValue, _actions: JsValue| -> Result<JsValue, JsValue> {
            let layout = inject_layout.clone();
            let close = Closure::wrap(Box::new(move || call_method(&layout, "closeDetails", &[]))
                as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
            object(&[("closeDetails", close.into_js_value())]).map(Into::into)
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>);
    register(
        slots,
        object(&[
            ("name", JsValue::from_str("details")),
            ("locale", JsValue::from_str(CONVERSATION_LOCALE_NAMESPACE)),
            (
                "children",
                object(&[(
                    "conversation.details.tool",
                    child("single", "session")?.into(),
                )])?
                .into(),
            ),
            ("store", store.clone()),
            ("inject", inject.into_js_value()),
        ])?,
        required(components, "DetailsPanel", "conversation apply components")?,
    )
}

fn configured_components() -> Result<JsValue, JsValue> {
    APPLY_COMPONENTS.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation apply was not configured").into()
        })
    })
}

fn view_tabs_face(slots: &JsValue) -> Result<JsValue, JsValue> {
    let list_slots = slots.clone();
    let list = Closure::wrap(Box::new(move || -> Result<Array, JsValue> {
        let entries = call_method(
            &list_slots,
            "entries",
            &[JsValue::from_str("conversation.view")],
        )?;
        let tabs = Array::new();
        for entry in Array::from(&entries).iter() {
            let options = required(&entry, "options", "Slot entry")?;
            let id = Reflect::get(&options, &JsValue::from_str("id"))?;
            let Some(id_string) = id.as_string() else {
                continue;
            };
            let raw_label = Reflect::get(&options, &JsValue::from_str("label"))?;
            let label = if raw_label.is_function() {
                raw_label
                    .dyn_into::<Function>()?
                    .call0(&JsValue::UNDEFINED)?
                    .as_string()
                    .unwrap_or_else(|| id_string.clone())
            } else {
                raw_label.as_string().unwrap_or_else(|| id_string.clone())
            };
            tabs.push(
                &object(&[
                    ("id", JsValue::from_str(&id_string)),
                    ("label", JsValue::from_str(&label)),
                ])?
                .into(),
            );
        }
        Ok(tabs)
    }) as Box<dyn FnMut() -> Result<Array, JsValue>>);
    let subscribe_slots = slots.clone();
    let subscribe = Closure::wrap(Box::new(move |listener: Function| {
        call_method(
            &subscribe_slots,
            "subscribe",
            &[JsValue::from_str("conversation.view"), listener.into()],
        )
    })
        as Box<dyn FnMut(Function) -> Result<JsValue, JsValue>>);
    let version_slots = slots.clone();
    let version = Closure::wrap(Box::new(move || {
        call_method(
            &version_slots,
            "getVersion",
            &[JsValue::from_str("conversation.view")],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    object(&[
        ("list", list.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
        ("version", version.into_js_value()),
    ])
    .map(Into::into)
}

fn root_children() -> Result<Object, JsValue> {
    object(&[
        ("conversation.session", child("single", "session")?.into()),
        (
            "conversation.session.header",
            child("single", "session")?.into(),
        ),
        ("conversation.composer", child("chain", "session")?.into()),
        (
            "conversation.composer.bar",
            child("single", "session-maybe")?.into(),
        ),
        (
            "conversation.input.overlay",
            child("list", "session")?.into(),
        ),
        ("conversation.input.dock", child("list", "session")?.into()),
        (
            "conversation.composer.dock",
            child("list", "session")?.into(),
        ),
        ("conversation.input.left", child("list", "session")?.into()),
        ("conversation.input.right", child("list", "session")?.into()),
        (
            "conversation.hero.workspace",
            child("single", "root")?.into(),
        ),
        (
            "conversation.hero.agentPreset",
            child("single", "root")?.into(),
        ),
    ])
}

fn chat_node_child_inject() -> Result<Object, JsValue> {
    let turn_data = Closure::wrap(Box::new(
        move |owner: JsValue, node_key: String| -> Result<JsValue, JsValue> {
            let use_session = required(&owner, "useSession", "Chat node owner")?;
            let hook = Closure::wrap(Box::new(move |key: String| -> Result<JsValue, JsValue> {
                let node_key = node_key.clone();
                let key = key.clone();
                let selector = Closure::wrap(Box::new(
                    move |snapshot: JsValue| -> Result<JsValue, JsValue> {
                        let chat = required(&snapshot, "chat", "Session snapshot")?;
                        let nodes = required(&chat, "nodes", "Chat snapshot")?;
                        let node = call_method(&nodes, "get", &[JsValue::from_str(&node_key)])?;
                        if node.is_null() || node.is_undefined() {
                            return Ok(JsValue::UNDEFINED);
                        }
                        let location = required(&node, "location", "Chat node")?;
                        let kind = required_string(&location, "kind", "Conversation location")?;
                        if !matches!(kind.as_str(), "turn" | "step") {
                            return Ok(JsValue::UNDEFINED);
                        }
                        let turn = required(&location, "turn", "Conversation location")?;
                        let data = required(&turn, "data", "Turn location")?;
                        call_method(&data, "get", &[JsValue::from_str(&key)])
                    },
                )
                    as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
                use_session
                    .clone()
                    .dyn_into::<Function>()?
                    .call1(&owner, &selector.into_js_value())
            })
                as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
            Ok(hook.into_js_value())
        },
    )
        as Box<dyn FnMut(JsValue, String) -> Result<JsValue, JsValue>>);
    object(&[
        ("kind", JsValue::from_str("keyed")),
        ("scope", JsValue::from_str("session")),
        (
            "inject",
            object(&[(
                "hooks",
                object(&[("turnData", turn_data.into_js_value())])?.into(),
            )])?
            .into(),
        ),
    ])
}

fn move_draft(input: &JsValue, from: &str, to: &str) -> Result<(), JsValue> {
    let from_shell = call_method(input, "shell", &[JsValue::from_str(from)])?;
    let next_shell = call_method(input, "shell", &[JsValue::from_str(to)])?;
    let snapshot = required(&from_shell, "snapshot", "SessionInput shell")?;
    let draft = required_string(&snapshot, "draft", "InputState")?;
    let image_ids = required(&snapshot, "imageIds", "InputState")?;
    let images = Array::from(&image_ids);
    let accepted = images.length() == 0
        || call_method(&next_shell, "addImages", std::slice::from_ref(&image_ids))?
            .as_bool()
            .unwrap_or(false);
    if !accepted {
        return Ok(());
    }
    if !draft.is_empty() {
        call_method(&next_shell, "setDraft", &[JsValue::from_str(&draft)])?;
        call_method(&from_shell, "setDraft", &[JsValue::from_str("")])?;
    }
    for image_id in images.iter() {
        call_method(&from_shell, "removeImage", &[image_id])?;
    }
    Ok(())
}

fn scoped_controller(
    sessions: &JsValue,
    controller: &JsValue,
    id: &str,
) -> Result<JsValue, JsValue> {
    let actx = call_method(sessions, "scope", &[JsValue::from_str(id)])?;
    if actx.is_null() || actx.is_undefined() {
        return Err(js_sys::Error::new(&format!(
            "ui-conversation: session {id:?} resolved no scope"
        ))
        .into());
    }
    call_method(controller, "forContext", &[actx])
}

fn session_cwd(sessions: &JsValue, id: &str) -> Result<Option<String>, JsValue> {
    let list = required(sessions, "list", "Sessions service")?;
    let snapshot = call_method(&list, "getSnapshot", &[])?;
    let by_id = required(&snapshot, "byId", "Sessions list snapshot")?;
    let summary = Reflect::get(&by_id, &JsValue::from_str(id))?;
    if summary.is_null() || summary.is_undefined() {
        return Ok(None);
    }
    Ok(Reflect::get(&summary, &JsValue::from_str("cwd"))?.as_string())
}

fn empty_store(value: JsValue) -> Result<JsValue, JsValue> {
    let snapshot = value;
    let get = Closure::wrap(Box::new(move || snapshot.clone()) as Box<dyn FnMut() -> JsValue>);
    let subscribe = Closure::wrap(Box::new(move |_listener: Function| -> JsValue {
        Closure::wrap(Box::new(move || {}) as Box<dyn FnMut()>).into_js_value()
    }) as Box<dyn FnMut(Function) -> JsValue>);
    object(&[
        ("getSnapshot", get.into_js_value()),
        ("subscribe", subscribe.into_js_value()),
    ])
    .map(Into::into)
}

fn child(kind: &str, scope: &str) -> Result<Object, JsValue> {
    object(&[
        ("kind", JsValue::from_str(kind)),
        ("scope", JsValue::from_str(scope)),
    ])
}

fn register(slots: &JsValue, options: Object, component: JsValue) -> Result<(), JsValue> {
    call_method(slots, "register", &[options.into(), component])?;
    Ok(())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, name, "object")?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(value)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required_u32(value: &JsValue, key: &str, owner: &str) -> Result<u32, JsValue> {
    let value = required(value, key, owner)?;
    let number = value
        .as_f64()
        .filter(|number| {
            number.is_finite()
                && *number >= 0.0
                && *number <= f64::from(u32::MAX)
                && number.fract() == 0.0
        })
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key:?} must be a u32")))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as u32)
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
