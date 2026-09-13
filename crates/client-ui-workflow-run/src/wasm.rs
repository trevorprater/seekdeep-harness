//! Browser workflow-run renderer and Client plugin registration.

use std::{cell::RefCell, collections::BTreeSet};

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    WORKFLOW_RUN_EN, WORKFLOW_RUN_NS, WORKFLOW_RUN_STYLES, WORKFLOW_RUN_ZH, WorkflowDotState,
    WorkflowRunChatData, WorkflowRunMemberData, WorkflowRunPhaseData, WorkflowRunStatus,
    WorkflowSessionSummary, navigable_members, phase_requires_expansion, phase_status_counts,
    run_requires_expansion, workflow_dot_state, workflow_run_definition,
};

const INJECT: &[&str] = &["conversationEvents", "slots", "sessions", "locale"];

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct ComponentModules {
    react: JsValue,
    disclosure_row: JsValue,
    chevron_right: JsValue,
    state_dot: JsValue,
    shallow_equal: JsValue,
    forced_open_toggle: JsValue,
}

#[derive(Clone)]
struct BrowserModules {
    components: ComponentModules,
    status_disclosure: JsValue,
}

/// Configures React, UI primitives, runtime equality, and the compiled stylesheet.
///
/// # Errors
///
/// Returns missing module exports or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiWorkflowRun)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_workflow_run(
    react: JsValue,
    primitives: JsValue,
    runtime: JsValue,
) -> Result<(), JsValue> {
    let forced_open_toggle = Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>).into_js_value();
    let components = ComponentModules {
        react,
        disclosure_row: required(&primitives, "DisclosureRow", "UI primitives")?,
        chevron_right: required(&primitives, "IconChevronRightOutline14", "UI primitives")?,
        state_dot: required(&primitives, "StateDot", "UI primitives")?,
        shallow_equal: required(&runtime, "shallowEqual", "Client runtime")?,
        forced_open_toggle,
    };
    let manual_disclosure = manual_disclosure_component(&components);
    let status_disclosure = status_disclosure_component(&components, &manual_disclosure);
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            components,
            status_disclosure,
        });
    });
    inject_styles()
}

/// Applies the workflow-run browser plugin.
///
/// # Errors
///
/// Returns missing service, Definition, locale, Slot, Session, or component failures.
#[wasm_bindgen(js_name = applyClientUiWorkflowRun)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_ui_workflow_run(ctx: JsValue) -> Result<(), JsValue> {
    let modules = configured_modules()?;
    let events = required(&ctx, "conversationEvents", "Client Context")?;
    let slots = required(&ctx, "slots", "Client Context")?;
    let sessions = required(&ctx, "sessions", "Client Context")?;
    let locale = required(&ctx, "locale", "Client Context")?;
    call_method(
        &events,
        "register",
        &[
            seekdeep_client_runtime::native_conversation_node_definition_to_js(
                workflow_run_definition(),
            )?,
        ],
    )?;
    own_locale_dictionaries(&ctx, &locale)?;

    let component = workflow_run_panel_component(&modules);
    let registration_slots = slots.clone();
    let installer = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let injected_sessions = sessions.clone();
        let inject = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let open_sessions = injected_sessions.clone();
            let open = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
                call_method(&open_sessions, "open", &[JsValue::from_str(&id)])?;
                Ok(())
            })
                as Box<dyn FnMut(String) -> Result<(), JsValue>>);
            object(&[("openSession", open.into_js_value())]).map(Into::into)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let options = object(&[
            ("name", JsValue::from_str("conversation.chat.node")),
            ("key", JsValue::from_str("workflow-run")),
            ("locale", JsValue::from_str(WORKFLOW_RUN_NS)),
            ("inject", inject.into_js_value()),
        ])?;
        call_method(
            &registration_slots,
            "register",
            &[options.into(), component.clone()],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        &slots,
        "inject",
        &[
            JsValue::from_str("conversation.chat.node"),
            installer.into_js_value(),
        ],
    )?;
    Ok(())
}

/// Returns the exact browser dependency order.
#[wasm_bindgen(js_name = workflowRunInject)]
pub fn workflow_run_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

/// Returns the compiled `WorkflowRunPanel` component.
///
/// # Errors
///
/// Returns before browser modules are configured.
#[wasm_bindgen(js_name = workflowRunPanelComponent)]
pub fn exported_workflow_run_panel_component() -> Result<JsValue, JsValue> {
    Ok(workflow_run_panel_component(&configured_modules()?))
}

