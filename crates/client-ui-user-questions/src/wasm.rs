//! Browser plugin, pending-carrier controller, and compiled React surfaces.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_user_questions_contract::{AskUserQuestionAnswer, AskUserQuestionItem};
use serde::Serialize;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    DraftAnswer, INJECT, LOCALE_NAMESPACE, PLAN_REVIEW_STYLES, PlanReview, QUESTION_EN,
    QUESTION_STYLES, QUESTION_ZH, QuestionBusy, QuestionFeedback, QuestionFlow, QuestionFlowEffect,
    parse_recommended_label, plan_review_of,
};

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    primitives: JsValue,
}

struct BrowserQuestionRuntime {
    key: String,
    session_id: String,
    carrier: JsValue,
    flow: QuestionFlow,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserQuestionSnapshot<'a> {
    key: &'a str,
    session_id: &'a str,
    index: usize,
    questions: &'a [AskUserQuestionItem],
    drafts: &'a [DraftAnswer],
    busy: Option<QuestionBusy>,
    feedback: Option<&'a QuestionFeedback>,
}

/// Configures React, shared primitives, and compiled styles.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiUserQuestions)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_user_questions(
    react: JsValue,
    primitives: JsValue,
) -> Result<(), JsValue> {
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules { react, primitives });
    });
    inject_styles()
}

