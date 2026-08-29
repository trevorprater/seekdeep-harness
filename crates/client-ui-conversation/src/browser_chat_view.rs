//! Compiled keyed chat flow, paging anchors, and scroll-follow ownership.

use std::cell::RefCell;

use js_sys::{Array, Date, Function, Math, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{browser_message_chrome::format_run_duration_browser, browser_reasoning::inject_style};

const CHAT_CSS: &str =
    include_str!("../../../packages/client/ui-conversation/src/client/chat/ChatView.module.css");
const FOLLOW_THRESHOLD: f64 = 24.0;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
    static COMPONENTS: RefCell<Option<ChatComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    chevron_down: JsValue,
    chat_node_seat: JsValue,
    pending_steering: JsValue,
}

#[derive(Clone)]
struct ChatComponents {
    chat_view: JsValue,
    turn_status: JsValue,
}

struct ScrollRefs {
    list: JsValue,
    at_bottom: JsValue,
    observed_top: JsValue,
    anchor: JsValue,
}

struct LayoutState {
    open_state: String,
    first_seq: Option<f64>,
    last_key: Option<String>,
    last_node: JsValue,
    last_steering_id: Option<String>,
    follow_sig: String,
    list_ref: JsValue,
    at_bottom_ref: JsValue,
    observed_top_ref: JsValue,
    anchor_ref: JsValue,
    first_seq_ref: JsValue,
    opened_ref: JsValue,
    last_key_ref: JsValue,
    last_steering_id_ref: JsValue,
    follow_sig_ref: JsValue,
    set_at_bottom: Function,
    chat_scroll: JsValue,
}

/// Configures the compiled `ChatView` and its turn-status child.
///
/// # Errors
///
/// Returns on missing React/hooks, primitive/dependency faces, or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationChatView)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_chat_view(
    react: JsValue,
    ui_primitives: JsValue,
    dependencies: JsValue,
) -> Result<(), JsValue> {
    for method in [
        "createElement",
        "useEffect",
        "useLayoutEffect",
        "useMemo",
        "useRef",
        "useState",
    ] {
        required_function(&react, method, "React")?;
    }
    let modules = BrowserModules {
        chevron_down: required_property(
            &ui_primitives,
            "IconChevronDownOutline14",
            "ui-primitives",
        )?,
        chat_node_seat: required_property(&dependencies, "ChatNodeSeat", "ChatView dependencies")?,
        pending_steering: required_property(
            &dependencies,
            "PendingSteeringBubble",
            "ChatView dependencies",
        )?,
        react,
    };
    inject_chat_styles()?;
    MODULES.with(|configured| *configured.borrow_mut() = Some(modules.clone()));
    let status_modules = modules.clone();
    let turn_status = raw_component(move |props| render_turn_status(&status_modules, props));
    let view_modules = modules;
    let status_for_view = turn_status.clone();
    let chat_view =
        raw_component(move |props| render_chat_view(&view_modules, &status_for_view, props));
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(ChatComponents {
            chat_view,
            turn_status,
        });
    });
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

/// Returns the compiled `ChatView` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = chatViewComponent)]
pub fn chat_view_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.chat_view)
}

/// Returns the compiled internal turn-status component for assembly tests.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = turnStatusComponent)]
pub fn turn_status_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.turn_status)
}

#[allow(clippy::too_many_lines)]
fn render_turn_status(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let start_time = Reflect::get(props, &JsValue::from_str("startTime"))?;
    let mounted_initializer = Closure::wrap(Box::new(Date::now) as Box<dyn FnMut() -> f64>);
    let mounted_state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &mounted_initializer.into_js_value())?
        .dyn_into::<Array>()?;
    let mounted_at = mounted_state
        .get(0)
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("TurnStatus mountedAt must be a number"))?;
    let anchor = if start_time.is_null() {
        mounted_at
    } else {
        start_time
            .as_f64()
            .ok_or_else(|| js_sys::TypeError::new("TurnStatus startTime must be number or null"))?
    };
    let initial_anchor = anchor;
    let elapsed_initializer =
        Closure::wrap(
            Box::new(move || Math::max(0.0, Date::now() - initial_anchor))
                as Box<dyn FnMut() -> f64>,
        );
    let elapsed_state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &elapsed_initializer.into_js_value())?
        .dyn_into::<Array>()?;
    let elapsed_ms = elapsed_state
        .get(0)
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new("TurnStatus elapsed state must be number"))?;
    let set_elapsed = elapsed_state.get(1).dyn_into::<Function>()?;
    install_turn_status_effect(&modules.react, anchor, &set_elapsed)?;
    let mut children = vec![JsValue::from_str("Deep diving...")];
    if elapsed_ms >= 15_000.0 {
        let translate = required_function(props, "t", "TurnStatus props")?;
        children.push(create_element(
            &modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-chat-turnStatusClock"),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[format_run_duration_browser(elapsed_ms, translate)?],
        )?);
    }
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-conversation-chat-turnStatus"),
            ),
            ("role", JsValue::from_str("status")),
            ("aria-live", JsValue::from_str("polite")),
        ])?),
        &children,
    )
}