fn manual_disclosure_component(modules: &ComponentModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_manual_disclosure(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

fn render_manual_disclosure(
    modules: &ComponentModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let setter = set_open;
    let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let updater = Closure::wrap(Box::new(|value: bool| !value) as Box<dyn FnMut(bool) -> bool>);
        setter.call1(&JsValue::UNDEFINED, &updater.into_js_value())?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let forwarded = clone_object(props);
    set(&forwarded, "open", &open)?;
    set(&forwarded, "expandable", &JsValue::TRUE)?;
    set(&forwarded, "onToggle", &toggle.into_js_value())?;
    element(
        &modules.react,
        &modules.disclosure_row,
        Some(&forwarded),
        &[],
    )
}

fn status_disclosure_component(modules: &ComponentModules, manual: &JsValue) -> JsValue {
    let modules = modules.clone();
    let manual = manual.clone();
    Closure::wrap(Box::new(move |props: JsValue| {
        render_status_disclosure(&modules, &manual, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
    .into_js_value()
}

fn render_status_disclosure(
    modules: &ComponentModules,
    manual: &JsValue,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let requires_expansion = required(props, "requiresExpansion", "StatusDisclosure")?
        .as_bool()
        .unwrap_or(false);
    let clean_cycle = Reflect::get(props, &JsValue::from_str("cleanCycleKey"))?;
    let forwarded = clone_object(props);
    Reflect::delete_property(&forwarded, &JsValue::from_str("requiresExpansion"))?;
    Reflect::delete_property(&forwarded, &JsValue::from_str("cleanCycleKey"))?;
    if requires_expansion {
        set(&forwarded, "open", &JsValue::TRUE)?;
        set(&forwarded, "expandable", &JsValue::FALSE)?;
        set(&forwarded, "onToggle", &modules.forced_open_toggle)?;
        element(
            &modules.react,
            &modules.disclosure_row,
            Some(&forwarded),
            &[],
        )
    } else {
        set(&forwarded, "key", &clean_cycle)?;
        element(&modules.react, manual, Some(&forwarded), &[])
    }
}

fn workflow_run_panel_component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_workflow_run_panel(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_workflow_run_panel(
    modules: &BrowserModules,
    props: &JsValue,
) -> Result<JsValue, JsValue> {
    let node = required(props, "node", "WorkflowRunPanel")?;
    let data = serde_wasm_bindgen::from_value::<WorkflowRunChatData>(required(
        &node,
        "data",
        "workflow-run Node",
    )?)
    .map_err(js_error_from_display)?;
    let session_id = SessionId::new(required_string(props, "sessionId", "WorkflowRunPanel")?);
    let use_sessions = required_function(props, "useSessions", "WorkflowRunPanel")?;
    let open_session = required_function(props, "openSession", "WorkflowRunPanel")?;
    let translate = required_function(props, "t", "WorkflowRunPanel")?;

    let selector_phases = data.phases.clone();
    let selector_parent = session_id.clone();
    let selector = Closure::wrap(
        Box::new(move |sessions: JsValue| -> Result<Array, JsValue> {
            navigable_from_js(&sessions, &selector_phases, &selector_parent)
        }) as Box<dyn FnMut(JsValue) -> Result<Array, JsValue>>,
    );
    let selected = use_sessions.call2(
        &JsValue::UNDEFINED,
        &selector.into_js_value(),
        &modules.components.shallow_equal,
    )?;
    let navigable = Array::from(&selected)
        .iter()
        .map(|id| {
            id.as_string()
                .ok_or_else(|| js_sys::Error::new("navigable Session id must be a string").into())
        })
        .collect::<Result<BTreeSet<_>, JsValue>>()?;

    let total_members = data
        .phases
        .iter()
        .map(|phase| phase.members.len())
        .sum::<usize>();
    let requires_expansion = run_requires_expansion(data.status, &data.phases);
    let phase_list = render_phase_list(modules, &data, &navigable, &open_session, &translate)?;
    let run = render_run_header(
        modules,
        &data.name,
        data.status,
        total_members,
        requires_expansion,
        &translate,
        phase_list,
    )?;
    tag(
        &modules.components.react,
        "section",
        Some(&object(&[
            ("className", JsValue::from_str("seekdeep-workflow-run-root")),
            ("data-workflow-run", JsValue::from_str("")),
            (
                "data-run-status",
                JsValue::from_str(status_name(data.status)),
            ),
        ])?),
        &[run],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_run_header(
    modules: &BrowserModules,
    name: &str,
    status: WorkflowRunStatus,
    count: usize,
    requires_expansion: bool,
    translate: &Function,
    children: JsValue,
) -> Result<JsValue, JsValue> {
    let icon = component(
        &modules.components.react,
        &modules.components.chevron_right,
        None,
        &[],
    )?;
    let separator = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-separator"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )?;
    let summary = tag(
        &modules.components.react,
        "span",
        Some(&class("seekdeep-workflow-run-runSummary")?),
        &[member_count(count, translate)?],
    )?;
    let dot = render_state_dot(modules, status)?;
    let status_text = tag(
        &modules.components.react,
        "span",
        None,
        &[translated(translate, status_key(status))?],
    )?;
    let tail = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-statusTail"),
            ),
            ("data-status", JsValue::from_str(status_name(status))),
        ])?),
        &[dot, status_text],
    )?;
    let collapsed = fragment(&modules.components.react, &[separator, summary, tail])?;
    let props = object(&[
        ("icon", icon),
        (
            "title",
            translated_value(translate, "run.title", "name", JsValue::from_str(name))?,
        ),
        ("requiresExpansion", JsValue::from_bool(requires_expansion)),
        ("expandOnRowClick", JsValue::TRUE),
        ("previewChevron", JsValue::FALSE),
        ("keepContentWhenOpen", JsValue::TRUE),
        (
            "rowClassName",
            JsValue::from_str("seekdeep-workflow-run-runHeader"),
        ),
        (
            "leadingClassName",
            JsValue::from_str("seekdeep-workflow-run-runLeading"),
        ),
        (
            "titleClassName",
            JsValue::from_str("seekdeep-workflow-run-runTitle"),
        ),
        ("collapsedContent", collapsed),
    ])?;
    component(
        &modules.components.react,
        &modules.status_disclosure,
        Some(&props),
        &[children],
    )
}

fn render_phase_list(
    modules: &BrowserModules,
    data: &WorkflowRunChatData,
    navigable: &BTreeSet<String>,
    open_session: &Function,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let mut phases = Vec::new();
    if data.phases.is_empty() {
        phases.push(tag(
            &modules.components.react,
            "span",
            Some(&class("seekdeep-workflow-run-empty")?),
            &[translated(translate, "run.empty")?],
        )?);
    } else {
        for phase in &data.phases {
            phases.push(render_phase_section(
                modules,
                phase,
                navigable,
                open_session,
                translate,
            )?);
        }
    }
    tag(
        &modules.components.react,
        "div",
        Some(&class("seekdeep-workflow-run-phaseList")?),
        &phases,
    )
}

fn render_phase_section(
    modules: &BrowserModules,
    phase: &WorkflowRunPhaseData,
    navigable: &BTreeSet<String>,
    open_session: &Function,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let icon = component(
        &modules.components.react,
        &modules.components.chevron_right,
        None,
        &[],
    )?;
    let separator = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-separator"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[],
    )?;
    let count = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-phaseCount"),
            ),
            ("data-phase-count", JsValue::from_str("")),
        ])?),
        &[member_count(phase.members.len(), translate)?],
    )?;
    let summary = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-phaseStatus"),
            ),
            ("data-phase-status-text", JsValue::from_str("")),
        ])?),
        &[phase_status_summary(phase, translate)?],
    )?;
    let collapsed = fragment(&modules.components.react, &[separator, count, summary])?;
    let mut members = Vec::new();
    for member in &phase.members {
        members.push(render_member_row(
            modules,
            member,
            navigable.contains(member.child_id.as_str()),
            open_session,
            translate,
        )?);
    }
    let member_list = tag(
        &modules.components.react,
        "div",
        Some(&class("seekdeep-workflow-run-members")?),
        &members,
    )?;
    let props = object(&[
        ("key", JsValue::from_str(&phase.key)),
        ("icon", icon),
        ("title", readable_phase(phase.phase.as_deref(), translate)?),
        (
            "cleanCycleKey",
            JsValue::from_f64(usize_as_f64(phase.members.len())),
        ),
        (
            "requiresExpansion",
            JsValue::from_bool(phase_requires_expansion(phase)),
        ),
        ("expandOnRowClick", JsValue::TRUE),
        ("previewChevron", JsValue::FALSE),
        ("keepContentWhenOpen", JsValue::TRUE),
        (
            "className",
            JsValue::from_str("seekdeep-workflow-run-phase"),
        ),
        (
            "rowClassName",
            JsValue::from_str("seekdeep-workflow-run-phaseHeader"),
        ),
        (
            "leadingClassName",
            JsValue::from_str("seekdeep-workflow-run-phaseLeading"),
        ),
        (
            "titleClassName",
            JsValue::from_str("seekdeep-workflow-run-phaseTitle"),
        ),
        ("collapsedContent", collapsed),
    ])?;
    component(
        &modules.components.react,
        &modules.status_disclosure,
        Some(&props),
        &[member_list],
    )
}