/// Applies the browser question plugin to a caller-bound Client Context.
///
/// # Errors
///
/// Returns missing service, locale, selector, Slot, or component failures.
#[wasm_bindgen(js_name = applyClientUiUserQuestions)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_user_questions(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    own_locale_dictionaries(&ctx, &locale)?;
    let component = question_composer_component(&modules);
    let installer_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let select = Closure::wrap(Box::new(move |owner: JsValue| -> Result<JsValue, JsValue> {
            let interactions = Array::from(&required(
                &owner,
                "interactions",
                "conversation composer owner",
            )?);
            Ok(interactions
                .iter()
                .find(|interaction| {
                    optional_string(interaction, "kind").as_deref() == Some("question")
                })
                .unwrap_or(JsValue::NULL))
        })
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
        let options = object(&[
            ("name", JsValue::from_str("conversation.composer")),
            ("select", select.into_js_value()),
            ("locale", JsValue::from_str(LOCALE_NAMESPACE)),
        ])?;
        call_method(
            &installer_slots,
            "register",
            &[options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.composer"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser Client dependency list.
#[wasm_bindgen(js_name = userQuestionsInject)]
pub fn user_questions_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `QuestionComposer` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = questionComposerComponent)]
pub fn exported_question_composer_component() -> Result<JsValue, JsValue> {
    Ok(question_composer_component(&configured_modules()?))
}

fn question_composer_component(modules: &BrowserModules) -> JsValue {
    let ui = ReactUi {
        react: modules.react.clone(),
        primitives: modules.primitives.clone(),
    };
    Closure::wrap(Box::new(move |props: JsValue| render_composer(&ui, &props))
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn runtime_face(matched: &JsValue) -> Result<JsValue, JsValue> {
    let runtime = Rc::new(RefCell::new(runtime_from_carrier(matched)?));
    let face = Object::new();

    let carrier_runtime = runtime.clone();
    let set_carrier = Closure::wrap(Box::new(move |matched: JsValue| -> Result<(), JsValue> {
        let key = required_string(&matched, "key", "question carrier")?;
        let mut runtime = carrier_runtime.borrow_mut();
        if runtime.key == key {
            runtime.carrier = matched;
            runtime.session_id =
                required_string(&runtime.carrier, "sessionId", "question carrier")?;
        } else {
            *runtime = runtime_from_carrier(&matched)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "setCarrier", &set_carrier.into_js_value())?;

    let snapshot_runtime = runtime.clone();
    let snapshot = Closure::wrap(Box::new(move || {
        let runtime = snapshot_runtime.borrow();
        json_compatible(&BrowserQuestionSnapshot {
            key: &runtime.key,
            session_id: &runtime.session_id,
            index: runtime.flow.index(),
            questions: runtime.flow.questions(),
            drafts: runtime.flow.drafts(),
            busy: runtime.flow.busy(),
            feedback: runtime.flow.feedback(),
        })
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "snapshot", &snapshot.into_js_value())?;

    let choose_runtime = runtime.clone();
    let choose = Closure::wrap(Box::new(move |label: String| {
        choose_runtime.borrow_mut().flow.choose(&label);
    }) as Box<dyn FnMut(String)>);
    set(&face, "choose", &choose.into_js_value())?;

    let custom_runtime = runtime.clone();
    let custom = Closure::wrap(Box::new(move |value: String| {
        custom_runtime.borrow_mut().flow.set_custom(value);
    }) as Box<dyn FnMut(String)>);
    set(&face, "setCustom", &custom.into_js_value())?;

    for (name, action) in [
        ("previous", 0_u8),
        ("next", 1),
        ("continue", 2),
        ("enterOption", 3),
        ("skip", 4),
        ("beginCancel", 5),
    ] {
        let action_runtime = runtime.clone();
        let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let mut runtime = action_runtime.borrow_mut();
            let effect = match action {
                0 => {
                    runtime.flow.previous();
                    QuestionFlowEffect::None
                }
                1 => {
                    runtime.flow.next();
                    QuestionFlowEffect::None
                }
                2 => runtime.flow.continue_flow(),
                3 => runtime.flow.enter_option(),
                4 => runtime.flow.skip(),
                _ => {
                    runtime.flow.begin_cancel();
                    QuestionFlowEffect::None
                }
            };
            effect_to_js(effect)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        set(&face, name, &callback.into_js_value())?;
    }

    let fail_runtime = runtime.clone();
    let fail = Closure::wrap(Box::new(move |message: String| {
        fail_runtime.borrow_mut().flow.fail(message);
    }) as Box<dyn FnMut(String)>);
    set(&face, "fail", &fail.into_js_value())?;

    let carrier_runtime = runtime;
    let carrier =
        Closure::wrap(Box::new(move || carrier_runtime.borrow().carrier.clone())
            as Box<dyn FnMut() -> JsValue>);
    set(&face, "carrier", &carrier.into_js_value())?;
    Ok(face.into())
}

fn runtime_from_carrier(matched: &JsValue) -> Result<BrowserQuestionRuntime, JsValue> {
    let payload = required(matched, "payload", "question carrier")?;
    let questions = serde_wasm_bindgen::from_value::<QuestionPayload>(payload)
        .map_err(js_error_from_display)?
        .questions;
    Ok(BrowserQuestionRuntime {
        key: required_string(matched, "key", "question carrier")?,
        session_id: required_string(matched, "sessionId", "question carrier")?,
        carrier: matched.clone(),
        flow: QuestionFlow::new(questions),
    })
}

#[derive(serde::Deserialize)]
struct QuestionPayload {
    questions: Vec<AskUserQuestionItem>,
}

fn effect_to_js(effect: QuestionFlowEffect) -> Result<JsValue, JsValue> {
    match effect {
        QuestionFlowEffect::None => Ok(JsValue::NULL),
        QuestionFlowEffect::Answer(answer) => json_compatible(&answer),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotValue {
    key: String,
    session_id: String,
    index: usize,
    questions: Vec<AskUserQuestionItem>,
    drafts: Vec<DraftAnswer>,
    busy: Option<QuestionBusy>,
    feedback: Option<QuestionFeedback>,
}

#[allow(clippy::too_many_lines)]
fn render_composer(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let matched = required(props, "matched", "QuestionComposer")?;
    let translate = required_function(props, "t", "QuestionComposer")?;
    let runtime_ref = use_ref(&ui.react, &JsValue::UNDEFINED)?;
    let mut runtime = Reflect::get(&runtime_ref, &JsValue::from_str("current"))?;
    if runtime.is_undefined() {
        runtime = runtime_face(&matched)?;
        Reflect::set(&runtime_ref, &JsValue::from_str("current"), &runtime)?;
    } else {
        call_method(&runtime, "setCarrier", &[matched])?;
    }
    let (revision, set_revision) = use_state(&ui.react, &JsValue::from_f64(0.0))?;
    let bump = bump_callback(&set_revision, revision.as_f64().unwrap_or(0.0));
    let snapshot: SnapshotValue =
        serde_wasm_bindgen::from_value(call_method(&runtime, "snapshot", &[])?)
            .map_err(js_error_from_display)?;
    let review = plan_review_of(&snapshot.questions);
    if let Some(review) = review {
        render_plan_review(ui, &runtime, &bump, &translate, &snapshot, &review)
    } else {
        render_question_flow(ui, &runtime, &bump, &translate, &snapshot)
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_question_flow(
    ui: &ReactUi,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    snapshot: &SnapshotValue,
) -> Result<JsValue, JsValue> {
    let question = &snapshot.questions[snapshot.index];
    let draft = &snapshot.drafts[snapshot.index];
    let disabled = snapshot.busy.is_some();
    let title_id = format!("question-{}-{}", snapshot.key, snapshot.index);
    let mut header_copy = Vec::new();
    if let Some(header) = &question.header {
        header_copy.push(ui.tag(
            "div",
            Some(&class("seekdeep-question-eyebrow")?),
            &[JsValue::from_str(header)],
        )?);
    }
    header_copy.push(ui.tag(
        "h2",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-question-title")),
            ("id", JsValue::from_str(&title_id)),
        ])?),
        &[JsValue::from_str(&question.question)],
    )?);
    let heading = ui.tag(
        "div",
        Some(&class("seekdeep-question-headingBlock")?),
        &header_copy,
    )?;
    let cancel = action_button(
        ui,
        runtime,
        bump,
        translate,
        "beginCancel",
        "nav.cancel",
        disabled,
        Some("seekdeep-question-iconButton"),
        SendKind::Cancel,
    )?;
    let header = ui.tag(
        "header",
        Some(&class("seekdeep-question-header")?),
        &[heading, cancel],
    )?;

    let mut body_children = Vec::new();
    if let Some(detail) = &question.detail {
        body_children.push(ui.tag(
            "div",
            Some(&class("seekdeep-question-detail")?),
            &[ui.primitive(
                "MarkdownText",
                Some(&object(&[("text", JsValue::from_str(detail))])?),
                &[],
            )?],
        )?);
    }
    let mut options = Vec::new();
    for (option_index, option) in question
        .options
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let selected = draft.selected.contains(&option.label);
        let display = parse_recommended_label(&option.label);
        let option_runtime = runtime.clone();
        let option_bump = bump.clone();
        let label = option.label.clone();
        let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(&option_runtime, "choose", &[JsValue::from_str(&label)])?;
            option_bump.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let key_runtime = runtime.clone();
        let key_bump = bump.clone();
        let on_key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if required_string(&event, "key", "option key event")? != "Enter" {
                return Ok(());
            }
            let answer = call_method(&key_runtime, "enterOption", &[])?;
            if answer.is_null() {
                return Ok(());
            }
            prevent_default(&event)?;
            send_answer(&key_runtime, &key_bump, answer)
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let mut copy = vec![ui.tag(
            "span",
            Some(&class("seekdeep-question-optionLabel")?),
            &[JsValue::from_str(&display.label)],
        )?];
        if display.recommended {
            copy.push(ui.tag(
                "span",
                Some(&class("seekdeep-question-badge")?),
                &[translated(translate, "option.recommended")?],
            )?);
        }
        if let Some(description) = &option.description {
            copy.push(ui.tag(
                "span",
                Some(&class("seekdeep-question-description")?),
                &[JsValue::from_str(description)],
            )?);
        }
        options.push(ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(if selected && question.multi_select != Some(true) {
                        "seekdeep-question-option seekdeep-question-optionSelected"
                    } else {
                        "seekdeep-question-option"
                    }),
                ),
                (
                    "role",
                    JsValue::from_str(if question.multi_select == Some(true) {
                        "checkbox"
                    } else {
                        "radio"
                    }),
                ),
                ("aria-checked", JsValue::from_bool(selected)),
                ("aria-label", JsValue::from_str(&display.label)),
                ("disabled", JsValue::from_bool(disabled)),
                ("onClick", on_click.into_js_value()),
                ("onKeyDown", on_key_down.into_js_value()),
            ])?),
            &[
                ui.tag(
                    "span",
                    Some(&class(if question.multi_select == Some(true) {
                        "seekdeep-question-checkbox"
                    } else {
                        "seekdeep-question-number"
                    })?),
                    &[JsValue::from_str(&(option_index + 1).to_string())],
                )?,
                ui.tag("span", Some(&class("seekdeep-question-optionCopy")?), &copy)?,
            ],
        )?);
    }
    options.push(custom_input(
        ui, runtime, bump, translate, snapshot, disabled,
    )?);
    body_children.push(ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-question-options")),
            (
                "role",
                JsValue::from_str(if question.multi_select == Some(true) {
                    "group"
                } else {
                    "radiogroup"
                }),
            ),
        ])?),
        &options,
    )?);
    let body = ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-question-body")),
            ("data-question-scroll", JsValue::from_str("")),
        ])?),
        &body_children,
    )?;

    let previous = simple_flow_button(
        ui,
        runtime,
        bump,
        translate,
        "previous",
        "nav.prev",
        snapshot.index == 0 || disabled,
    )?;
    let next = simple_flow_button(
        ui,
        runtime,
        bump,
        translate,
        "next",
        "nav.next",
        snapshot.index + 1 == snapshot.questions.len() || disabled,
    )?;
    let progress = ui.tag(
        "span",
        Some(&class("seekdeep-question-progress")?),
        &[JsValue::from_str(&format!(
            "{} / {}",
            snapshot.index + 1,
            snapshot.questions.len()
        ))],
    )?;
    let feedback = ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-question-feedback")),
            ("role", JsValue::from_str("status")),
        ])?),
        &[feedback_value(translate, snapshot.feedback.as_ref())?],
    )?;
    let skip = action_button(
        ui,
        runtime,
        bump,
        translate,
        "skip",
        "action.skip",
        disabled,
        None,
        SendKind::Answer,
    )?;
    let continue_label = if snapshot.busy == Some(QuestionBusy::Answer) {
        "submitting"
    } else if snapshot.index + 1 == snapshot.questions.len() {
        "submit"
    } else {
        "action.next"
    };
    let continue_button = action_button(
        ui,
        runtime,
        bump,
        translate,
        "continue",
        continue_label,
        disabled || !answered(draft),
        None,
        SendKind::Answer,
    )?;
    let footer = ui.tag(
        "footer",
        Some(&class("seekdeep-question-footer")?),
        &[
            ui.tag(
                "div",
                Some(&class("seekdeep-question-pager")?),
                &[previous, progress, next],
            )?,
            feedback,
            ui.tag(
                "div",
                Some(&class("seekdeep-question-footerActions")?),
                &[skip, continue_button],
            )?,
        ],
    )?;
    let card = ui.tag(
        "section",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-question-card")),
            ("aria-labelledby", JsValue::from_str(&title_id)),
        ])?),
        &[header, body, footer],
    )?;
    ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-question-frame")),
            ("data-question-key", JsValue::from_str(&snapshot.key)),
        ])?),
        &[card],
    )
}