fn install_turn_status_effect(
    react: &JsValue,
    anchor: f64,
    set_elapsed: &Function,
) -> Result<(), JsValue> {
    let setter = set_elapsed.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let tick_setter = setter.clone();
        let tick = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            tick_setter.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_f64(Math::max(0.0, Date::now() - anchor)),
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        tick.unchecked_ref::<Function>()
            .call0(&JsValue::UNDEFINED)?;
        let timer = set_interval(&tick, 1_000.0)?;
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            clear_interval(&timer)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        Ok(cleanup)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_f64(anchor)),
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn render_chat_view(
    modules: &BrowserModules,
    turn_status: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let order = select_session(props, "chat", Some("order"))?.dyn_into::<Array>()?;
    let node_store = select_session(props, "chat", Some("nodes"))?;
    let timeline = select_session(props, "chat", Some("timeline"))?;
    let inbox = select_session(props, "queue", None)?.dyn_into::<Array>()?;
    let session_id = required_property(props, "sessionId", "ChatView props")?;
    let cwd = select_cwd(props, &session_id)?;
    let running = required_bool(&select_session(props, "running", None)?, "session running")?;
    let open_state = required_string_value(
        &select_session(props, "openState", None)?,
        "session openState",
    )?;
    let open_error = select_session(props, "openError", None)?;
    let has_more = required_bool(&select_session(props, "hasMore", None)?, "session hasMore")?;
    let loading_older = required_bool(
        &select_session(props, "loadingOlder", None)?,
        "session loadingOlder",
    )?;
    let selected_call_id = select_store_call(props)?;
    let pending_inbox = inbox.clone();
    let pending_factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let predicate = Closure::wrap(Box::new(move |item: JsValue| -> Result<bool, JsValue> {
            Ok(Reflect::get(&item, &JsValue::from_str("placement"))?
                .as_string()
                .as_deref()
                == Some("steering"))
        })
            as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>)
        .into_js_value();
        call_method(&pending_inbox, "filter", &[predicate])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let pending = required_function(&modules.react, "useMemo", "React")?
        .call2(
            &modules.react,
            &pending_factory.into_js_value(),
            &Array::of1(inbox.as_ref()),
        )?
        .dyn_into::<Array>()?;
    let timeline_value = timeline.clone();
    let running_factory = Closure::wrap(Box::new(move || running_turn_start_time(&timeline_value))
        as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let running_turn_start = required_function(&modules.react, "useMemo", "React")?.call2(
        &modules.react,
        &running_factory.into_js_value(),
        &Array::of1(&timeline),
    )?;

    let list_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let column_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let at_bottom_ref = use_ref(&modules.react, &JsValue::TRUE)?;
    let at_bottom_state = required_function(&modules.react, "useState", "React")?
        .call1(&modules.react, &JsValue::TRUE)?
        .dyn_into::<Array>()?;
    let at_bottom = required_bool(&at_bottom_state.get(0), "ChatView atBottom state")?;
    let set_at_bottom = at_bottom_state.get(1).dyn_into::<Function>()?;
    let observed_top_ref = use_ref(&modules.react, &JsValue::from_f64(0.0))?;
    let anchor_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let first_seq_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let opened_ref = use_ref(&modules.react, &JsValue::FALSE)?;
    let last_key_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let last_steering_id_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let follow_sig_ref = use_ref(&modules.react, &JsValue::NULL)?;

    let first_key = order.get(0);
    let first_node = if first_key.is_undefined() {
        JsValue::UNDEFINED
    } else {
        call_method(&node_store, "get", &[first_key])?
    };
    let first_seq = if first_node.is_undefined() {
        None
    } else {
        Some(numeric_property(
            &first_node,
            "anchorSeq",
            "first chat node",
        )?)
    };
    let last_key_value = if order.length() == 0 {
        JsValue::NULL
    } else {
        order.get(order.length() - 1)
    };
    let last_key = last_key_value.as_string();
    let last_node = if last_key_value.is_null() {
        JsValue::UNDEFINED
    } else {
        call_method(&node_store, "get", std::slice::from_ref(&last_key_value))?
    };
    let last_steering_id = if pending.length() == 0 {
        None
    } else {
        Reflect::get(&pending.get(pending.length() - 1), &JsValue::from_str("id"))?.as_string()
    };
    let follow_sig = format!(
        "{}:{}:{}:{}:{}:{}",
        open_state,
        first_seq.map_or_else(|| "null".to_owned(), number_string),
        last_key.as_deref().unwrap_or("null"),
        order.length(),
        u8::from(running),
        last_steering_id.as_deref().unwrap_or_default()
    );
    let chat_scroll = required_property(props, "chatScroll", "ChatView props")?;
    install_layout_effect(
        &modules.react,
        LayoutState {
            open_state: open_state.clone(),
            first_seq,
            last_key: last_key.clone(),
            last_node: last_node.clone(),
            last_steering_id: last_steering_id.clone(),
            follow_sig: follow_sig.clone(),
            list_ref: list_ref.clone(),
            at_bottom_ref: at_bottom_ref.clone(),
            observed_top_ref: observed_top_ref.clone(),
            anchor_ref: anchor_ref.clone(),
            first_seq_ref: first_seq_ref.clone(),
            opened_ref: opened_ref.clone(),
            last_key_ref: last_key_ref.clone(),
            last_steering_id_ref: last_steering_id_ref.clone(),
            follow_sig_ref: follow_sig_ref.clone(),
            set_at_bottom: set_at_bottom.clone(),
            chat_scroll: chat_scroll.clone(),
        },
    )?;

    let noop = Closure::wrap(Box::new(move || {}) as Box<dyn FnMut()>).into_js_value();
    let on_scroll_ref = use_ref(&modules.react, &noop)?;
    install_on_scroll_callback(
        &on_scroll_ref,
        ScrollRefs {
            list: list_ref.clone(),
            at_bottom: at_bottom_ref.clone(),
            observed_top: observed_top_ref.clone(),
            anchor: anchor_ref.clone(),
        },
        &set_at_bottom,
        &chat_scroll,
    )?;
    install_scroll_listener(&modules.react, &list_ref, &on_scroll_ref)?;

    let follow_ref = use_ref(&modules.react, &JsValue::NULL)?;
    install_follow_callback(
        &follow_ref,
        &list_ref,
        &at_bottom_ref,
        &observed_top_ref,
        &chat_scroll,
    )?;
    install_resize_follow(&modules.react, &column_ref, &list_ref, &follow_ref)?;
    install_loading_effect(&modules.react, loading_older, &anchor_ref)?;
    let load_older = required_function(props, "loadOlder", "ChatView props")?;
    let load_older_anchored = load_older_callback(&list_ref, &anchor_ref, &load_older)?;

    let translate = required_function(props, "t", "ChatView props")?;
    let mut flow_children = Vec::new();
    if open_state == "loading" {
        flow_children.push(div_text(
            modules,
            "seekdeep-conversation-chat-hint",
            translate.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("chat.loadingHistory"),
            )?,
        )?);
    }
    if open_state == "error" && !open_error.is_null() {
        flow_children.push(div_text(
            modules,
            "seekdeep-conversation-chat-openError",
            translate.apply(
                &JsValue::UNDEFINED,
                &Array::of2(
                    &JsValue::from_str("chat.loadError"),
                    object(&[
                        (
                            "message",
                            required_property(&open_error, "message", "open error")?,
                        ),
                        (
                            "code",
                            required_property(&open_error, "code", "open error")?,
                        ),
                    ])?
                    .as_ref(),
                ),
            )?,
        )?);
    }
    if has_more {
        let label_key = if loading_older {
            "loading"
        } else {
            "chat.loadOlder"
        };
        let button = create_element(
            &modules.react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("disabled", JsValue::from_bool(loading_older)),
                ("onClick", load_older_anchored.into()),
            ])?),
            &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(label_key))?],
        )?;
        flow_children.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&class_props("seekdeep-conversation-chat-older")?),
            &[button],
        )?);
    }
    for index in 0..order.length() {
        let node_key = order.get(index);
        flow_children.push(create_element(
            &modules.react,
            &modules.chat_node_seat,
            Some(&object(&[
                ("key", node_key.clone()),
                ("nodeKey", node_key),
                (
                    "useSession",
                    required_property(props, "useSession", "ChatView props")?,
                ),
                ("selectedCallId", selected_call_id.clone()),
                ("cwd", cwd.clone()),
                (
                    "openFile",
                    required_property(props, "openFile", "ChatView props")?,
                ),
                (
                    "inspectCall",
                    required_property(props, "inspectCall", "ChatView props")?,
                ),
                (
                    "forkAt",
                    required_property(props, "forkAt", "ChatView props")?,
                ),
                (
                    "loadImage",
                    required_property(props, "loadImage", "ChatView props")?,
                ),
                (
                    "fileMentions",
                    required_property(props, "fileMentions", "ChatView props")?,
                ),
                (
                    "renderSlot",
                    required_property(props, "renderSlot", "ChatView props")?,
                ),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?);
    }
    if running {
        flow_children.push(create_element(
            &modules.react,
            turn_status,
            Some(&object(&[
                ("startTime", running_turn_start),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?);
    }
    for index in 0..pending.length() {
        let item = pending.get(index);
        flow_children.push(create_element(
            &modules.react,
            &modules.pending_steering,
            Some(&object(&[
                ("key", required_property(&item, "id", "pending steering")?),
                (
                    "content",
                    required_property(&item, "content", "pending steering")?,
                ),
                (
                    "loadImage",
                    required_property(props, "loadImage", "ChatView props")?,
                ),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?);
    }
    let column = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", column_ref),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-chat-column"),
            ),
            ("data-chat-flow", JsValue::from_str("")),
        ])?),
        &flow_children,
    )?;
    let mut scroll_children = vec![column];
    if !at_bottom {
        let click_refs = ScrollRefs {
            list: list_ref.clone(),
            at_bottom: at_bottom_ref.clone(),
            observed_top: observed_top_ref.clone(),
            anchor: anchor_ref.clone(),
        };
        let click_setter = set_at_bottom;
        let click_scroll = chat_scroll;
        let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let local = current(&click_refs.list)?;
            if !local.is_null() {
                to_bottom(
                    &scroller_of(&local)?,
                    &click_refs,
                    &click_setter,
                    &click_scroll,
                )?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let button = create_element(
            &modules.react,
            &JsValue::from_str("button"),
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-chat-toBottom"),
                ),
                (
                    "aria-label",
                    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("chat.toBottom"))?,
                ),
                ("onClick", click),
            ])?),
            &[create_element(
                &modules.react,
                &modules.chevron_down,
                None,
                &[],
            )?],
        )?;
        scroll_children.push(create_element(
            &modules.react,
            &JsValue::from_str("div"),
            Some(&class_props("seekdeep-conversation-chat-toBottomSlot")?),
            &[button],
        )?);
    }
    let scroll = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", list_ref),
            (
                "className",
                JsValue::from_str("seekdeep-conversation-chat-scroll"),
            ),
        ])?),
        &scroll_children,
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-chat-root")?),
        &[scroll],
    )
}