fn render_member_row(
    modules: &BrowserModules,
    member: &WorkflowRunMemberData,
    navigable: bool,
    open_session: &Function,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let name = readable_member(&member.label, translate)?;
    let dot = render_state_dot(modules, member.status)?;
    let dot_slot = tag(
        &modules.components.react,
        "span",
        Some(&class("seekdeep-workflow-run-dotSlot")?),
        &[dot],
    )?;
    let label = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-memberLabel"),
            ),
            ("data-member-label", JsValue::from_str("")),
        ])?),
        std::slice::from_ref(&name),
    )?;
    let label_wrap = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-memberLabelWrap"),
            ),
            ("data-member-label-wrap", JsValue::from_str("")),
        ])?),
        &[label],
    )?;
    let status = tag(
        &modules.components.react,
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-memberStatus"),
            ),
            ("data-member-status-text", JsValue::from_str("")),
        ])?),
        &[translated(translate, status_key(member.status))?],
    )?;
    let children = [dot_slot, label_wrap, status];
    if !navigable {
        return tag(
            &modules.components.react,
            "div",
            Some(&object(&[
                ("key", JsValue::from_f64(u64_as_f64(member.seq))),
                (
                    "className",
                    JsValue::from_str("seekdeep-workflow-run-memberRow"),
                ),
                (
                    "data-member-status",
                    JsValue::from_str(status_name(member.status)),
                ),
            ])?),
            &children,
        );
    }
    let opener = open_session.clone();
    let child_id = member.child_id.to_string();
    let open = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        opener.call1(&JsValue::UNDEFINED, &JsValue::from_str(&child_id))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        &modules.components.react,
        "button",
        Some(&object(&[
            ("key", JsValue::from_f64(u64_as_f64(member.seq))),
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-workflow-run-memberButton"),
            ),
            (
                "data-member-status",
                JsValue::from_str(status_name(member.status)),
            ),
            (
                "aria-label",
                translated_value(translate, "member.open", "name", name)?,
            ),
            ("onClick", open.into_js_value()),
        ])?),
        &children,
    )
}