fn custom_input(
    ui: &ReactUi,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    snapshot: &SnapshotValue,
    disabled: bool,
) -> Result<JsValue, JsValue> {
    let question = &snapshot.questions[snapshot.index];
    let draft = &snapshot.drafts[snapshot.index];
    let input_runtime = runtime.clone();
    let input_bump = bump.clone();
    let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required(&event, "target", "custom answer change")?;
        let value = required_string(&target, "value", "custom answer input")?;
        call_method(&input_runtime, "setCustom", &[JsValue::from_str(&value)])?;
        input_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let key_runtime = runtime.clone();
    let key_bump = bump.clone();
    let on_key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if required_string(&event, "key", "custom answer key event")? != "Enter"
            || optional_bool(&event, "shiftKey").unwrap_or(false)
            || is_composing(&event)
        {
            return Ok(());
        }
        prevent_default(&event)?;
        let answer = call_method(&key_runtime, "continue", &[])?;
        if answer.is_null() {
            key_bump.call0(&JsValue::UNDEFINED)?;
            Ok(())
        } else {
            send_answer(&key_runtime, &key_bump, answer)
        }
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let has_options = question
        .options
        .as_ref()
        .is_some_and(|options| !options.is_empty());
    let input = ui.tag(
        if has_options { "input" } else { "textarea" },
        Some(&object(&[
            (
                "type",
                if has_options {
                    JsValue::from_str("text")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "className",
                JsValue::from_str(if has_options {
                    "seekdeep-question-customInput"
                } else {
                    "seekdeep-question-customTextarea"
                }),
            ),
            ("value", JsValue::from_str(&draft.custom)),
            ("disabled", JsValue::from_bool(disabled)),
            ("placeholder", translated(translate, "custom.placeholder")?),
            ("rows", JsValue::from_f64(2.0)),
            ("onChange", on_change.into_js_value()),
            ("onKeyDown", on_key_down.into_js_value()),
        ])?),
        &[],
    )?;
    if !has_options {
        return Ok(input);
    }
    let indicator_class = if question.multi_select == Some(true) {
        if draft.custom.is_empty() {
            "seekdeep-question-checkbox"
        } else {
            "seekdeep-question-checkbox seekdeep-question-checkboxChecked"
        }
    } else {
        "seekdeep-question-number"
    };
    let indicator = ui.tag(
        "span",
        Some(&object(&[
            ("className", JsValue::from_str(indicator_class)),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )?;
    ui.tag(
        "div",
        Some(&class(if draft.custom.is_empty() {
            "seekdeep-question-customRow"
        } else {
            "seekdeep-question-customRow seekdeep-question-customRowActive"
        })?),
        &[indicator, input],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_plan_review(
    ui: &ReactUi,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    snapshot: &SnapshotValue,
    review: &PlanReview,
) -> Result<JsValue, JsValue> {
    let disabled = snapshot.busy.is_some();
    let discuss = action_button(
        ui,
        runtime,
        bump,
        translate,
        "beginCancel",
        "plan.discuss",
        disabled,
        Some("seekdeep-plan-review-discuss"),
        SendKind::Cancel,
    )?;
    let mut actions = vec![discuss];
    if let Some(decline) = &review.decline {
        actions.push(plan_decision_button(
            ui,
            runtime,
            bump,
            translate,
            decline,
            "plan.decline",
            disabled,
        )?);
    }
    actions.push(plan_decision_button(
        ui,
        runtime,
        bump,
        translate,
        &review.approve,
        "plan.approve",
        disabled,
    )?);
    let feedback = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-plan-review-feedback"),
            ),
            ("role", JsValue::from_str("status")),
        ])?),
        &[feedback_value(translate, snapshot.feedback.as_ref())?],
    )?;
    let card = ui.tag(
        "section",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-plan-review-card")),
            ("aria-label", JsValue::from_str(&review.question)),
        ])?),
        &[
            ui.tag(
                "div",
                Some(&class("seekdeep-plan-review-strip")?),
                &[
                    ui.tag("span", Some(&class("seekdeep-plan-review-dot")?), &[])?,
                    translated(translate, "plan.header")?,
                ],
            )?,
            ui.tag(
                "div",
                Some(&object(&[
                    ("className", JsValue::from_str("seekdeep-plan-review-body")),
                    ("data-plan-review-scroll", JsValue::from_str("")),
                ])?),
                &[ui.primitive(
                    "MarkdownText",
                    Some(&object(&[("text", JsValue::from_str(&review.plan))])?),
                    &[],
                )?],
            )?,
            ui.tag(
                "div",
                Some(&class("seekdeep-plan-review-footer")?),
                &[
                    feedback,
                    ui.tag(
                        "div",
                        Some(&class("seekdeep-plan-review-actions")?),
                        &actions,
                    )?,
                ],
            )?,
        ],
    )?;
    ui.tag(
        "div",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-plan-review-frame")),
            ("data-plan-review-key", JsValue::from_str(&snapshot.key)),
        ])?),
        &[card],
    )
}

