//! Browser plugin, Goal dock, and command-input React renderer.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    GOAL_BAR_STYLES, GOAL_COMMAND_STYLES, GOAL_LOCALES, GOAL_NS, GoalBarAction, GoalBarController,
    GoalBarPhase, GoalBarSnapshot, goal_command_input_definition,
};

const INJECT: &[&str] = &[
    "slots",
    "sessions",
    "remote",
    "remote.goals",
    "locale",
    "conversationEvents",
];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
}

struct GoalRuntime {
    controller: GoalBarController,
    goal: Option<GoalBarSnapshot>,
}

/// Configures React, shared primitives, and compiled styles.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiGoal)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_goal(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, primitives });
    });
    inject_styles()
}

/// Applies the browser Goal plugin.
///
/// # Errors
///
/// Returns missing service, Definition, locale, Slot, projection, or component failures.
#[wasm_bindgen(js_name = applyClientUiGoal)]
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn apply_client_ui_goal(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let sessions = required(&ctx, "sessions", "Client Context")?;
    required(&ctx, "remote", "Client Context")?;
    required(&ctx, "remote.goals", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    let events = required(&ctx, "conversationEvents", "Client Context")?;
    call_method(
        &events,
        "register",
        &[
            seekdeep_client_runtime::native_conversation_node_definition_to_js(
                goal_command_input_definition(),
            )?,
        ],
    )?;
    own_locale_dictionaries(&ctx, &locale)?;

    let command_component = goal_command_component(&modules);
    let command_slots = slots.clone();
    let command_installer = Closure::wrap(Box::new(move || {
        let options = object(&[
            ("name", JsValue::from_str("conversation.chat.node")),
            ("key", JsValue::from_str("command-input")),
            ("locale", JsValue::from_str(GOAL_NS)),
        ])?;
        call_method(
            &command_slots,
            "register",
            &[options.into(), command_component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.chat.node"),
            command_installer.into_js_value(),
        ],
    )?;

    let dock_component = goal_dock_component(&modules);
    let dock_slots = slots.clone();
    let dock_sessions = sessions;
    let dock_ctx = ctx;
    let dock_installer = Closure::wrap(Box::new(move || {
        let inject_sessions = dock_sessions.clone();
        let inject_ctx = dock_ctx.clone();
        let inject = Closure::wrap(Box::new(move |session_id: JsValue| {
            goal_actions_face(&inject_ctx, &inject_sessions, &session_id)
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let options = object(&[
            ("name", JsValue::from_str("conversation.input.dock")),
            ("id", JsValue::from_str("goal")),
            ("order", JsValue::from_f64(10.0)),
            ("locale", JsValue::from_str(GOAL_NS)),
            ("inject", inject.into_js_value()),
        ])?;
        call_method(
            &dock_slots,
            "register",
            &[options.into(), dock_component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.input.dock"),
            dock_installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser dependency list.
#[wasm_bindgen(js_name = goalInject)]
pub fn goal_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled direct `GoalBar` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = goalBarComponent)]
pub fn exported_goal_bar_component() -> Result<JsValue, JsValue> {
    Ok(goal_bar_component(&configured_modules()?))
}

fn goal_actions_face(
    ctx: &JsValue,
    sessions: &JsValue,
    session_id: &JsValue,
) -> Result<JsValue, JsValue> {
    if session_id.as_string().is_none() {
        return Err(js_sys::Error::new("Goal dock Session id must be a string").into());
    }
    let face = Object::new();
    for (name, remote_method, with_objective) in [
        ("onEdit", "edit", true),
        ("onPause", "pause", false),
        ("onResume", "resume", false),
        ("onClear", "clear", false),
    ] {
        let action_ctx = ctx.clone();
        let action_sessions = sessions.clone();
        let action_session_id = session_id.clone();
        let callback = Closure::wrap(Box::new(move |objective: JsValue| -> Promise {
            let ctx = action_ctx.clone();
            let sessions = action_sessions.clone();
            let session_id = action_session_id.clone();
            future_to_promise(async move {
                let Some(reference) = current_goal_ref(&sessions, &session_id)? else {
                    return no_current_goal();
                };
                let remote = required(&ctx, "remote", "Client Context")?;
                let goals = required(&remote, "goals", "Remote namespace")?;
                let mut arguments = vec![session_id, reference];
                if with_objective {
                    arguments.push(
                        object(&[(
                            "objective",
                            objective
                                .as_string()
                                .map_or(JsValue::UNDEFINED, |value| JsValue::from_str(&value)),
                        )])?
                        .into(),
                    );
                }
                let returned = call_method(&goals, remote_method, &arguments)?;
                JsFuture::from(Promise::resolve(&returned)).await
            })
        }) as Box<dyn FnMut(JsValue) -> Promise>);
        set(&face, name, &callback.into_js_value())?;
    }
    Ok(face.into())
}

fn current_goal_ref(sessions: &JsValue, session_id: &JsValue) -> Result<Option<JsValue>, JsValue> {
    let binding = call_method(sessions, "binding", std::slice::from_ref(session_id))?;
    if binding.is_null() || binding.is_undefined() {
        return Ok(None);
    }
    let session = required(&binding, "session", "Session binding")?;
    let projections = required(&session, "projections", "Session")?;
    let face = call_method(&projections, "faceOf", &[JsValue::from_str("goal")])?;
    if face.is_null() || face.is_undefined() {
        return Ok(None);
    }
    let projection = call_method(&face, "getSnapshot", &[])?;
    if projection.is_null() || projection.is_undefined() {
        return Ok(None);
    }
    let goal = required(&projection, "goal", "Goal projection")?;
    Ok(Some(
        object(&[
            ("id", required(&goal, "id", "Goal snapshot")?),
            ("revision", required(&goal, "revision", "Goal snapshot")?),
        ])?
        .into(),
    ))
}

fn no_current_goal() -> Result<JsValue, JsValue> {
    object(&[
        ("ok", JsValue::FALSE),
        (
            "error",
            object(&[
                ("code", JsValue::from_str("no-current-goal")),
                ("message", JsValue::from_str("no current goal to mutate")),
                ("details", Object::new().into()),
            ])?
            .into(),
        ),
    ])
    .map(Into::into)
}

fn goal_dock_component(modules: &BrowserModules) -> JsValue {
    let bar = goal_bar_component(modules);
    Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        let use_projection = required_function(&props, "useProjection", "GoalDock")?;
        let projection = use_projection.call1(&JsValue::UNDEFINED, &JsValue::from_str("goal"))?;
        let goal = if projection.is_null() || projection.is_undefined() {
            projection
        } else {
            Reflect::get(&projection, &JsValue::from_str("goal"))?
        };
        let bar_props = clone_props(&props);
        set(&bar_props, "goal", &goal)?;
        element(&bar, &bar_props, &[])
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn goal_bar_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    Closure::wrap(Box::new(move |props: JsValue| render_goal_bar(&ui, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn goal_command_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        let node = required(&props, "node", "GoalCommandInputView")?;
        let data = required(&node, "data", "Goal command Node")?;
        let text = required(&data, "text", "Goal command data")?;
        let translate = required_function(&props, "t", "GoalCommandInputView")?;
        ui.tag(
            "div",
            Some(&object(&[
                ("className", JsValue::from_str("seekdeep-goal-command-row")),
                ("data-command-input", JsValue::from_str("")),
                ("role", JsValue::from_str("group")),
                ("aria-label", translated(&translate, "commandInput.aria")?),
            ])?),
            &[ui.tag(
                "div",
                Some(&class("seekdeep-goal-command-stack")?),
                &[ui.tag(
                    "div",
                    Some(&class("seekdeep-goal-command-bubble")?),
                    &[ui.primitive("MessageText", Some(&object(&[("text", text)])?), &[])?],
                )?],
            )?],
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

#[allow(clippy::too_many_lines)] // Controller ABI methods stay together for auditability.
fn runtime_face() -> Result<JsValue, JsValue> {
    let runtime = Rc::new(RefCell::new(GoalRuntime {
        controller: GoalBarController::new(),
        goal: None,
    }));
    let face = Object::new();
    let goal_runtime = runtime.clone();
    let set_goal = Closure::wrap(Box::new(move |goal: JsValue| -> Result<(), JsValue> {
        let goal = if goal.is_null() || goal.is_undefined() {
            None
        } else {
            Some(serde_wasm_bindgen::from_value(goal).map_err(js_error_from_display)?)
        };
        let mut runtime = goal_runtime.borrow_mut();
        runtime.controller.reconcile(goal.as_ref());
        runtime.goal = goal;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "setGoal", &set_goal.into_js_value())?;

    let snapshot_runtime = runtime.clone();
    let snapshot = Closure::wrap(Box::new(move || {
        let runtime = snapshot_runtime.borrow();
        let value = object(&[
            (
                "goal",
                runtime.goal.as_ref().map_or(Ok(JsValue::NULL), |goal| {
                    serde_wasm_bindgen::to_value(goal).map_err(js_error_from_display)
                })?,
            ),
            (
                "visible",
                JsValue::from_bool(runtime.controller.visible(runtime.goal.as_ref())),
            ),
            ("editing", JsValue::from_bool(runtime.controller.editing())),
            ("draft", JsValue::from_str(runtime.controller.draft())),
            ("pending", JsValue::from_bool(runtime.controller.pending())),
            (
                "error",
                runtime
                    .controller
                    .action_error()
                    .map_or(JsValue::NULL, JsValue::from_str),
            ),
        ])?;
        Ok::<_, JsValue>(value.into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "snapshot", &snapshot.into_js_value())?;

    let edit_runtime = runtime.clone();
    let begin_edit = Closure::wrap(Box::new(move |objective: String| {
        edit_runtime.borrow_mut().controller.begin_edit(&objective);
    }) as Box<dyn FnMut(String)>);
    set(&face, "beginEdit", &begin_edit.into_js_value())?;
    let draft_runtime = runtime.clone();
    let set_draft = Closure::wrap(Box::new(move |draft: String| {
        draft_runtime.borrow_mut().controller.set_draft(draft);
    }) as Box<dyn FnMut(String)>);
    set(&face, "setDraft", &set_draft.into_js_value())?;
    let cancel_runtime = runtime.clone();
    let cancel = Closure::wrap(Box::new(move || {
        cancel_runtime.borrow_mut().controller.cancel_edit();
    }) as Box<dyn FnMut()>);
    set(&face, "cancelEdit", &cancel.into_js_value())?;
    let action_runtime = runtime.clone();
    let begin_action =
        Closure::wrap(
            Box::new(move || action_runtime.borrow_mut().controller.begin_action())
                as Box<dyn FnMut() -> bool>,
        );
    set(&face, "beginAction", &begin_action.into_js_value())?;
    let settle_runtime = runtime;
    let settle = Closure::wrap(Box::new(
        move |action: String, goal_id: JsValue, result: JsValue| -> Result<(), JsValue> {
            let action = match action.as_str() {
                "edit" => GoalBarAction::Edit,
                "pause" => GoalBarAction::Pause,
                "resume" => GoalBarAction::Resume,
                "clear" => GoalBarAction::Clear,
                _ => return Err(js_sys::Error::new("unknown Goal bar action").into()),
            };
            let outcome = if optional_bool(&result, "ok") == Some(true) {
                Ok(())
            } else {
                let error = required(&result, "error", "Goal action result")?;
                Err((
                    required_string(&error, "code", "Goal action error")?,
                    required_string(&error, "message", "Goal action error")?,
                ))
            };
            let mut runtime = settle_runtime.borrow_mut();
            runtime.controller.settle_action(
                action,
                goal_id.as_string().as_deref(),
                outcome
                    .as_ref()
                    .map_err(|(code, message)| (code.as_str(), message.as_str()))
                    .copied(),
            );
            Ok(())
        },
    )
        as Box<dyn FnMut(String, JsValue, JsValue) -> Result<(), JsValue>>);
    set(&face, "settleAction", &settle.into_js_value())?;
    Ok(face.into())
}

#[derive(serde::Deserialize)]
struct BarView {
    goal: Option<GoalBarSnapshot>,
    visible: bool,
    editing: bool,
    draft: String,
    pending: bool,
    error: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn render_goal_bar(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let goal = Reflect::get(props, &JsValue::from_str("goal"))?;
    let translate = required_function(props, "t", "GoalBar")?;
    let runtime_ref = use_ref(&ui.react, &JsValue::UNDEFINED)?;
    let mut runtime = Reflect::get(&runtime_ref, &JsValue::from_str("current"))?;
    if runtime.is_undefined() {
        runtime = runtime_face()?;
        Reflect::set(&runtime_ref, &JsValue::from_str("current"), &runtime)?;
    }
    call_method(&runtime, "setGoal", &[goal])?;
    let (revision, set_revision) = use_state(&ui.react, &JsValue::from_f64(0.0))?;
    let bump = bump_callback(&set_revision, revision.as_f64().unwrap_or(0.0));
    let view: BarView = serde_wasm_bindgen::from_value(call_method(&runtime, "snapshot", &[])?)
        .map_err(js_error_from_display)?;
    if !view.visible {
        return Ok(JsValue::NULL);
    }
    let goal = view.goal.as_ref().expect("visible bar has a goal");
    if view.editing {
        return render_edit_bar(ui, props, &runtime, &bump, &translate, &view, goal);
    }
    let mut children = vec![
        ui.tag(
            "span",
            Some(&class("seekdeep-goal-goalGlyph")?),
            &[ui.primitive("IconGoalOutline16", None, &[])?],
        )?,
        ui.tag(
            "span",
            Some(&class("seekdeep-goal-label")?),
            &[translated(&translate, phase_key(goal.phase))?],
        )?,
        ui.tag(
            "span",
            Some(&class("seekdeep-goal-objective")?),
            &[JsValue::from_str(&goal.objective)],
        )?,
    ];
    if let Some(error) = &view.error {
        children.push(error_node(ui, error)?);
    }
    let mut actions = Vec::new();
    if goal.phase == GoalBarPhase::Active {
        actions.push(goal_action_button(
            ui,
            props,
            &runtime,
            &bump,
            &translate,
            goal,
            "pause",
            "onPause",
            "action.pause",
            view.pending,
        )?);
    } else if goal.phase == GoalBarPhase::Paused {
        actions.push(goal_action_button(
            ui,
            props,
            &runtime,
            &bump,
            &translate,
            goal,
            "resume",
            "onResume",
            "action.resume",
            view.pending,
        )?);
    }
    let edit_runtime = runtime.clone();
    let edit_bump = bump.clone();
    let objective = goal.objective.clone();
    let edit = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&edit_runtime, "beginEdit", &[JsValue::from_str(&objective)])?;
        edit_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    actions.push(icon_button(
        ui,
        &translate,
        "action.edit",
        "IconEditOutline16",
        view.pending,
        edit.into_js_value(),
    )?);
    actions.push(goal_action_button(
        ui,
        props,
        &runtime,
        &bump,
        &translate,
        goal,
        "clear",
        "onClear",
        "action.clear",
        view.pending,
    )?);
    children.push(ui.tag("div", Some(&class("seekdeep-goal-actions")?), &actions)?);
    dock(
        ui,
        &children,
        goal.blocked_reason
            .as_ref()
            .map(|reason| reason.message.as_str()),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_edit_bar(
    ui: &ReactUi,
    props: &JsValue,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    view: &BarView,
    goal: &GoalBarSnapshot,
) -> Result<JsValue, JsValue> {
    let draft_runtime = runtime.clone();
    let draft_bump = bump.clone();
    let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required(&event, "target", "Goal objective change")?;
        let value = required(&target, "value", "Goal objective input")?;
        call_method(&draft_runtime, "setDraft", &[value])?;
        draft_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let key_runtime = runtime.clone();
    let key_bump = bump.clone();
    let key_props = props.clone();
    let key_goal = goal.clone();
    let key_draft = view.draft.trim().to_owned();
    let on_key = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        match required_string(&event, "key", "Goal objective key")?.as_str() {
            "Enter" if !key_draft.is_empty() => run_goal_action(
                &key_props,
                &key_runtime,
                &key_bump,
                &key_goal,
                "edit",
                "onEdit",
                Some(key_draft.as_str()),
            ),
            "Escape" => {
                call_method(&key_runtime, "cancelEdit", &[])?;
                key_bump.call0(&JsValue::UNDEFINED)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let input = ui.tag(
        "input",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-goal-objectiveInput"),
            ),
            ("type", JsValue::from_str("text")),
            ("aria-label", translated(translate, "objective.aria")?),
            ("value", JsValue::from_str(&view.draft)),
            ("onChange", on_change.into_js_value()),
            ("onKeyDown", on_key.into_js_value()),
        ])?),
        &[],
    )?;
    let save_props = props.clone();
    let save_runtime = runtime.clone();
    let save_bump = bump.clone();
    let save_goal = goal.clone();
    let save_draft = view.draft.trim().to_owned();
    let save = Closure::wrap(Box::new(move || {
        run_goal_action(
            &save_props,
            &save_runtime,
            &save_bump,
            &save_goal,
            "edit",
            "onEdit",
            Some(save_draft.as_str()),
        )
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let cancel_runtime = runtime.clone();
    let cancel_bump = bump.clone();
    let cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&cancel_runtime, "cancelEdit", &[])?;
        cancel_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let actions = ui.tag(
        "div",
        Some(&class("seekdeep-goal-actions")?),
        &[
            icon_button(
                ui,
                translate,
                "action.save",
                "IconCheckOutline16",
                view.pending || view.draft.trim().is_empty(),
                save.into_js_value(),
            )?,
            icon_button(
                ui,
                translate,
                "action.cancel",
                "IconCloseOutline16",
                view.pending,
                cancel.into_js_value(),
            )?,
        ],
    )?;
    let mut children = vec![input];
    if let Some(error) = &view.error {
        children.push(error_node(ui, error)?);
    }
    children.push(actions);
    dock(ui, &children, None)
}

#[allow(clippy::too_many_arguments)]
fn goal_action_button(
    ui: &ReactUi,
    props: &JsValue,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    goal: &GoalBarSnapshot,
    action: &str,
    callback: &str,
    label: &str,
    disabled: bool,
) -> Result<JsValue, JsValue> {
    let action_props = props.clone();
    let action_runtime = runtime.clone();
    let action_bump = bump.clone();
    let action_goal = goal.clone();
    let action_name = action.to_owned();
    let callback = callback.to_owned();
    let on_click = Closure::wrap(Box::new(move || {
        run_goal_action(
            &action_props,
            &action_runtime,
            &action_bump,
            &action_goal,
            &action_name,
            &callback,
            None,
        )
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    icon_button(
        ui,
        translate,
        label,
        match action {
            "pause" => "IconPauseOutline16",
            "resume" => "IconPlayOutline16",
            "clear" => "IconTrashOutline16",
            _ => "IconGoalOutline16",
        },
        disabled,
        on_click.into_js_value(),
    )
}

fn run_goal_action(
    props: &JsValue,
    runtime: &JsValue,
    bump: &Function,
    goal: &GoalBarSnapshot,
    action: &str,
    callback: &str,
    objective: Option<&str>,
) -> Result<(), JsValue> {
    if call_method(runtime, "beginAction", &[])?.as_bool() != Some(true) {
        return Ok(());
    }
    bump.call0(&JsValue::UNDEFINED)?;
    let callback = required_function(props, callback, "GoalBar")?;
    let returned = objective.map_or_else(
        || callback.call0(&JsValue::UNDEFINED),
        |objective| callback.call1(&JsValue::UNDEFINED, &JsValue::from_str(objective)),
    )?;
    let promise = Promise::resolve(&returned);
    let settle_runtime = runtime.clone();
    let settle_bump = bump.clone();
    let action = action.to_owned();
    let goal_id = goal.id.clone();
    let _ = future_to_promise(async move {
        let result = JsFuture::from(promise).await?;
        call_method(
            &settle_runtime,
            "settleAction",
            &[
                JsValue::from_str(&action),
                JsValue::from_str(&goal_id),
                result,
            ],
        )?;
        settle_bump.call0(&JsValue::UNDEFINED)?;
        Ok(JsValue::UNDEFINED)
    });
    Ok(())
}

fn icon_button(
    ui: &ReactUi,
    translate: &Function,
    label: &str,
    icon: &str,
    disabled: bool,
    on_click: JsValue,
) -> Result<JsValue, JsValue> {
    let label = translated(translate, label)?;
    let button = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str("seekdeep-goal-iconBtn")),
            ("disabled", JsValue::from_bool(disabled)),
            ("aria-label", label.clone()),
            ("onClick", on_click),
        ])?),
        &[ui.primitive(icon, None, &[])?],
    )?;
    ui.primitive(
        "Tooltip",
        Some(&object(&[
            ("label", label),
            ("side", JsValue::from_str("bottom")),
            ("delayMs", JsValue::from_f64(500.0)),
        ])?),
        &[button],
    )
}

fn dock(ui: &ReactUi, children: &[JsValue], title: Option<&str>) -> Result<JsValue, JsValue> {
    let bar = ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-goal-bar")),
            ("title", title.map_or(JsValue::UNDEFINED, JsValue::from_str)),
        ])?),
        children,
    )?;
    ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-goal-dock")),
            ("data-goal-bar", JsValue::from_str("")),
        ])?),
        &[bar],
    )
}

fn error_node(ui: &ReactUi, error: &str) -> Result<JsValue, JsValue> {
    ui.tag(
        "span",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-goal-error")),
            ("role", JsValue::from_str("alert")),
        ])?),
        &[JsValue::from_str(error)],
    )
}

fn phase_key(phase: GoalBarPhase) -> &'static str {
    match phase {
        GoalBarPhase::Active | GoalBarPhase::Complete => "phase.active",
        GoalBarPhase::Paused => "phase.paused",
        GoalBarPhase::Blocked => "phase.blocked",
    }
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = Object::new();
    let en = Object::new();
    for (key, chinese, english) in GOAL_LOCALES {
        set(&zh, key, &JsValue::from_str(chinese))?;
        set(&en, key, &JsValue::from_str(english))?;
    }
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[JsValue::from_str(GOAL_NS), dictionaries.clone().into()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-goal: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-goal";
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let selector = format!(
        "style[data-plugin={}]",
        serde_json::to_string(PACKAGE).unwrap()
    );
    if !call_method(&document, "querySelector", &[JsValue::from_str(&selector)])?.is_null() {
        return Ok(());
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin"), JsValue::from_str(PACKAGE)],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&format!("{GOAL_BAR_STYLES}\n{GOAL_COMMAND_STYLES}")),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

#[derive(Clone)]
struct ReactUi {
    react: JsValue,
    primitives: JsValue,
}

impl ReactUi {
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
            &required(&self.primitives, name, "UI primitives")?,
            props,
            children,
        )
    }

    fn element(
        &self,
        kind: &JsValue,
        props: Option<&Object>,
        children: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        let args = Array::new();
        args.push(kind);
        args.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            args.push(child);
        }
        required_function(&self.react, "createElement", "React")?.apply(&self.react, &args)
    }
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-goal is not configured").into())
    })
}

fn element(kind: &JsValue, props: &Object, children: &[JsValue]) -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let args = Array::new();
    args.push(kind);
    args.push(props);
    for child in children {
        args.push(child);
    }
    required_function(&modules.react, "createElement", "React")?.apply(&modules.react, &args)
}

fn clone_props(value: &JsValue) -> Object {
    Object::assign(&Object::new(), &Object::from(value.clone()))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn bump_callback(setter: &Function, revision: f64) -> Function {
    let setter = setter.clone();
    Closure::wrap(Box::new(move || {
        setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(revision + 1.0))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value()
    .dyn_into()
    .expect("Closure converts to Function")
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn class(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

fn set(value: &Object, key: &str, entry: &JsValue) -> Result<(), JsValue> {
    Reflect::set(value, &JsValue::from_str(key), entry).map(|_| ())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = if key.contains('.') {
        key.split('.').try_fold(value.clone(), |current, part| {
            Reflect::get(&current, &JsValue::from_str(part))
        })?
    } else {
        Reflect::get(value, &JsValue::from_str(key))?
    };
    if entry.is_null() || entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn optional_bool(value: &JsValue, key: &str) -> Option<bool> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_bool())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