fn render_state_dot(
    modules: &BrowserModules,
    status: WorkflowRunStatus,
) -> Result<JsValue, JsValue> {
    component(
        &modules.components.react,
        &modules.components.state_dot,
        Some(&object(&[(
            "state",
            JsValue::from_str(dot_state_name(workflow_dot_state(status))),
        )])?),
        &[],
    )
}

fn navigable_from_js(
    sessions: &JsValue,
    phases: &[WorkflowRunPhaseData],
    parent_id: &SessionId,
) -> Result<Array, JsValue> {
    let ordinary = Array::from(&required(sessions, "ids", "Session list state")?)
        .iter()
        .map(|id| {
            id.as_string()
                .map(SessionId::new)
                .ok_or_else(|| js_sys::Error::new("Session list id must be a string").into())
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    let by_id = required(sessions, "byId", "Session list state")?;
    let mut seen = BTreeSet::new();
    let mut summaries = Vec::new();
    for member in phases.iter().flat_map(|phase| &phase.members) {
        if !seen.insert(member.child_id.clone()) {
            continue;
        }
        let summary = Reflect::get(&by_id, &JsValue::from_str(member.child_id.as_str()))?;
        if summary.is_null() || summary.is_undefined() {
            continue;
        }
        let parent_id = Reflect::get(&summary, &JsValue::from_str("parentId"))?
            .as_string()
            .map(SessionId::new);
        summaries.push(WorkflowSessionSummary {
            id: member.child_id.clone(),
            subagent: Reflect::get(&summary, &JsValue::from_str("origin"))?
                .as_string()
                .as_deref()
                == Some("subagent"),
            parent_id,
            running: Reflect::get(&summary, &JsValue::from_str("running"))?.as_bool() == Some(true),
        });
    }
    let selected = Array::new();
    for id in navigable_members(&ordinary, &summaries, phases, parent_id) {
        selected.push(&JsValue::from_str(id.as_str()));
    }
    Ok(selected)
}

fn phase_status_summary(
    phase: &WorkflowRunPhaseData,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    let labels = phase_status_counts(phase)
        .into_iter()
        .map(|(status, count)| {
            translated_value(
                translate,
                status_count_key(status),
                "count",
                JsValue::from_f64(usize_as_f64(count)),
            )?
            .as_string()
            .ok_or_else(|| {
                js_sys::Error::new("workflow status count must translate to a string").into()
            })
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    Ok(JsValue::from_str(&labels.join(" · ")))
}

fn member_count(count: usize, translate: &Function) -> Result<JsValue, JsValue> {
    translated_value(
        translate,
        if count == 1 {
            "run.members.one"
        } else {
            "run.members.other"
        },
        "count",
        JsValue::from_f64(usize_as_f64(count)),
    )
}

fn readable_phase(phase: Option<&str>, translate: &Function) -> Result<JsValue, JsValue> {
    match phase {
        None => translated(translate, "phase.unassigned"),
        Some("") => translated(translate, "phase.empty"),
        Some(phase) => Ok(JsValue::from_str(phase)),
    }
}

fn readable_member(label: &str, translate: &Function) -> Result<JsValue, JsValue> {
    if label.is_empty() {
        translated(translate, "member.empty")
    } else {
        Ok(JsValue::from_str(label))
    }
}

const fn status_name(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

const fn status_key(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "status.running",
        WorkflowRunStatus::Completed => "status.completed",
        WorkflowRunStatus::Failed => "status.failed",
        WorkflowRunStatus::Cancelled => "status.cancelled",
        WorkflowRunStatus::Interrupted => "status.interrupted",
    }
}

const fn status_count_key(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "statusCount.running",
        WorkflowRunStatus::Completed => "statusCount.completed",
        WorkflowRunStatus::Failed => "statusCount.failed",
        WorkflowRunStatus::Cancelled => "statusCount.cancelled",
        WorkflowRunStatus::Interrupted => "statusCount.interrupted",
    }
}

const fn dot_state_name(state: WorkflowDotState) -> &'static str {
    match state {
        WorkflowDotState::Ongoing => "ongoing",
        WorkflowDotState::Done => "done",
        WorkflowDotState::Error => "error",
        WorkflowDotState::Warning => "warning",
    }
}

fn own_locale_dictionaries(ctx: &JsValue, locale: &JsValue) -> Result<(), JsValue> {
    let zh = dictionary(WORKFLOW_RUN_ZH)?;
    let en = dictionary(WORKFLOW_RUN_EN)?;
    let dictionaries = object(&[("zh", zh.into()), ("en", en.into())])?;
    let locale = locale.clone();
    let installer = Closure::wrap(Box::new(move || {
        call_method(
            &locale,
            "register",
            &[
                JsValue::from_str(WORKFLOW_RUN_NS),
                dictionaries.clone().into(),
            ],
        )
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    call_method(
        ctx,
        "effect",
        &[
            installer.into_js_value(),
            JsValue::from_str("ui-workflow-run: dictionaries"),
        ],
    )?;
    Ok(())
}

fn inject_styles() -> Result<(), JsValue> {
    const PACKAGE: &str = "@seekdeep-ai/seekdeep-client-ui-workflow-run";
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
        &JsValue::from_str(WORKFLOW_RUN_STYLES),
    )?;
    let head = required(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-workflow-run is not configured").into())
    })
}

fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn translated_value(
    translate: &Function,
    key: &str,
    field: &str,
    value: JsValue,
) -> Result<JsValue, JsValue> {
    translate.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(key),
        &object(&[(field, value)])?.into(),
    )
}

fn fragment(react: &JsValue, children: &[JsValue]) -> Result<JsValue, JsValue> {
    let fragment = required(react, "Fragment", "React")?;
    element(react, &fragment, None, children)
}

fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, &JsValue::from_str(name), props, children)
}

fn component(
    react: &JsValue,
    component: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, component, props, children)
}

fn element(
    react: &JsValue,
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
    required_function(react, "createElement", "React")?.apply(react, &args)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn clone_object(value: &JsValue) -> Object {
    Object::assign(&Object::new(), &Object::from(value.clone()))
}

fn dictionary<const N: usize>(entries: [(&str, &str); N]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, &JsValue::from_str(entry))?;
    }
    Ok(value)
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

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn usize_as_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn js_error_from_display(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