#[derive(Clone, Copy)]
enum SendKind {
    Answer,
    Cancel,
}

#[allow(clippy::too_many_arguments)]
fn action_button(
    ui: &ReactUi,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    method: &str,
    label_key: &str,
    disabled: bool,
    class_name: Option<&str>,
    send_kind: SendKind,
) -> Result<JsValue, JsValue> {
    let action_runtime = runtime.clone();
    let action_bump = bump.clone();
    let method = method.to_owned();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let answer = call_method(&action_runtime, &method, &[])?;
        action_bump.call0(&JsValue::UNDEFINED)?;
        match send_kind {
            SendKind::Answer if !answer.is_null() => {
                send_answer(&action_runtime, &action_bump, answer)
            }
            SendKind::Cancel => send_cancel(&action_runtime, &action_bump),
            SendKind::Answer => Ok(()),
        }
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    ui.primitive(
        "Button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                class_name.map_or(JsValue::UNDEFINED, JsValue::from_str),
            ),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[translated(translate, label_key)?],
    )
}

fn simple_flow_button(
    ui: &ReactUi,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    method: &str,
    label_key: &str,
    disabled: bool,
) -> Result<JsValue, JsValue> {
    let action_runtime = runtime.clone();
    let action_bump = bump.clone();
    let method = method.to_owned();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&action_runtime, &method, &[])?;
        action_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-question-iconButton"),
            ),
            ("aria-label", translated(translate, label_key)?),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[],
    )
}