fn select_session(
    props: &JsValue,
    first: &'static str,
    second: Option<&'static str>,
) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(
        Box::new(move |snapshot: JsValue| -> Result<JsValue, JsValue> {
            let value = Reflect::get(&snapshot, &JsValue::from_str(first))?;
            second.map_or(Ok(value.clone()), |key| {
                Reflect::get(&value, &JsValue::from_str(key))
            })
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    required_function(props, "useSession", "ChatView props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn select_cwd(props: &JsValue, session_id: &JsValue) -> Result<JsValue, JsValue> {
    let selected = session_id.clone();
    let selector = Closure::wrap(
        Box::new(move |sessions: JsValue| -> Result<JsValue, JsValue> {
            let by_id = Reflect::get(&sessions, &JsValue::from_str("byId"))?;
            let row = Reflect::get(&by_id, &selected)?;
            if row.is_undefined() {
                Ok(JsValue::UNDEFINED)
            } else {
                Reflect::get(&row, &JsValue::from_str("cwd"))
            }
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    required_function(props, "useSessions", "ChatView props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn select_store_call(props: &JsValue) -> Result<JsValue, JsValue> {
    let selector = Closure::wrap(Box::new(move |store: JsValue| -> Result<JsValue, JsValue> {
        let selection = Reflect::get(&store, &JsValue::from_str("selection"))?;
        if selection.is_null() || selection.is_undefined() {
            Ok(JsValue::UNDEFINED)
        } else {
            Reflect::get(&selection, &JsValue::from_str("callId"))
        }
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    required_function(props, "useStore", "ChatView props")?
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

fn running_turn_start_time(timeline: &JsValue) -> Result<JsValue, JsValue> {
    let turns = required_property(timeline, "turns", "chat timeline")?;
    let values = call_method(&turns, "values", &[])?;
    let iterator = js_sys::try_iter(&values)?
        .ok_or_else(|| js_sys::TypeError::new("timeline turns values must be iterable"))?;
    let mut latest = JsValue::NULL;
    for item in iterator {
        let turn = item?;
        if Reflect::get(&turn, &JsValue::from_str("status"))?
            .as_string()
            .as_deref()
            == Some("open")
        {
            let start = Reflect::get(&turn, &JsValue::from_str("start"))?;
            if !start.is_undefined() {
                latest = required_property(&start, "time", "turn start")?;
            }
        }
    }
    Ok(latest)
}

fn install_layout_effect(react: &JsValue, state: LayoutState) -> Result<(), JsValue> {
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        layout_chat(&state)?;
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useLayoutEffect", "React")?.call1(react, &effect.into_js_value())?;
    Ok(())
}

#[allow(clippy::float_cmp, clippy::too_many_lines)]
fn layout_chat(state: &LayoutState) -> Result<(), JsValue> {
    let local = current(&state.list_ref)?;
    if local.is_null() {
        return Ok(());
    }
    let scrollport = scroller_of(&local)?;
    if state.open_state == "open" && !current_bool(&state.opened_ref, "opened ref")? {
        set_current(&state.opened_ref, &JsValue::TRUE)?;
        let saved = required_function(&state.chat_scroll, "read", "chatScroll")?
            .call0(&state.chat_scroll)?;
        let refs = ScrollRefs {
            list: state.list_ref.clone(),
            at_bottom: state.at_bottom_ref.clone(),
            observed_top: state.observed_top_ref.clone(),
            anchor: state.anchor_ref.clone(),
        };
        if saved.is_null() {
            to_bottom(&scrollport, &refs, &state.set_at_bottom, &state.chat_scroll)?;
        } else {
            set_number_property(
                &scrollport,
                "scrollTop",
                numeric_property(&saved, "scrollTop", "saved chat scroll")?,
            )?;
            let anchor_key = required_string(&saved, "anchorKey", "saved chat scroll")?;
            let row = anchor_element(&local, &anchor_key)?;
            if !row.is_null() {
                let delta = flow_top(&row, &scrollport)?
                    - numeric_property(&saved, "anchorTop", "saved chat scroll")?;
                add_scroll_top(&scrollport, delta)?;
            }
            let top = numeric_property(&scrollport, "scrollTop", "scrollport")?;
            set_current(&state.observed_top_ref, &JsValue::from_f64(top))?;
            let is_at_bottom = bottom_distance(&scrollport)? <= FOLLOW_THRESHOLD + 1.0;
            set_current(&state.at_bottom_ref, &JsValue::from_bool(is_at_bottom))?;
            state
                .set_at_bottom
                .call1(&JsValue::UNDEFINED, &JsValue::from_bool(is_at_bottom))?;
            if is_at_bottom {
                save_scroll(&state.chat_scroll, &JsValue::NULL)?;
            } else if let Some(position) = scroll_position(&local, &scrollport)? {
                save_scroll(&state.chat_scroll, position.as_ref())?;
            }
        }
        set_optional_number(&state.first_seq_ref, state.first_seq)?;
        set_optional_string(&state.last_key_ref, state.last_key.as_deref())?;
        set_optional_string(
            &state.last_steering_id_ref,
            state.last_steering_id.as_deref(),
        )?;
        set_current(&state.follow_sig_ref, &JsValue::from_str(&state.follow_sig))?;
        return Ok(());
    }
    let previous_first = current(&state.first_seq_ref)?.as_f64();
    let anchor = current(&state.anchor_ref)?;
    if !anchor.is_null()
        && state.first_seq.is_some()
        && previous_first.is_some()
        && state.first_seq.unwrap_or_default() < previous_first.unwrap_or_default()
    {
        set_current(&state.anchor_ref, &JsValue::NULL)?;
        let key = required_string(&anchor, "key", "paging anchor")?;
        let row = anchor_element(&local, &key)?;
        if !row.is_null() {
            let delta =
                flow_top(&row, &scrollport)? - numeric_property(&anchor, "top", "paging anchor")?;
            add_scroll_top(&scrollport, delta)?;
        }
        let top = numeric_property(&scrollport, "scrollTop", "scrollport")?;
        set_current(&state.observed_top_ref, &JsValue::from_f64(top))?;
        set_optional_number(&state.first_seq_ref, state.first_seq)?;
        set_optional_string(&state.last_key_ref, state.last_key.as_deref())?;
        set_optional_string(
            &state.last_steering_id_ref,
            state.last_steering_id.as_deref(),
        )?;
        set_current(&state.follow_sig_ref, &JsValue::from_str(&state.follow_sig))?;
        return Ok(());
    }
    set_optional_number(&state.first_seq_ref, state.first_seq)?;
    let previous_last_key = current(&state.last_key_ref)?.as_string();
    let previous_steering = current(&state.last_steering_id_ref)?.as_string();
    let previous_sig = current(&state.follow_sig_ref)?.as_string();
    let last_kind = if state.last_node.is_null() || state.last_node.is_undefined() {
        None
    } else {
        Reflect::get(&state.last_node, &JsValue::from_str("kind"))?.as_string()
    };
    let appended_user = state.last_key != previous_last_key && last_kind.as_deref() == Some("user");
    let appended_steering =
        state.last_steering_id.is_some() && state.last_steering_id != previous_steering;
    let tip_moved = previous_sig.as_deref() != Some(state.follow_sig.as_str());
    set_optional_string(&state.last_key_ref, state.last_key.as_deref())?;
    set_optional_string(
        &state.last_steering_id_ref,
        state.last_steering_id.as_deref(),
    )?;
    set_current(&state.follow_sig_ref, &JsValue::from_str(&state.follow_sig))?;
    if appended_user
        || appended_steering
        || (tip_moved && current_bool(&state.at_bottom_ref, "at-bottom ref")?)
    {
        let refs = ScrollRefs {
            list: state.list_ref.clone(),
            at_bottom: state.at_bottom_ref.clone(),
            observed_top: state.observed_top_ref.clone(),
            anchor: state.anchor_ref.clone(),
        };
        to_bottom(&scrollport, &refs, &state.set_at_bottom, &state.chat_scroll)?;
    }
    Ok(())
}

fn install_on_scroll_callback(
    on_scroll_ref: &JsValue,
    refs: ScrollRefs,
    set_at_bottom: &Function,
    chat_scroll: &JsValue,
) -> Result<(), JsValue> {
    let setter = set_at_bottom.clone();
    let scroll = chat_scroll.clone();
    let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        handle_scroll(&refs, &setter, &scroll)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    set_current(on_scroll_ref, &callback)
}

fn handle_scroll(
    refs: &ScrollRefs,
    set_at_bottom: &Function,
    chat_scroll: &JsValue,
) -> Result<(), JsValue> {
    let local = current(&refs.list)?;
    if local.is_null() {
        return Ok(());
    }
    let scrollport = scroller_of(&local)?;
    let scroll_height = numeric_property(&scrollport, "scrollHeight", "scrollport")?;
    let client_height = numeric_property(&scrollport, "clientHeight", "scrollport")?;
    let top = numeric_property(&scrollport, "scrollTop", "scrollport")?;
    let floor = Math::max(0.0, scroll_height - client_height);
    let observed = current_number(&refs.observed_top, "observed-top ref")?;
    let moved_by_reader = Math::abs(top - Math::min(observed, floor)) > 0.5;
    let is_at_bottom = if moved_by_reader {
        floor - top <= FOLLOW_THRESHOLD + 1.0
    } else {
        current_bool(&refs.at_bottom, "at-bottom ref")?
    };
    if !moved_by_reader && is_at_bottom {
        return to_bottom(&scrollport, refs, set_at_bottom, chat_scroll);
    }
    set_current(&refs.at_bottom, &JsValue::from_bool(is_at_bottom))?;
    set_at_bottom.call1(&JsValue::UNDEFINED, &JsValue::from_bool(is_at_bottom))?;
    let position = if is_at_bottom {
        None
    } else {
        scroll_position(&local, &scrollport)?
    };
    if is_at_bottom {
        set_current(&refs.anchor, &JsValue::NULL)?;
    } else if !current(&refs.anchor)?.is_null()
        && let Some(position) = position.as_ref()
    {
        set_current(
            &refs.anchor,
            object(&[
                (
                    "key",
                    required_property(position, "anchorKey", "scroll position")?,
                ),
                (
                    "top",
                    required_property(position, "anchorTop", "scroll position")?,
                ),
            ])?
            .as_ref(),
        )?;
    }
    if is_at_bottom {
        save_scroll(chat_scroll, &JsValue::NULL)?;
    } else if let Some(position) = position.as_ref() {
        save_scroll(chat_scroll, position.as_ref())?;
    }
    set_current(&refs.observed_top, &JsValue::from_f64(top))
}

fn install_scroll_listener(
    react: &JsValue,
    list_ref: &JsValue,
    on_scroll_ref: &JsValue,
) -> Result<(), JsValue> {
    let effect_list = list_ref.clone();
    let effect_handler = on_scroll_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let local = current(&effect_list)?;
        if local.is_null() {
            return Ok(JsValue::UNDEFINED);
        }
        let scrollport = scroller_of(&local)?;
        let handler_ref = effect_handler.clone();
        let handler = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let callback = current(&handler_ref)?.dyn_into::<Function>()?;
            callback.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        call_method(
            &scrollport,
            "addEventListener",
            &[
                JsValue::from_str("scroll"),
                handler.clone(),
                object(&[("passive", JsValue::TRUE)])?.into(),
            ],
        )?;
        let cleanup_port = scrollport;
        let cleanup_handler = handler;
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &cleanup_port,
                "removeEventListener",
                &[JsValue::from_str("scroll"), cleanup_handler.clone()],
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        Ok(cleanup)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::new(),
    )?;
    Ok(())
}

fn install_follow_callback(
    follow_ref: &JsValue,
    list_ref: &JsValue,
    at_bottom_ref: &JsValue,
    observed_top_ref: &JsValue,
    chat_scroll: &JsValue,
) -> Result<(), JsValue> {
    let list = list_ref.clone();
    let at_bottom = at_bottom_ref.clone();
    let observed = observed_top_ref.clone();
    let scroll = chat_scroll.clone();
    let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let local = current(&list)?;
        if !local.is_null() && current_bool(&at_bottom, "at-bottom ref")? {
            let scrollport = scroller_of(&local)?;
            let height = numeric_property(&scrollport, "scrollHeight", "scrollport")?;
            set_number_property(&scrollport, "scrollTop", height)?;
            let top = numeric_property(&scrollport, "scrollTop", "scrollport")?;
            set_current(&observed, &JsValue::from_f64(top))?;
            save_scroll(&scroll, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    set_current(follow_ref, &callback)
}

fn install_resize_follow(
    react: &JsValue,
    column_ref: &JsValue,
    list_ref: &JsValue,
    follow_ref: &JsValue,
) -> Result<(), JsValue> {
    let effect_column = column_ref.clone();
    let effect_list = list_ref.clone();
    let effect_follow = follow_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let column = current(&effect_column)?;
        let local = current(&effect_list)?;
        let constructor = Reflect::get(&js_sys::global(), &JsValue::from_str("ResizeObserver"))?;
        if column.is_null() || local.is_null() || constructor.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let callback_ref = effect_follow.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let current = current(&callback_ref)?;
            if let Ok(function) = current.dyn_into::<Function>() {
                function.call0(&JsValue::UNDEFINED)?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let observer = Reflect::construct(
            &constructor.dyn_into::<Function>()?,
            &Array::of1(callback.as_ref()),
        )?;
        call_method(&observer, "observe", &[column])?;
        let scrollport = scroller_of(&local)?;
        let composer = query_selector(&scrollport, "[data-composer-seat]")?;
        if !composer.is_null() {
            call_method(&observer, "observe", &[composer])?;
        }
        let cleanup_observer = observer;
        let cleanup = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(&cleanup_observer, "disconnect", &[])?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        Ok(cleanup)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::new(),
    )?;
    Ok(())
}

fn install_loading_effect(
    react: &JsValue,
    loading_older: bool,
    anchor_ref: &JsValue,
) -> Result<(), JsValue> {
    let anchor = anchor_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !loading_older {
            set_current(&anchor, &JsValue::NULL)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    required_function(react, "useEffect", "React")?.call2(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_bool(loading_older)),
    )?;
    Ok(())
}

fn load_older_callback(
    list_ref: &JsValue,
    anchor_ref: &JsValue,
    load_older: &Function,
) -> Result<Function, JsValue> {
    let list = list_ref.clone();
    let anchor = anchor_ref.clone();
    let load = load_older.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let local = current(&list)?;
        if !local.is_null() {
            let scrollport = scroller_of(&local)?;
            let row = paging_anchor(&local, &scrollport)?;
            if !row.is_null() {
                let key = dataset_value(&row, "chatAnchorKey")?;
                if !key.is_undefined() {
                    set_current(
                        &anchor,
                        object(&[
                            ("key", key),
                            ("top", JsValue::from_f64(flow_top(&row, &scrollport)?)),
                        ])?
                        .as_ref(),
                    )?;
                }
            }
        }
        load.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .dyn_into()
}

fn to_bottom(
    scrollport: &JsValue,
    refs: &ScrollRefs,
    set_at_bottom: &Function,
    chat_scroll: &JsValue,
) -> Result<(), JsValue> {
    set_current(&refs.anchor, &JsValue::NULL)?;
    let height = numeric_property(scrollport, "scrollHeight", "scrollport")?;
    set_number_property(scrollport, "scrollTop", height)?;
    let top = numeric_property(scrollport, "scrollTop", "scrollport")?;
    set_current(&refs.observed_top, &JsValue::from_f64(top))?;
    set_current(&refs.at_bottom, &JsValue::TRUE)?;
    set_at_bottom.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
    save_scroll(chat_scroll, &JsValue::NULL)
}

fn save_scroll(chat_scroll: &JsValue, position: &JsValue) -> Result<(), JsValue> {
    required_function(chat_scroll, "save", "chatScroll")?.call1(chat_scroll, position)?;
    Ok(())
}

fn scroll_position(list: &JsValue, scrollport: &JsValue) -> Result<Option<Object>, JsValue> {
    let row = paging_anchor(list, scrollport)?;
    if row.is_null() {
        return Ok(None);
    }
    let key = dataset_value(&row, "chatAnchorKey")?;
    if key.is_undefined() {
        return Ok(None);
    }
    Ok(Some(object(&[
        ("anchorKey", key),
        ("anchorTop", JsValue::from_f64(flow_top(&row, scrollport)?)),
        (
            "scrollTop",
            JsValue::from_f64(numeric_property(scrollport, "scrollTop", "scrollport")?),
        ),
    ])?))
}

fn paging_anchor(list: &JsValue, scrollport: &JsValue) -> Result<JsValue, JsValue> {
    let viewport = rect(scrollport)?;
    let composer = query_selector(scrollport, "[data-composer-seat]")?;
    let visible_bottom = if composer.is_null() {
        numeric_property(&viewport, "bottom", "scrollport rect")?
    } else {
        numeric_property(&rect(&composer)?, "top", "composer rect")?
    };
    let viewport_top = numeric_property(&viewport, "top", "scrollport rect")?;
    let document = required_property(&js_sys::global(), "document", "global")?;
    let elements_from_point = Reflect::get(&document, &JsValue::from_str("elementsFromPoint"))?;
    if elements_from_point.is_function() && visible_bottom > viewport_top {
        let content = rect(list)?;
        let left = Math::max(
            numeric_property(&viewport, "left", "scrollport rect")?,
            numeric_property(&content, "left", "list rect")?,
        );
        let right = Math::min(
            numeric_property(&viewport, "right", "scrollport rect")?,
            numeric_property(&content, "right", "list rect")?,
        );
        let x = left + Math::max(0.0, right - left) / 2.0;
        let height = visible_bottom - viewport_top;
        let points = [
            1.0,
            Math::min(32.0, height / 3.0),
            height / 2.0,
            Math::max(1.0, height - 1.0),
        ];
        for offset in points {
            let elements = elements_from_point
                .dyn_ref::<Function>()
                .ok_or_else(|| js_sys::TypeError::new("elementsFromPoint must be function"))?
                .apply(
                    &document,
                    &Array::of2(
                        &JsValue::from_f64(x),
                        &JsValue::from_f64(viewport_top + offset),
                    ),
                )?
                .dyn_into::<Array>()?;
            for index in 0..elements.length() {
                let element = elements.get(index);
                let row = call_method(
                    &element,
                    "closest",
                    &[JsValue::from_str("[data-chat-anchor-key]")],
                )?;
                if !row.is_null()
                    && call_method(list, "contains", std::slice::from_ref(&row))?.as_bool()
                        == Some(true)
                {
                    return Ok(row);
                }
            }
        }
    }
    let rows = query_selector_all(list, "[data-chat-anchor-key]")?;
    let mut first = JsValue::NULL;
    for row in rows {
        if first.is_null() {
            first = row.clone();
        }
        let row_rect = rect(&row)?;
        if numeric_property(&row_rect, "bottom", "row rect")? > viewport_top
            && numeric_property(&row_rect, "top", "row rect")? < visible_bottom
        {
            return Ok(row);
        }
    }
    Ok(first)
}

fn anchor_element(list: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    for row in query_selector_all(list, "[data-chat-anchor-key]")? {
        if dataset_value(&row, "chatAnchorKey")?.as_string().as_deref() == Some(key) {
            return Ok(row);
        }
    }
    Ok(JsValue::NULL)
}

fn flow_top(row: &JsValue, scrollport: &JsValue) -> Result<f64, JsValue> {
    Ok(numeric_property(&rect(row)?, "top", "row rect")?
        - numeric_property(&rect(scrollport)?, "top", "scrollport rect")?)
}

fn scroller_of(from: &JsValue) -> Result<JsValue, JsValue> {
    let closest = call_method(
        from,
        "closest",
        &[JsValue::from_str("[data-conversation-scroll]")],
    )?;
    Ok(if closest.is_null() {
        from.clone()
    } else {
        closest
    })
}

fn bottom_distance(scrollport: &JsValue) -> Result<f64, JsValue> {
    Ok(numeric_property(scrollport, "scrollHeight", "scrollport")?
        - numeric_property(scrollport, "scrollTop", "scrollport")?
        - numeric_property(scrollport, "clientHeight", "scrollport")?)
}

fn query_selector(value: &JsValue, selector: &str) -> Result<JsValue, JsValue> {
    call_method(value, "querySelector", &[JsValue::from_str(selector)])
}

fn query_selector_all(value: &JsValue, selector: &str) -> Result<Vec<JsValue>, JsValue> {
    let list = call_method(value, "querySelectorAll", &[JsValue::from_str(selector)])?;
    let length = numeric_property(&list, "length", "NodeList")?;
    let length = number_to_u32(length, "NodeList length")?;
    let mut rows = Vec::new();
    for index in 0..length {
        rows.push(call_method(
            &list,
            "item",
            &[JsValue::from_f64(f64::from(index))],
        )?);
    }
    Ok(rows)
}

fn rect(value: &JsValue) -> Result<JsValue, JsValue> {
    call_method(value, "getBoundingClientRect", &[])
}

fn dataset_value(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let dataset = required_property(value, "dataset", "element")?;
    Reflect::get(&dataset, &JsValue::from_str(key))
}

fn div_text(modules: &BrowserModules, class_name: &str, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props(class_name)?),
        &[text],
    )
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value)?;
    Ok(())
}

fn current_bool(reference: &JsValue, owner: &str) -> Result<bool, JsValue> {
    required_bool(&current(reference)?, owner)
}

fn current_number(reference: &JsValue, owner: &str) -> Result<f64, JsValue> {
    current(reference)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be number")).into())
}

fn set_optional_number(reference: &JsValue, value: Option<f64>) -> Result<(), JsValue> {
    set_current(reference, &value.map_or(JsValue::NULL, JsValue::from_f64))
}

fn set_optional_string(reference: &JsValue, value: Option<&str>) -> Result<(), JsValue> {
    set_current(reference, &value.map_or(JsValue::NULL, JsValue::from_str))
}

fn add_scroll_top(scrollport: &JsValue, delta: f64) -> Result<(), JsValue> {
    let current = numeric_property(scrollport, "scrollTop", "scrollport")?;
    set_number_property(scrollport, "scrollTop", current + delta)
}

fn set_number_property(value: &JsValue, key: &str, number: f64) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), &JsValue::from_f64(number))?;
    Ok(())
}

fn inject_chat_styles() -> Result<(), JsValue> {
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
    )
}

fn set_interval(callback: &JsValue, delay: f64) -> Result<JsValue, JsValue> {
    let window = browser_window()?;
    required_function(&window, "setInterval", "window")?
        .apply(&window, &Array::of2(callback, &JsValue::from_f64(delay)))
}

fn clear_interval(timer: &JsValue) -> Result<(), JsValue> {
    let window = browser_window()?;
    required_function(&window, "clearInterval", "window")?.call1(&window, timer)?;
    Ok(())
}

fn browser_window() -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    let window = Reflect::get(&global, &JsValue::from_str("window"))?;
    Ok(if window.is_undefined() {
        global.into()
    } else {
        window
    })
}

fn configured_components() -> Result<ChatComponents, JsValue> {
    COMPONENTS.with(|components| {
        components.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation ChatView was not configured").into()
        })
    })
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn required_bool(value: &JsValue, owner: &str) -> Result<bool, JsValue> {
    value
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be boolean")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be string")).into())
}

fn required_string_value(value: &JsValue, owner: &str) -> Result<String, JsValue> {
    value
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} must be string")).into())
}

fn numeric_property(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be number")).into())
}

fn number_to_u32(value: f64, owner: &str) -> Result<u32, JsValue> {
    number_string(value)
        .parse::<u32>()
        .map_err(|_| js_sys::RangeError::new(&format!("{owner} must be a u32")).into())
}

fn number_string(value: f64) -> String {
    js_sys::Number::from(value)
        .to_string_with_radix(10)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| value.to_string())
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