fn plan_decision_button(
    ui: &ReactUi,
    runtime: &JsValue,
    bump: &Function,
    translate: &Function,
    option: &seekdeep_user_questions_contract::AskUserQuestionOption,
    label_key: &str,
    disabled: bool,
) -> Result<JsValue, JsValue> {
    let decision_runtime = runtime.clone();
    let decision_bump = bump.clone();
    let label = option.label.clone();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&decision_runtime, "choose", &[JsValue::from_str(&label)])?;
        let answer = call_method(&decision_runtime, "continue", &[])?;
        decision_bump.call0(&JsValue::UNDEFINED)?;
        if answer.is_null() {
            Ok(())
        } else {
            send_answer(&decision_runtime, &decision_bump, answer)
        }
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    ui.primitive(
        "Button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "title",
                option
                    .description
                    .as_ref()
                    .map_or(JsValue::UNDEFINED, |description| {
                        JsValue::from_str(description)
                    }),
            ),
            ("disabled", JsValue::from_bool(disabled)),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[translated(translate, label_key)?],
    )
}

fn send_answer(runtime: &JsValue, bump: &Function, answer: JsValue) -> Result<(), JsValue> {
    let snapshot: SnapshotValue =
        serde_wasm_bindgen::from_value(call_method(runtime, "snapshot", &[])?)
            .map_err(js_error_from_display)?;
    let answer: AskUserQuestionAnswer =
        serde_wasm_bindgen::from_value(answer).map_err(js_error_from_display)?;
    let result = serde_json::json!({
        "ok": true,
        "value": {"sessionId": snapshot.session_id, "answer": answer},
    });
    send_response(runtime, bump, &result, "response")
}

fn send_cancel(runtime: &JsValue, bump: &Function) -> Result<(), JsValue> {
    send_response(
        runtime,
        bump,
        &serde_json::json!({
            "ok": false,
            "error": {
                "code": "cancelled",
                "message": "the user closed this question request",
                "details": {},
            },
        }),
        "cancellation",
    )
}

fn send_response(
    runtime: &JsValue,
    bump: &Function,
    result: &serde_json::Value,
    operation: &str,
) -> Result<(), JsValue> {
    let carrier = call_method(runtime, "carrier", &[])?;
    let result = json_compatible(&result)?;
    let returned = call_method(&carrier, "respond", &[result])
        .unwrap_or_else(|error| Promise::reject(&error).into());
    let promise = Promise::resolve(&returned);
    let failure_runtime = runtime.clone();
    let failure_bump = bump.clone();
    let operation = operation.to_owned();
    let _ = future_to_promise(async move {
        let failure = match JsFuture::from(promise).await {
            Ok(receipt) if optional_bool(&receipt, "accepted") == Some(true) => None,
            Ok(receipt) => Some(format!(
                "question {operation} rejected: {}",
                optional_string(&receipt, "reason").unwrap_or_else(|| "undefined".to_owned())
            )),
            Err(error) => Some(js_error_text(&error)),
        };
        if let Some(message) = failure {
            call_method(&failure_runtime, "fail", &[JsValue::from_str(&message)])?;
            failure_bump.call0(&JsValue::UNDEFINED)?;
        }
        Ok(JsValue::UNDEFINED)
    });
    Ok(())
}

fn feedback_value(
    translate: &Function,
    feedback: Option<&QuestionFeedback>,
) -> Result<JsValue, JsValue> {
    match feedback {
        None => Ok(JsValue::NULL),
        Some(QuestionFeedback::Text(text)) => Ok(JsValue::from_str(text)),
        Some(feedback) => translated(
            translate,
            feedback
                .locale_key()
                .expect("validation feedback has a key"),
        ),
    }
}

fn answered(draft: &DraftAnswer) -> bool {
    !draft.selected.is_empty() || !draft.custom.trim().is_empty()
}

fn is_composing(event: &JsValue) -> bool {
    let native = Reflect::get(event, &JsValue::from_str("nativeEvent")).unwrap_or_default();
    optional_bool(&native, "isComposing").unwrap_or(false)
        || optional_number(&native, "keyCode") == Some(229.0)
}

fn prevent_default(event: &JsValue) -> Result<(), JsValue> {
    call_method(event, "preventDefault", &[]).map(|_| ())
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

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let dictionaries = object(&[
        ("zh", dictionary(QUESTION_ZH)?.into()),
        ("en", dictionary(QUESTION_EN)?.into()),
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
            JsValue::from_str("ui-user-questions: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-user-questions";
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
        &JsValue::from_str(&format!("{QUESTION_STYLES}\n{PLAN_REVIEW_STYLES}")),
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
            .ok_or_else(|| js_sys::Error::new("client-ui-user-questions is not configured").into())
    })
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn json_compatible(value: &impl Serialize) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(js_error_from_display)
}

fn dictionary(entries: &[(&str, &str)]) -> Result<Object, JsValue> {
    let dictionary = Object::new();
    for (key, value) in entries {
        set(&dictionary, key, &JsValue::from_str(value))?;
    }
    Ok(dictionary)
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
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
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

fn optional_string(value: &JsValue, key: &str) -> Option<String> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_string())
}

fn optional_bool(value: &JsValue, key: &str) -> Option<bool> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_bool())
}

fn optional_number(value: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_f64())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error_text(value: &JsValue) -> String {
    Reflect::get(value, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}

fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
