//! React component functions whose state and branching are owned by Rust/WASM.

use std::collections::{BTreeMap, BTreeSet};

use js_sys::{Array, Function, Map, Object, Promise, Reflect, Set};
use seekdeep_cordis_client_runner::{
    CordisRunActivity, CordisRunFailure, DynamicCordisLivePackage,
};
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisInventoryRow, DynamicCordisRenderFailure, DynamicCordisRunMode,
};
use seekdeep_identity::SessionId;
use serde::Deserialize;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    CordisPanelAction, CordisPanelRowModel, CordisRunCardPointer, CordisSourceTab,
    CordisToolViewKey, ToolCallBlock, cordis_action_card, cordis_action_row_model,
    cordis_define_card, cordis_define_row_model, cordis_panel_model, cordis_run_card,
    cordis_run_row_model, package_of,
};

const DEFINE_PREFIX: &str = "seekdeep-cordis-define-";
const RUN_PREFIX: &str = "seekdeep-cordis-run-";
const PANEL_PREFIX: &str = "seekdeep-cordis-panel-";

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
        let create = function(&self.react, "createElement")?;
        let arguments = Array::new();
        arguments.push(kind);
        arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            arguments.push(child);
        }
        create.apply(&self.react, &arguments)
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
        let kind = required(&self.primitives, name)?;
        self.element(&kind, props, children)
    }

    fn hook(&self, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
        let hook = function(&self.react, name)?;
        let values = Array::new();
        for argument in arguments {
            values.push(argument);
        }
        hook.apply(&self.react, &values)
    }
}

/// Builds the four UI component functions registered by the browser plugin.
///
/// `react` and `primitives` must be the page's existing module instances so Hooks and
/// design-system components retain one owner.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = cordisUiComponents)]
#[allow(clippy::needless_pass_by_value)]
pub fn cordis_ui_components(react: JsValue, primitives: JsValue) -> Result<JsValue, JsValue> {
    let ui = ReactUi { react, primitives };
    let output = Object::new();
    set(&output, "CordisActionRow", &action_component(ui.clone()))?;
    set(&output, "CordisRunRow", &run_component(ui.clone()))?;
    set(&output, "CordisDefineRow", &define_component(ui.clone()))?;
    set(&output, "CordisPanel", &panel_component(ui))?;
    Ok(output.into())
}

fn action_component(ui: ReactUi) -> JsValue {
    let component = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        render_action(&ui, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

fn render_action(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let call_id = string(props, "callId")?;
    let tool_name = string(props, "toolName")?;
    let block = decode::<ToolCallBlock>(required(props, "block")?)?;
    let model = cordis_action_row_model(cordis_action_card(&block), &call_id, &tool_name);
    let card = &model.card;
    let root = object(&[
        ("className", JsValue::from_str(&run_class("card"))),
        ("data-tool", JsValue::from_str(&tool_name)),
        ("data-state", enum_string(&card.state)?),
    ])?;
    let icon_props = Object::new();
    set(&icon_props, "size", &JsValue::from_f64(14.0))?;
    let icon = match card.state {
        crate::CordisToolState::Error => {
            set(&icon_props, "state", &JsValue::from_str("error"))?;
            ui.primitive("StateDot", Some(&icon_props), &[])?
        }
        crate::CordisToolState::Stopped => {
            set(&icon_props, "state", &JsValue::from_str("warning"))?;
            ui.primitive("StateDot", Some(&icon_props), &[])?
        }
        crate::CordisToolState::Running | crate::CordisToolState::Ok if model.remove => {
            ui.primitive("IconTrashOutline16", Some(&icon_props), &[])?
        }
        crate::CordisToolState::Running | crate::CordisToolState::Ok => {
            ui.primitive("IconStopFill16", Some(&icon_props), &[])?
        }
    };
    let icon = ui.tag("span", Some(&class_props(&run_class("icon"))?), &[icon])?;
    let title = ui.tag(
        "span",
        Some(&class_props(&run_class("title"))?),
        &[translate(props, model.title_key, None)?],
    )?;
    let separator = object(&[
        ("className", JsValue::from_str(&run_class("separator"))),
        ("aria-hidden", JsValue::TRUE),
    ])?;
    let separator = ui.tag("span", Some(&separator), &[])?;
    let summary_class = if card.error_summary.is_some() {
        run_class("error")
    } else {
        run_class("summary")
    };
    let summary = ui.tag(
        "span",
        Some(&class_props(&summary_class)?),
        &[JsValue::from_str(&model.summary)],
    )?;
    let mut row_children = vec![icon, title, separator, summary];
    if let Some(inspect) = optional_function(props, "inspect")? {
        let inspect_props = object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&run_class("inspect"))),
            ("aria-label", JsValue::from_str("Inspect")),
            ("onClick", inspect.into()),
        ])?;
        row_children.push(ui.tag("button", Some(&inspect_props), &[inspect_icon(ui)?])?);
    }
    let row = ui.tag("div", Some(&class_props(&run_class("row"))?), &row_children)?;
    let mut children = vec![row];
    if let Some(output) = &card.output {
        children.push(ui.tag(
            "pre",
            Some(&class_props(&run_class("output"))?),
            &[JsValue::from_str(output)],
        )?);
    }
    ui.tag("div", Some(&root), &children)
}

fn run_component(ui: ReactUi) -> JsValue {
    let component = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        render_run(&ui, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_run(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let call_id = string(props, "callId")?;
    let block = decode::<ToolCallBlock>(required(props, "block")?)?;
    let card = cordis_run_card(&block);
    let inventory = hook_snapshot(props, "useInventory")?;
    let rows = decode::<Vec<DynamicCordisInventoryRow>>(required(&inventory, "rows")?)?;
    let removed = string_set(&required(&inventory, "removed")?);
    let loaded = live_packages(hook_snapshot(props, "useLoaded")?)?;
    let latest_value = hook_snapshot(props, "useRunCards")?;
    let latest = pointer_map(&latest_value)?;
    let active_value = hook_snapshot(props, "useActiveRuns")?;
    let active_runs = activity_map(&active_value)?;
    let model = cordis_run_row_model(
        card,
        &call_id,
        &rows,
        &removed,
        &loaded,
        &latest,
        &active_runs,
    );
    observe_run_card(ui, props, &call_id, &model)?;

    let card = &model.card;
    let mut root_pairs = vec![
        ("className", JsValue::from_str(&run_class("card"))),
        ("data-tool", JsValue::from_str("cordis_run")),
        ("data-state", enum_string(&card.state)?),
        (
            "data-cordis-status",
            JsValue::from_str(run_reading(model.reading)),
        ),
    ];
    optional_attr(
        &mut root_pairs,
        "data-cordis-plugin-id",
        card.plugin_id.as_ref().map(ToString::to_string),
    );
    optional_attr(
        &mut root_pairs,
        "data-cordis-package-id",
        card.package_id.as_ref().map(ToString::to_string),
    );
    optional_attr(
        &mut root_pairs,
        "data-cordis-run-id",
        card.plugin_run_id.as_ref().map(ToString::to_string),
    );
    let root = object(&root_pairs)?;
    let icon_props = Object::new();
    set(&icon_props, "size", &JsValue::from_f64(14.0))?;
    let icon = match card.state {
        crate::CordisToolState::Error => {
            set(&icon_props, "state", &JsValue::from_str("error"))?;
            ui.primitive("StateDot", Some(&icon_props), &[])?
        }
        crate::CordisToolState::Stopped => {
            set(&icon_props, "state", &JsValue::from_str("warning"))?;
            ui.primitive("StateDot", Some(&icon_props), &[])?
        }
        crate::CordisToolState::Running | crate::CordisToolState::Ok => {
            ui.primitive("IconCodeOutline16", Some(&icon_props), &[])?
        }
    };
    let icon = ui.tag("span", Some(&class_props(&run_class("icon"))?), &[icon])?;
    let title_key =
        if card.mode == Some(seekdeep_cordis_dynamic_types::DynamicCordisRunMode::Update) {
            "row.updateTitle"
        } else {
            "row.runTitle"
        };
    let title = ui.tag(
        "span",
        Some(&class_props(&run_class("title"))?),
        &[translate(props, title_key, None)?],
    )?;
    let separator = object(&[
        ("className", JsValue::from_str(&run_class("separator"))),
        ("aria-hidden", JsValue::TRUE),
    ])?;
    let separator = ui.tag("span", Some(&separator), &[])?;
    let summary_class = if card.error_summary.is_some() {
        run_class("error")
    } else {
        run_class("summary")
    };
    let summary = ui.tag(
        "span",
        Some(&class_props(&summary_class)?),
        &[JsValue::from_str(&model.summary)],
    )?;
    let status = ui.tag(
        "span",
        Some(&class_props(&run_class("status"))?),
        &[translate(props, model.reading.locale_key(), None)?],
    )?;
    let mut row_children = vec![icon, title, separator, summary, status];
    if let Some(inspect) = optional_function(props, "inspect")? {
        let inspect_props = object(&[
            ("type", JsValue::from_str("button")),
            ("className", JsValue::from_str(&run_class("inspect"))),
            ("aria-label", JsValue::from_str("Inspect")),
            ("onClick", inspect.into()),
        ])?;
        row_children.push(ui.tag("button", Some(&inspect_props), &[inspect_icon(ui)?])?);
    }
    let row = ui.tag("div", Some(&class_props(&run_class("row"))?), &row_children)?;
    let mut children = vec![row];
    match model.reading {
        crate::CordisRunReading::Removed => children.push(message(ui, props, "run.removed")?),
        crate::CordisRunReading::Superseded => {
            children.push(message(ui, props, "run.superseded")?);
        }
        crate::CordisRunReading::Failed => {
            if let Some(failure) = &model.failure_message {
                children.push(ui.tag(
                    "div",
                    Some(&class_props(&run_class("message"))?),
                    &[JsValue::from_str(failure)],
                )?);
            }
        }
        crate::CordisRunReading::Idle
        | crate::CordisRunReading::AwaitingApproval
        | crate::CordisRunReading::ClientPending
        | crate::CordisRunReading::Running => {}
    }
    if model.show_business {
        children.push(render_business(ui, props, &model)?);
    } else if !matches!(
        model.reading,
        crate::CordisRunReading::Removed | crate::CordisRunReading::Superseded
    ) && let Some(output) = &card.output
    {
        children.push(output_block(ui, output)?);
    }
    ui.tag("div", Some(&root), &children)
}

fn observe_run_card(
    ui: &ReactUi,
    props: &JsValue,
    call_id: &str,
    model: &crate::CordisRunRowModel,
) -> Result<(), JsValue> {
    let callback = required_function(props, "onObserveRunCard")?;
    let callback_dependency = callback.clone();
    let key = model.key.as_ref().map(|key| key.as_str().to_owned());
    let seq = model.card.seq;
    let plugin_run_id = model.card.plugin_run_id.as_ref().map(ToString::to_string);
    let call_id = call_id.to_owned();
    let effect_key = key.clone();
    let effect_plugin_run_id = plugin_run_id.clone();
    let effect_call_id = call_id.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        if let (Some(key), Some(seq), Some(plugin_run_id)) =
            (effect_key.as_deref(), seq, effect_plugin_run_id.as_deref())
            && let Ok(pointer) = object(&[
                ("key", JsValue::from_str(key)),
                ("callId", JsValue::from_str(&effect_call_id)),
                ("seq", js_number(seq)),
                ("pluginRunId", JsValue::from_str(plugin_run_id)),
            ])
        {
            let _ = callback.call1(&JsValue::UNDEFINED, &pointer);
        }
        JsValue::UNDEFINED
    }) as Box<dyn FnMut() -> JsValue>);
    let dependencies = Array::new();
    dependencies.push(&JsValue::from_str(call_id.as_str()));
    dependencies.push(&key.as_deref().map_or(JsValue::NULL, JsValue::from_str));
    dependencies.push(&seq.map_or(JsValue::NULL, js_number));
    dependencies.push(
        &plugin_run_id
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    dependencies.push(&callback_dependency);
    ui.hook("useEffect", &[effect.into_js_value(), dependencies.into()])?;
    Ok(())
}

fn render_business(
    ui: &ReactUi,
    props: &JsValue,
    model: &crate::CordisRunRowModel,
) -> Result<JsValue, JsValue> {
    let card = &model.card;
    let key = model.key.as_ref().expect("show_business requires key");
    let render_slot = required_function(props, "renderSlot")?;
    let owner = object(&[
        (
            "pluginId",
            JsValue::from_str(card.plugin_id.as_ref().unwrap().as_str()),
        ),
        (
            "packageId",
            JsValue::from_str(card.package_id.as_ref().unwrap().as_str()),
        ),
        (
            "pluginRunId",
            JsValue::from_str(card.plugin_run_id.as_ref().unwrap().as_str()),
        ),
    ])?;
    let fallback = card
        .output
        .as_ref()
        .map(|output| output_block(ui, output))
        .transpose()?
        .unwrap_or(JsValue::NULL);
    let options = object(&[
        ("entryKey", JsValue::from_str(key.as_str())),
        ("fallback", fallback),
    ])?;
    let rendered = render_slot.call3(
        &JsValue::UNDEFINED,
        &JsValue::from_str("tool.view.cordis"),
        &owner,
        &options,
    )?;
    let business_props = object(&[
        ("className", JsValue::from_str(&run_class("business"))),
        ("data-cordis-business-view", JsValue::from_str(key.as_str())),
    ])?;
    ui.tag("div", Some(&business_props), &[rendered])
}

fn define_component(ui: ReactUi) -> JsValue {
    let component = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        render_define(&ui, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

fn panel_component(ui: ReactUi) -> JsValue {
    let component = Closure::wrap(Box::new(move |props: JsValue| -> Result<JsValue, JsValue> {
        render_panel(&ui, &props)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    component.into_js_value()
}

fn render_define(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let call_id = string(props, "callId")?;
    let block = decode::<ToolCallBlock>(required(props, "block")?)?;
    let card = cordis_define_card(&block);
    let inventory = hook_snapshot(props, "useInventory")?;
    let rows = decode::<Vec<DynamicCordisInventoryRow>>(required(&inventory, "rows")?)?;
    let removed = string_set(&required(&inventory, "removed")?);
    let loaded = live_packages(hook_snapshot(props, "useLoaded")?)?;

    let expanded_state = Array::from(&ui.hook("useState", &[JsValue::FALSE])?);
    let expanded = expanded_state.get(0).as_bool().unwrap_or(false);
    let set_expanded = expanded_state.get(1).dyn_into::<Function>()?;
    let initial_source = if card.client_code.is_some() {
        "client"
    } else {
        "host"
    };
    let source_state = Array::from(&ui.hook("useState", &[JsValue::from_str(initial_source)])?);
    let selected_source = match source_state.get(0).as_string().as_deref() {
        Some("host") => CordisSourceTab::Host,
        _ => CordisSourceTab::Client,
    };
    let set_source = source_state.get(1).dyn_into::<Function>()?;
    let source_panel_id = ui
        .hook("useId", &[])?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("React.useId() did not return a string"))?;
    let model = cordis_define_row_model(card, &call_id, &rows, &removed, &loaded, selected_source);
    let open = expanded && model.expandable;
    let mut root_pairs = vec![
        ("className", JsValue::from_str(&define_class("card"))),
        ("data-tool", JsValue::from_str("cordis_define")),
        ("data-state", enum_string(&model.card.state)?),
        (
            "data-cordis-status",
            JsValue::from_str(define_reading(model.reading)),
        ),
    ];
    if model.reading == crate::CordisDefineReading::Removed {
        root_pairs.push(("data-terminal", JsValue::TRUE));
    }
    optional_attr(
        &mut root_pairs,
        "data-cordis-plugin-id",
        model.card.plugin_id.as_ref().map(ToString::to_string),
    );
    optional_attr(
        &mut root_pairs,
        "data-cordis-package-id",
        model.card.package_id.as_ref().map(ToString::to_string),
    );
    let root = object(&root_pairs)?;
    let mut root_children = Vec::new();
    if let Some(a11y) = model.a11y_state_key {
        root_children.push(ui.tag(
            "span",
            Some(&class_props(&define_class("visuallyHidden"))?),
            &[translate(props, a11y, None)?],
        )?);
    }
    let leading = define_leading(ui, model.card.state)?;
    let collapsed = define_collapsed(ui, props, &model)?;
    let next_expanded = !expanded;
    let toggle = Closure::wrap(Box::new(move || {
        let _ = set_expanded.call1(&JsValue::UNDEFINED, &JsValue::from_bool(next_expanded));
    }) as Box<dyn FnMut()>);
    let disclosure_props = object(&[
        ("rowClassName", JsValue::from_str(&define_class("row"))),
        ("titleClassName", JsValue::from_str(&define_class("title"))),
        (
            "chevronClassName",
            JsValue::from_str(&define_class("chevron")),
        ),
        ("icon", leading),
        ("title", translate(props, "row.defineTitle", None)?),
        ("open", JsValue::from_bool(open)),
        ("expandable", JsValue::from_bool(model.expandable)),
        ("expandOnRowClick", JsValue::TRUE),
        ("keepContentWhenOpen", JsValue::TRUE),
        ("onToggle", toggle.into_js_value()),
        ("collapsedContent", collapsed),
    ])?;
    let body = define_body(ui, props, &model, &source_panel_id, &set_source)?;
    root_children.push(ui.primitive("DisclosureRow", Some(&disclosure_props), &[body])?);
    ui.tag("div", Some(&root), &root_children)
}

#[allow(clippy::too_many_lines)]
fn render_panel(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let inventory_value = hook_snapshot(props, "useInventory")?;
    let rows = decode::<Vec<DynamicCordisInventoryRow>>(required(&inventory_value, "rows")?)?;
    let inventory_read = optional_bool(&inventory_value, "read")?.unwrap_or(false);
    let inventory_error = optional_string(&inventory_value, "error")?;
    let active_value = hook_snapshot(props, "useActiveRuns")?;
    let active_runs = activity_map(&active_value)?;
    let run_error_value = hook_snapshot(props, "useRunErrors")?;
    let run_errors = failure_map(&run_error_value)?;
    let loaded = live_packages(hook_snapshot(props, "useLoaded")?)?;
    let render_failure_value = hook_snapshot(props, "useRenderFailures")?;
    let render_failures = render_failure_map(&render_failure_value)?;
    let current_session = current_session(props)?;

    let open_state = Array::from(&ui.hook("useState", &[JsValue::FALSE])?);
    let open = open_state.get(0).as_bool().unwrap_or(false);
    let set_open = open_state.get(1).dyn_into::<Function>()?;
    let selected_state = Array::from(&ui.hook("useState", &[Object::new().into()])?);
    let selected_value = selected_state.get(0);
    let selected = selected_map(&selected_value)?;
    let set_selected = selected_state.get(1).dyn_into::<Function>()?;
    let pending_state = Array::from(&ui.hook("useState", &[Set::new(&JsValue::UNDEFINED).into()])?);
    let pending_value = pending_state.get(0);
    let pending = string_set(&pending_value);
    let set_pending = pending_state.get(1).dyn_into::<Function>()?;
    let action_error_state = Array::from(&ui.hook("useState", &[Map::new().into()])?);
    let action_error_value = action_error_state.get(0);
    let action_errors = string_map(&action_error_value)?;
    let set_action_errors = action_error_state.get(1).dyn_into::<Function>()?;
    let on_refresh = required_function(props, "onRefresh")?;
    panel_effects(
        ui,
        &active_runs,
        &active_value,
        open,
        &set_open,
        &on_refresh,
    )?;

    let model = cordis_panel_model(
        &rows,
        &active_runs,
        &loaded,
        current_session.as_ref(),
        &selected,
        &pending,
    );
    let all_count = model.mine.len() + model.theirs.len();
    if all_count == 0 {
        return Ok(JsValue::NULL);
    }
    let state = PanelStateFunctions {
        set_open,
        set_selected,
        set_pending,
        set_action_errors,
        on_refresh,
    };
    let diagnostics = PanelDiagnostics {
        run_errors: &run_errors,
        action_errors: &action_errors,
        render_failures: &render_failures,
    };
    let mut layer_children = Vec::new();
    if open {
        layer_children.push(render_panel_popup(
            ui,
            props,
            &model,
            inventory_read,
            inventory_error.as_deref(),
            &state,
            &diagnostics,
        )?);
    }
    let wide = optional_bool(props, "wide")?.unwrap_or(false);
    let set_open_click = state.set_open.clone();
    let toggle = Closure::wrap(Box::new(move || {
        let _ = set_open_click.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!open));
    }) as Box<dyn FnMut()>);
    let badge_props = object(&[
        ("type", JsValue::from_str("button")),
        ("className", JsValue::from_str(&panel_class("badge"))),
        ("data-cordis-badge", js_number_usize(all_count)),
        (
            "data-cordis-approval-badge",
            js_number_usize(model.approvals),
        ),
        (
            "data-active",
            if model.approvals > 0 {
                JsValue::TRUE
            } else {
                JsValue::UNDEFINED
            },
        ),
        ("aria-label", translate(props, "panel.plugins.aria", None)?),
        ("aria-expanded", JsValue::from_bool(open)),
        ("onClick", toggle.into_js_value()),
    ])?;
    let mut badge_children = vec![ui.primitive("IconCordisPluginOutline14", None, &[])?];
    if wide {
        badge_children.push(ui.tag(
            "span",
            Some(&class_props(&panel_class("badgeLabel"))?),
            &[translate(props, "panel.trigger", None)?],
        )?);
        let count_params = object(&[("count", js_number_usize(model.running))])?;
        badge_children.push(ui.tag(
            "span",
            Some(&class_props(&panel_class("badgeCount"))?),
            &[translate(props, "panel.runningCount", Some(&count_params))?],
        )?);
    }
    let badge = ui.tag("button", Some(&badge_props), &badge_children)?;
    let footer = ui.tag(
        "div",
        Some(&class_props(&panel_class("footerButtons"))?),
        &[badge],
    )?;
    layer_children.push(footer);
    let layer_class = if wide {
        panel_class("layer")
    } else {
        format!("{} {}", panel_class("layer"), panel_class("rail"))
    };
    ui.tag("div", Some(&class_props(&layer_class)?), &layer_children)
}

#[derive(Clone)]
struct PanelStateFunctions {
    set_open: Function,
    set_selected: Function,
    set_pending: Function,
    set_action_errors: Function,
    on_refresh: Function,
}

struct PanelDiagnostics<'a> {
    run_errors: &'a BTreeMap<CordisDynamicPluginId, CordisRunFailure>,
    action_errors: &'a BTreeMap<CordisDynamicPluginId, String>,
    render_failures: &'a BTreeMap<CordisDynamicPluginId, DynamicCordisRenderFailure>,
}

fn panel_effects(
    ui: &ReactUi,
    active_runs: &BTreeMap<CordisDynamicPluginId, CordisRunActivity>,
    active_value: &JsValue,
    open: bool,
    set_open: &Function,
    on_refresh: &Function,
) -> Result<(), JsValue> {
    let initial = Set::new(&JsValue::UNDEFINED);
    let request_ref = ui.hook("useRef", &[initial.clone().into()])?;
    let previous = Set::from(required(&request_ref, "current")?);
    let now = Set::new(&JsValue::UNDEFINED);
    let mut discovered = false;
    for activity in active_runs.values() {
        if let CordisRunActivity::AwaitingApproval { request_id, .. } = activity {
            let id = JsValue::from_str(request_id.as_str());
            if !previous.has(&id) {
                discovered = true;
            }
            now.add(&id);
        }
    }
    let set_open = set_open.clone();
    let request_ref_effect = request_ref.clone();
    let approval_effect = Closure::wrap(Box::new(move || -> JsValue {
        let _ = Reflect::set(&request_ref_effect, &JsValue::from_str("current"), &now);
        if discovered {
            let _ = set_open.call1(&JsValue::UNDEFINED, &JsValue::TRUE);
        }
        JsValue::UNDEFINED
    }) as Box<dyn FnMut() -> JsValue>);
    let approval_deps = Array::new();
    approval_deps.push(active_value);
    ui.hook(
        "useEffect",
        &[approval_effect.into_js_value(), approval_deps.into()],
    )?;

    let refresh = on_refresh.clone();
    let initial_refresh = Closure::wrap(Box::new(move || -> JsValue {
        let _ = refresh.call0(&JsValue::UNDEFINED);
        JsValue::UNDEFINED
    }) as Box<dyn FnMut() -> JsValue>);
    let initial_deps = Array::new();
    initial_deps.push(on_refresh);
    ui.hook(
        "useEffect",
        &[initial_refresh.into_js_value(), initial_deps.into()],
    )?;

    let refresh = on_refresh.clone();
    let open_refresh = Closure::wrap(Box::new(move || -> JsValue {
        if open {
            let _ = refresh.call0(&JsValue::UNDEFINED);
        }
        JsValue::UNDEFINED
    }) as Box<dyn FnMut() -> JsValue>);
    let open_deps = Array::new();
    open_deps.push(on_refresh);
    open_deps.push(&JsValue::from_bool(open));
    ui.hook(
        "useEffect",
        &[open_refresh.into_js_value(), open_deps.into()],
    )?;
    Ok(())
}

fn render_panel_popup(
    ui: &ReactUi,
    props: &JsValue,
    model: &crate::CordisPanelModel,
    inventory_read: bool,
    inventory_error: Option<&str>,
    state: &PanelStateFunctions,
    diagnostics: &PanelDiagnostics<'_>,
) -> Result<JsValue, JsValue> {
    let mut body_children = Vec::new();
    if let Some(error) = inventory_error {
        let params = object(&[("message", JsValue::from_str(error))])?;
        let error_props = object(&[
            ("className", JsValue::from_str(&panel_class("readError"))),
            ("role", JsValue::from_str("alert")),
        ])?;
        body_children.push(ui.tag(
            "p",
            Some(&error_props),
            &[translate(props, "panel.readFailed", Some(&params))?],
        )?);
    } else if !inventory_read {
        body_children.push(ui.tag(
            "p",
            Some(&class_props(&panel_class("note"))?),
            &[translate(props, "panel.loading", None)?],
        )?);
    }
    if !model.mine.is_empty() {
        body_children.push(render_panel_group(
            ui,
            props,
            "panel.group.current",
            &model.mine,
            state,
            diagnostics,
        )?);
    }
    if !model.theirs.is_empty() {
        body_children.push(render_panel_group(
            ui,
            props,
            "panel.group.others",
            &model.theirs,
            state,
            diagnostics,
        )?);
    }
    let body = ui.tag(
        "div",
        Some(&class_props(&panel_class("body"))?),
        &body_children,
    )?;
    let title = ui.tag(
        "span",
        Some(&class_props(&panel_class("title"))?),
        &[translate(props, "panel.title", None)?],
    )?;
    let header = ui.tag(
        "header",
        Some(&class_props(&panel_class("header"))?),
        &[title],
    )?;
    let panel_props = object(&[
        ("className", JsValue::from_str(&panel_class("panel"))),
        ("data-cordis-panel", JsValue::TRUE),
        ("aria-label", translate(props, "panel.title", None)?),
    ])?;
    ui.tag("section", Some(&panel_props), &[header, body])
}

fn render_panel_group(
    ui: &ReactUi,
    props: &JsValue,
    label_key: &str,
    rows: &[CordisPanelRowModel],
    state: &PanelStateFunctions,
    diagnostics: &PanelDiagnostics<'_>,
) -> Result<JsValue, JsValue> {
    let heading = ui.tag(
        "h3",
        Some(&class_props(&panel_class("group"))?),
        &[translate(props, label_key, None)?],
    )?;
    let rendered = rows
        .iter()
        .map(|row| render_panel_row(ui, props, row, state, diagnostics))
        .collect::<Result<Vec<_>, _>>()?;
    let list = ui.tag("ul", Some(&class_props(&panel_class("rows"))?), &rendered)?;
    ui.tag("section", None, &[heading, list])
}

fn render_panel_row(
    ui: &ReactUi,
    props: &JsValue,
    row: &CordisPanelRowModel,
    state: &PanelStateFunctions,
    diagnostics: &PanelDiagnostics<'_>,
) -> Result<JsValue, JsValue> {
    let mut row_props = vec![
        ("key", JsValue::from_str(row.plugin_id.as_str())),
        ("className", JsValue::from_str(&panel_class("row"))),
        ("data-cordis-row", JsValue::from_str(row.plugin_id.as_str())),
        (
            "data-cordis-status",
            JsValue::from_str(panel_status_name(row.status)),
        ),
    ];
    if row.awaiting.is_some() {
        row_props.push(("data-cordis-awaiting", JsValue::TRUE));
    }
    let mut children = vec![panel_row_head(ui, props, row)?];
    if let Some(listed) = &row.listed
        && listed.packages.len() > 1
        && let Some(selected) = &row.selected_package_id
    {
        children.push(version_picker(ui, props, row, listed, selected, state)?);
    }
    children.push(panel_row_detail(ui, props, row, state)?);
    if let Some(transition) = panel_transition(ui, props, row, state)? {
        children.push(transition);
    }
    if let Some(failure) = diagnostics.run_errors.get(&row.plugin_id) {
        children.push(alert(
            ui,
            &format!("{} ({})", failure.message, failure_reason(failure)),
        )?);
    } else if let Some(host) = row
        .listed
        .as_ref()
        .and_then(|listed| listed.latest_run.as_ref())
        .filter(|latest| latest.status == seekdeep_cordis_dynamic_types::CordisRunStatus::Failed)
        .and_then(|latest| latest.error.as_ref())
    {
        children.push(alert(
            ui,
            &format!("{} ({})", host.message, diagnostic_phase(host.phase)),
        )?);
    }
    if let Some(error) = diagnostics.action_errors.get(&row.plugin_id) {
        children.push(alert(ui, error)?);
    }
    if let Some(failure) = diagnostics.render_failures.get(&row.plugin_id) {
        children.push(render_failure_alert(ui, props, failure)?);
    }
    if let (Some(active), Some(selected)) = (&row.active_package, &row.selected_package_id)
        && active.package_id != *selected
    {
        children.push(ui.tag(
            "span",
            Some(&class_props(&panel_class("activeVersion"))?),
            &[JsValue::from_str(&format!(
                "{}: {} · {}",
                translated_string(props, "status.running")?,
                active.name,
                active.package_id
            ))],
        )?);
    }
    ui.tag("li", Some(&object(&row_props)?), &children)
}

fn panel_row_head(
    ui: &ReactUi,
    props: &JsValue,
    row: &CordisPanelRowModel,
) -> Result<JsValue, JsValue> {
    let id = ui.tag(
        "span",
        Some(&class_props(&panel_class("rowId"))?),
        &[JsValue::from_str(row.plugin_id.as_str())],
    )?;
    let name = ui.tag(
        "span",
        Some(&class_props(&panel_class("rowName"))?),
        &[JsValue::from_str(&row.name)],
    )?;
    let status = ui.tag(
        "span",
        Some(&class_props(&panel_class("rowStatus"))?),
        &[translate(props, row.status.locale_key(), None)?],
    )?;
    ui.tag(
        "div",
        Some(&class_props(&panel_class("rowHead"))?),
        &[id, name, status],
    )
}

fn version_picker(
    ui: &ReactUi,
    props: &JsValue,
    row: &CordisPanelRowModel,
    listed: &DynamicCordisInventoryRow,
    selected: &CordisDynamicPackageId,
    state: &PanelStateFunctions,
) -> Result<JsValue, JsValue> {
    let plugin_id = row.plugin_id.to_string();
    let setter = state.set_selected.clone();
    let on_change = Closure::wrap(Box::new(move |event: JsValue| {
        let value = Reflect::get(&event, &JsValue::from_str("target"))
            .and_then(|target| Reflect::get(&target, &JsValue::from_str("value")))
            .ok()
            .and_then(|value| value.as_string());
        if let Some(value) = value {
            update_selected(&setter, &plugin_id, &value);
        }
    }) as Box<dyn FnMut(JsValue)>);
    let select_props = object(&[
        ("value", JsValue::from_str(selected.as_str())),
        ("disabled", JsValue::from_bool(row.busy)),
        ("onChange", on_change.into_js_value()),
    ])?;
    let options = listed
        .packages
        .iter()
        .map(|package| {
            let props = object(&[
                ("key", JsValue::from_str(package.package_id.as_str())),
                ("value", JsValue::from_str(package.package_id.as_str())),
            ])?;
            ui.tag(
                "option",
                Some(&props),
                &[JsValue::from_str(&format!(
                    "{} · {}",
                    package.name, package.package_id
                ))],
            )
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    let select = ui.tag("select", Some(&select_props), &options)?;
    let label = ui.tag("span", None, &[translate(props, "panel.version", None)?])?;
    ui.tag(
        "label",
        Some(&class_props(&panel_class("versionPicker"))?),
        &[label, select],
    )
}

#[allow(clippy::too_many_lines)]
fn panel_row_detail(
    ui: &ReactUi,
    props: &JsValue,
    row: &CordisPanelRowModel,
    state: &PanelStateFunctions,
) -> Result<JsValue, JsValue> {
    let purpose = ui.tag(
        "span",
        Some(&class_props(&panel_class("rowPurpose"))?),
        &[JsValue::from_str(&row.purpose)],
    )?;
    let mut actions = Vec::new();
    if let Some(request_id) = &row.awaiting {
        actions.push(panel_row_action(
            ui,
            props,
            "action.approveOnce",
            "data-cordis-approve",
            request_id.as_str(),
            row,
            action_callback(
                props,
                state,
                row,
                "onApprove",
                vec![JsValue::from_str(request_id.as_str()), JsValue::FALSE],
                true,
            )?,
            "IconCheckOutline16",
        )?);
        actions.push(panel_row_action(
            ui,
            props,
            "action.approvePlugin",
            "data-cordis-approve-plugin",
            request_id.as_str(),
            row,
            action_callback(
                props,
                state,
                row,
                "onApprove",
                vec![JsValue::from_str(request_id.as_str()), JsValue::TRUE],
                true,
            )?,
            "DoubleCheck",
        )?);
        actions.push(panel_row_action(
            ui,
            props,
            "action.decline",
            "data-cordis-decline",
            request_id.as_str(),
            row,
            action_callback(
                props,
                state,
                row,
                "onDecline",
                vec![JsValue::from_str(request_id.as_str())],
                true,
            )?,
            "IconCloseOutline16",
        )?);
    } else {
        if row.actions.contains(&CordisPanelAction::RunSelected)
            && let (Some(listed), Some(package_id), Some(package)) =
                (&row.listed, &row.selected_package_id, &row.selected_package)
        {
            actions.push(panel_row_action(
                ui,
                props,
                "action.run",
                "data-cordis-switch",
                "run",
                row,
                action_callback(
                    props,
                    state,
                    row,
                    "onRun",
                    vec![run_request(
                        &listed.agent_id,
                        &row.plugin_id,
                        package_id,
                        row.run_mode,
                        package.has_client_half,
                    )?],
                    false,
                )?,
                "IconPlayOutline16",
            )?);
        }
        if row.actions.contains(&CordisPanelAction::RetryClient)
            && let (Some(listed), Some(active)) = (&row.listed, &row.active_package)
        {
            actions.push(panel_row_action(
                ui,
                props,
                "action.run",
                "data-cordis-switch",
                "run",
                row,
                action_callback(
                    props,
                    state,
                    row,
                    "onRun",
                    vec![run_request(
                        &listed.agent_id,
                        &row.plugin_id,
                        &active.package_id,
                        DynamicCordisRunMode::Run,
                        true,
                    )?],
                    false,
                )?,
                "IconPlayOutline16",
            )?);
        }
        if row.actions.contains(&CordisPanelAction::Stop)
            && let Some(listed) = &row.listed
        {
            actions.push(panel_row_action(
                ui,
                props,
                "action.stop",
                "data-cordis-switch",
                "stop",
                row,
                action_callback(
                    props,
                    state,
                    row,
                    "onStop",
                    vec![
                        JsValue::from_str(listed.agent_id.as_str()),
                        JsValue::from_str(row.plugin_id.as_str()),
                    ],
                    false,
                )?,
                "IconStopFill16",
            )?);
        }
        if row.actions.contains(&CordisPanelAction::Remove)
            && let Some(listed) = &row.listed
        {
            actions.push(panel_row_action(
                ui,
                props,
                "action.remove",
                "data-cordis-remove",
                row.plugin_id.as_str(),
                row,
                action_callback(
                    props,
                    state,
                    row,
                    "onRemove",
                    vec![
                        JsValue::from_str(listed.agent_id.as_str()),
                        JsValue::from_str(row.plugin_id.as_str()),
                    ],
                    false,
                )?,
                "IconTrashOutline16",
            )?);
        }
    }
    let actions = ui.tag(
        "div",
        Some(&class_props(&panel_class("rowActions"))?),
        &actions,
    )?;
    ui.tag(
        "div",
        Some(&class_props(&panel_class("rowDetail"))?),
        &[purpose, actions],
    )
}

#[allow(clippy::too_many_arguments)]
fn panel_row_action(
    ui: &ReactUi,
    props: &JsValue,
    label_key: &str,
    data_key: &str,
    data_value: &str,
    row: &CordisPanelRowModel,
    callback: Function,
    icon_name: &str,
) -> Result<JsValue, JsValue> {
    let label = translate(props, label_key, None)?;
    let button_props = object(&[
        ("type", JsValue::from_str("button")),
        ("className", JsValue::from_str(&panel_class("actionButton"))),
        ("aria-label", label.clone()),
        (data_key, JsValue::from_str(data_value)),
        ("disabled", JsValue::from_bool(row.busy)),
        ("onClick", callback.into()),
    ])?;
    let icon = if icon_name == "DoubleCheck" {
        double_check_icon(ui)?
    } else {
        let icon_props = object(&[("size", JsValue::from_f64(14.0))])?;
        ui.primitive(icon_name, Some(&icon_props), &[])?
    };
    let button = ui.tag("button", Some(&button_props), &[icon])?;
    let tooltip_props = object(&[
        ("label", label),
        ("side", JsValue::from_str("bottom")),
        ("delayMs", JsValue::from_f64(500.0)),
    ])?;
    ui.primitive("Tooltip", Some(&tooltip_props), &[button])
}

fn double_check_icon(ui: &ReactUi) -> Result<JsValue, JsValue> {
    let props = object(&[("size", JsValue::from_f64(12.0))])?;
    let first = ui.primitive("IconCheckOutline16", Some(&props), &[])?;
    let second = ui.primitive("IconCheckOutline16", Some(&props), &[])?;
    let span_props = object(&[
        ("className", JsValue::from_str(&panel_class("doubleCheck"))),
        ("aria-hidden", JsValue::TRUE),
    ])?;
    ui.tag("span", Some(&span_props), &[first, second])
}

fn panel_transition(
    ui: &ReactUi,
    props: &JsValue,
    row: &CordisPanelRowModel,
    state: &PanelStateFunctions,
) -> Result<Option<JsValue>, JsValue> {
    let Some(listed) = &row.listed else {
        return Ok(None);
    };
    let Some(next) = listed
        .next_package_id
        .as_ref()
        .filter(|next| Some(*next) != listed.current_package_id.as_ref())
    else {
        return Ok(None);
    };
    if row.awaiting.is_some() {
        return Ok(None);
    }
    let mut labels = Vec::new();
    if let Some(current) = &listed.current_package_id {
        let params = object(&[("packageId", JsValue::from_str(current.as_str()))])?;
        labels.push(ui.tag(
            "span",
            None,
            &[translate(props, "panel.current", Some(&params))?],
        )?);
    } else {
        labels.push(ui.tag("span", None, &[])?);
    }
    let next_params = object(&[("packageId", JsValue::from_str(next.as_str()))])?;
    labels.push(ui.tag(
        "span",
        None,
        &[translate(props, "panel.next", Some(&next_params))?],
    )?);
    let next_package = package_of(listed, next);
    let retry_mode = if listed.current_package_id.is_some() {
        DynamicCordisRunMode::Update
    } else {
        DynamicCordisRunMode::Run
    };
    let retry = transition_button(
        ui,
        props,
        row,
        "action.retry",
        action_callback(
            props,
            state,
            row,
            "onRun",
            vec![run_request(
                &listed.agent_id,
                &row.plugin_id,
                next,
                retry_mode,
                next_package.is_some_and(|package| package.has_client_half),
            )?],
            false,
        )?,
    )?;
    let mut actions = vec![retry];
    if let Some(current) = &listed.current_package_id {
        actions.push(transition_button(
            ui,
            props,
            row,
            "action.rollback",
            action_callback(
                props,
                state,
                row,
                "onRun",
                vec![run_request(
                    &listed.agent_id,
                    &row.plugin_id,
                    current,
                    DynamicCordisRunMode::Run,
                    package_of(listed, current).is_some_and(|package| package.has_client_half),
                )?],
                false,
            )?,
        )?);
    }
    labels.push(ui.tag(
        "div",
        Some(&class_props(&panel_class("transitionActions"))?),
        &actions,
    )?);
    Ok(Some(ui.tag(
        "div",
        Some(&class_props(&panel_class("transition"))?),
        &labels,
    )?))
}

fn transition_button(
    ui: &ReactUi,
    props: &JsValue,
    row: &CordisPanelRowModel,
    label_key: &str,
    callback: Function,
) -> Result<JsValue, JsValue> {
    let button_props = object(&[
        ("type", JsValue::from_str("button")),
        ("disabled", JsValue::from_bool(row.busy)),
        ("onClick", callback.into()),
    ])?;
    ui.tag(
        "button",
        Some(&button_props),
        &[translate(props, label_key, None)?],
    )
}

fn action_callback(
    props: &JsValue,
    state: &PanelStateFunctions,
    row: &CordisPanelRowModel,
    callback_name: &str,
    arguments: Vec<JsValue>,
    close_on_success: bool,
) -> Result<Function, JsValue> {
    use std::{cell::Cell, rc::Rc};

    let callback = required_function(props, callback_name)?;
    let plugin_id = row.plugin_id.to_string();
    let set_pending = state.set_pending.clone();
    let set_errors = state.set_action_errors.clone();
    let set_open = state.set_open.clone();
    let on_refresh = state.on_refresh.clone();
    let gate = Rc::new(Cell::new(false));
    let action = Closure::wrap(Box::new(move || -> Promise {
        if gate.replace(true) {
            return Promise::resolve(&JsValue::UNDEFINED);
        }
        update_set(&set_pending, &plugin_id, true);
        update_map(&set_errors, &plugin_id, None);
        let js_arguments = Array::new();
        for argument in &arguments {
            js_arguments.push(argument);
        }
        let returned = callback
            .apply(&JsValue::UNDEFINED, &js_arguments)
            .unwrap_or_else(|error| Promise::reject(&error).into());
        let promise = Promise::resolve(&returned);
        let plugin_id = plugin_id.clone();
        let set_pending = set_pending.clone();
        let set_errors = set_errors.clone();
        let set_open = set_open.clone();
        let on_refresh = on_refresh.clone();
        let gate = gate.clone();
        future_to_promise(async move {
            let settlement = JsFuture::from(promise).await;
            match settlement {
                Ok(result) => {
                    if action_failure(&result).is_some() {
                        update_map(&set_errors, &plugin_id, action_failure(&result));
                    } else if close_on_success {
                        let _ = set_open.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
                    }
                }
                Err(error) => update_map(&set_errors, &plugin_id, Some(js_error_text(&error))),
            }
            update_set(&set_pending, &plugin_id, false);
            let _ = on_refresh.call0(&JsValue::UNDEFINED);
            gate.set(false);
            Ok(JsValue::UNDEFINED)
        })
    }) as Box<dyn FnMut() -> Promise>);
    Ok(action.into_js_value().unchecked_into())
}

fn run_request(
    session_id: &SessionId,
    plugin_id: &CordisDynamicPluginId,
    package_id: &CordisDynamicPackageId,
    mode: DynamicCordisRunMode,
    has_client_half: bool,
) -> Result<JsValue, JsValue> {
    object(&[
        ("agentId", JsValue::from_str(session_id.as_str())),
        ("pluginId", JsValue::from_str(plugin_id.as_str())),
        ("packageId", JsValue::from_str(package_id.as_str())),
        (
            "mode",
            JsValue::from_str(match mode {
                DynamicCordisRunMode::Run => "run",
                DynamicCordisRunMode::Update => "update",
            }),
        ),
        ("hasClientHalf", JsValue::from_bool(has_client_half)),
    ])
    .map(Into::into)
}

fn update_set(setter: &Function, plugin_id: &str, insert: bool) {
    let plugin_id = plugin_id.to_owned();
    let update = Closure::wrap(Box::new(move |current: JsValue| -> JsValue {
        let next = Set::new(&current);
        if insert {
            next.add(&JsValue::from_str(&plugin_id));
        } else {
            next.delete(&JsValue::from_str(&plugin_id));
        }
        next.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    let _ = setter.call1(&JsValue::UNDEFINED, &update.into_js_value());
}

fn update_map(setter: &Function, plugin_id: &str, message: Option<String>) {
    let plugin_id = plugin_id.to_owned();
    let update = Closure::wrap(Box::new(move |current: JsValue| -> JsValue {
        let next = Map::new();
        if let Some(iterator) = js_sys::try_iter(&current).ok().flatten() {
            for entry in iterator.flatten() {
                let entry = Array::from(&entry);
                next.set(&entry.get(0), &entry.get(1));
            }
        }
        if let Some(message) = &message {
            next.set(&JsValue::from_str(&plugin_id), &JsValue::from_str(message));
        } else {
            next.delete(&JsValue::from_str(&plugin_id));
        }
        next.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    let _ = setter.call1(&JsValue::UNDEFINED, &update.into_js_value());
}

fn update_selected(setter: &Function, plugin_id: &str, package_id: &str) {
    let plugin_id = plugin_id.to_owned();
    let package_id = package_id.to_owned();
    let update = Closure::wrap(Box::new(move |current: JsValue| -> JsValue {
        let next = Object::assign(&Object::new(), &Object::from(current));
        let _ = Reflect::set(
            &next,
            &JsValue::from_str(&plugin_id),
            &JsValue::from_str(&package_id),
        );
        next.into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    let _ = setter.call1(&JsValue::UNDEFINED, &update.into_js_value());
}

fn action_failure(result: &JsValue) -> Option<String> {
    if result.is_undefined() || result.is_null() {
        return None;
    }
    let ok = Reflect::get(result, &JsValue::from_str("ok"))
        .ok()
        .and_then(|value| value.as_bool());
    if ok != Some(false) {
        return None;
    }
    Reflect::get(result, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| Some("operation failed".to_owned()))
}

fn alert(ui: &ReactUi, message: &str) -> Result<JsValue, JsValue> {
    let props = object(&[
        ("className", JsValue::from_str(&panel_class("rowError"))),
        ("role", JsValue::from_str("alert")),
    ])?;
    ui.tag("div", Some(&props), &[JsValue::from_str(message)])
}

fn render_failure_alert(
    ui: &ReactUi,
    props: &JsValue,
    failure: &DynamicCordisRenderFailure,
) -> Result<JsValue, JsValue> {
    let label_key = if failure.abdicated {
        "render.failedAbdicated"
    } else {
        "render.failedHeld"
    };
    let params = object(&[("slot", JsValue::from_str(&failure.slot))])?;
    let label = translated_string_with(props, label_key, Some(&params))?;
    let mut attributes = vec![
        ("className", JsValue::from_str(&panel_class("rowError"))),
        ("role", JsValue::from_str("alert")),
        (
            "data-cordis-render-failure",
            JsValue::from_str(&failure.slot),
        ),
    ];
    if failure.abdicated {
        attributes.push(("data-cordis-render-abdicated", JsValue::TRUE));
    }
    ui.tag(
        "div",
        Some(&object(&attributes)?),
        &[JsValue::from_str(&format!("{label} {}", failure.message))],
    )
}

fn failure_reason(failure: &CordisRunFailure) -> &'static str {
    match failure.reason {
        seekdeep_cordis_client_runner::CordisPageFailureReason::HostHalfFailed => {
            "host-half-failed"
        }
        seekdeep_cordis_client_runner::CordisPageFailureReason::ClientHalfFailed => {
            "client-half-failed"
        }
    }
}

fn diagnostic_phase(phase: seekdeep_cordis_dynamic_types::CordisDiagnosticPhase) -> &'static str {
    use seekdeep_cordis_dynamic_types::CordisDiagnosticPhase;
    match phase {
        CordisDiagnosticPhase::Approval => "approval",
        CordisDiagnosticPhase::HostLoad => "host-load",
        CordisDiagnosticPhase::HostApply => "host-apply",
        CordisDiagnosticPhase::ClientLoad => "client-load",
        CordisDiagnosticPhase::ClientApply => "client-apply",
        CordisDiagnosticPhase::ClientRender => "client-render",
    }
}

fn panel_status_name(status: crate::CordisPanelStatus) -> &'static str {
    match status {
        crate::CordisPanelStatus::Idle => "idle",
        crate::CordisPanelStatus::AwaitingApproval => "awaiting-approval",
        crate::CordisPanelStatus::Failed => "failed",
        crate::CordisPanelStatus::ClientPending => "client-pending",
        crate::CordisPanelStatus::Running => "running",
    }
}

fn panel_class(name: &str) -> String {
    format!("{PANEL_PREFIX}{name}")
}

fn translated_string(props: &JsValue, key: &str) -> Result<String, JsValue> {
    translated_string_with(props, key, None)
}

fn translated_string_with(
    props: &JsValue,
    key: &str,
    parameters: Option<&Object>,
) -> Result<String, JsValue> {
    translate(props, key, parameters)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("ui-cordis translation did not return a string").into())
}

fn message(ui: &ReactUi, props: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    ui.tag(
        "div",
        Some(&class_props(&run_class("message"))?),
        &[translate(props, key, None)?],
    )
}

fn output_block(ui: &ReactUi, output: &str) -> Result<JsValue, JsValue> {
    ui.tag(
        "pre",
        Some(&class_props(&run_class("output"))?),
        &[JsValue::from_str(output)],
    )
}

fn inspect_icon(ui: &ReactUi) -> Result<JsValue, JsValue> {
    ui.primitive("IconInspectOutline12", None, &[])
}

fn hook_snapshot(props: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let hook = required_function(props, name)?;
    let selector =
        Closure::wrap(Box::new(|value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    hook.call1(&JsValue::UNDEFINED, &selector.into_js_value())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LivePackageWire {
    plugin_id: CordisDynamicPluginId,
    package_id: CordisDynamicPackageId,
    plugin_run_id: CordisDynamicPluginRunId,
    name: String,
    #[serde(default)]
    slots: Vec<String>,
    #[serde(default)]
    style_count: usize,
}

fn live_packages(value: JsValue) -> Result<Vec<DynamicCordisLivePackage>, JsValue> {
    decode::<Vec<LivePackageWire>>(value).map(|rows| {
        rows.into_iter()
            .map(|row| DynamicCordisLivePackage {
                plugin_id: row.plugin_id,
                package_id: row.package_id,
                plugin_run_id: row.plugin_run_id,
                name: row.name,
                slots: row.slots,
                style_count: row.style_count,
            })
            .collect()
    })
}

fn string_set(value: &JsValue) -> BTreeSet<CordisDynamicPluginId> {
    js_sys::try_iter(value)
        .ok()
        .flatten()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|value| value.as_string())
        .map(CordisDynamicPluginId::new)
        .collect()
}

fn pointer_map(
    value: &JsValue,
) -> Result<BTreeMap<CordisToolViewKey, CordisRunCardPointer>, JsValue> {
    map_entries(value, |key, value| {
        let key = CordisToolViewKey::new(key);
        Ok((
            key.clone(),
            CordisRunCardPointer {
                key,
                call_id: string(&value, "callId")?,
                seq: number(&value, "seq")?,
                plugin_run_id: CordisDynamicPluginRunId::new(string(&value, "pluginRunId")?),
            },
        ))
    })
}

fn activity_map(
    value: &JsValue,
) -> Result<BTreeMap<CordisDynamicPluginId, CordisRunActivity>, JsValue> {
    map_entries(value, |key, value| {
        Ok((CordisDynamicPluginId::new(key), decode(value)?))
    })
}

fn failure_map(
    value: &JsValue,
) -> Result<BTreeMap<CordisDynamicPluginId, CordisRunFailure>, JsValue> {
    map_entries(value, |key, value| {
        Ok((CordisDynamicPluginId::new(key), decode(value)?))
    })
}

fn render_failure_map(
    value: &JsValue,
) -> Result<BTreeMap<CordisDynamicPluginId, DynamicCordisRenderFailure>, JsValue> {
    map_entries(value, |key, value| {
        Ok((CordisDynamicPluginId::new(key), decode(value)?))
    })
}

fn string_map(value: &JsValue) -> Result<BTreeMap<CordisDynamicPluginId, String>, JsValue> {
    map_entries(value, |key, value| {
        let message = value
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Cordis action error is not a string"))?;
        Ok((CordisDynamicPluginId::new(key), message))
    })
}

fn selected_map(
    value: &JsValue,
) -> Result<BTreeMap<CordisDynamicPluginId, CordisDynamicPackageId>, JsValue> {
    let mut output = BTreeMap::new();
    for key in Object::keys(&Object::from(value.clone())).iter() {
        let Some(key) = key.as_string() else {
            continue;
        };
        let package_id = Reflect::get(&Object::from(value.clone()), &JsValue::from_str(&key))?
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Cordis selected Package is not a string"))?;
        output.insert(
            CordisDynamicPluginId::new(key),
            CordisDynamicPackageId::new(package_id),
        );
    }
    Ok(output)
}

fn current_session(props: &JsValue) -> Result<Option<SessionId>, JsValue> {
    let hook = required_function(props, "useSessions")?;
    let selector = Closure::wrap(Box::new(|state: JsValue| {
        Reflect::get(&state, &JsValue::from_str("current")).unwrap_or(JsValue::UNDEFINED)
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    Ok(hook
        .call1(&JsValue::UNDEFINED, &selector.into_js_value())?
        .as_string()
        .map(SessionId::new))
}

fn map_entries<K: Ord, V>(
    value: &JsValue,
    mut convert: impl FnMut(String, JsValue) -> Result<(K, V), JsValue>,
) -> Result<BTreeMap<K, V>, JsValue> {
    let mut output = BTreeMap::new();
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("Cordis observable snapshot is not iterable"))?;
    for entry in iterator {
        let entry = Array::from(&entry?);
        let key = entry
            .get(0)
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Cordis Map key is not a string"))?;
        let (key, value) = convert(key, entry.get(1))?;
        output.insert(key, value);
    }
    Ok(output)
}

fn run_reading(reading: crate::CordisRunReading) -> &'static str {
    match reading {
        crate::CordisRunReading::Idle => "idle",
        crate::CordisRunReading::AwaitingApproval => "awaiting-approval",
        crate::CordisRunReading::Failed => "failed",
        crate::CordisRunReading::ClientPending => "client-pending",
        crate::CordisRunReading::Running => "running",
        crate::CordisRunReading::Removed => "removed",
        crate::CordisRunReading::Superseded => "superseded",
    }
}

fn translate(props: &JsValue, key: &str, parameters: Option<&Object>) -> Result<JsValue, JsValue> {
    let t = required_function(props, "t")?;
    parameters.map_or_else(
        || t.call1(&JsValue::UNDEFINED, &JsValue::from_str(key)),
        |parameters| t.call2(&JsValue::UNDEFINED, &JsValue::from_str(key), parameters),
    )
}

fn run_class(name: &str) -> String {
    format!("{RUN_PREFIX}{name}")
}

fn define_class(name: &str) -> String {
    format!("{DEFINE_PREFIX}{name}")
}

fn define_reading(reading: crate::CordisDefineReading) -> &'static str {
    match reading {
        crate::CordisDefineReading::Idle => "idle",
        crate::CordisDefineReading::ClientPending => "client-pending",
        crate::CordisDefineReading::Running => "running",
        crate::CordisDefineReading::Removed => "removed",
    }
}

fn define_leading(ui: &ReactUi, state: crate::CordisToolState) -> Result<JsValue, JsValue> {
    let props = Object::new();
    match state {
        crate::CordisToolState::Error => {
            set(&props, "state", &JsValue::from_str("error"))?;
            ui.primitive("StateDot", Some(&props), &[])
        }
        crate::CordisToolState::Stopped => {
            set(&props, "state", &JsValue::from_str("warning"))?;
            ui.primitive("StateDot", Some(&props), &[])
        }
        crate::CordisToolState::Running | crate::CordisToolState::Ok => {
            set(&props, "size", &JsValue::from_f64(14.0))?;
            ui.primitive("IconCodeOutline16", Some(&props), &[])
        }
    }
}

fn define_collapsed(
    ui: &ReactUi,
    props: &JsValue,
    model: &crate::CordisDefineRowModel,
) -> Result<JsValue, JsValue> {
    let separator_props = object(&[
        ("className", JsValue::from_str(&define_class("separator"))),
        ("aria-hidden", JsValue::TRUE),
    ])?;
    let mut children = vec![ui.tag("span", Some(&separator_props), &[])?];
    let name_class = if model.card.error_summary.is_some() {
        define_class("errorSummary")
    } else {
        define_class("name")
    };
    children.push(ui.tag(
        "span",
        Some(&class_props(&name_class)?),
        &[JsValue::from_str(
            model.card.error_summary.as_deref().unwrap_or(&model.name),
        )],
    )?);
    if model.card.error_summary.is_none() {
        let purpose = match &model.card.purpose {
            Some(purpose) => JsValue::from_str(purpose),
            None => translate(props, "purpose.missing", None)?,
        };
        children.push(ui.tag(
            "span",
            Some(&class_props(&define_class("purpose"))?),
            &[purpose],
        )?);
    }
    if model.card.plugin_id.is_some() {
        let label = ui.tag(
            "span",
            Some(&class_props(&define_class("statusLabel"))?),
            &[translate(props, model.reading.locale_key(), None)?],
        )?;
        children.push(ui.tag(
            "span",
            Some(&class_props(&define_class("readout"))?),
            &[label],
        )?);
    }
    let fragment = required(&ui.react, "Fragment")?;
    ui.element(&fragment, None, &children)
}

fn define_body(
    ui: &ReactUi,
    props: &JsValue,
    model: &crate::CordisDefineRowModel,
    source_panel_id: &str,
    set_source: &Function,
) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    if let Some(active_code) = &model.active_code {
        children.push(define_source(
            ui,
            props,
            model,
            source_panel_id,
            set_source,
            active_code,
        )?);
    }
    if let Some(output) = &model.card.output {
        let label = ui.tag(
            "div",
            Some(&class_props(&define_class("sectionLabel"))?),
            &[translate(props, "body.output", None)?],
        )?;
        let mut output_pairs = vec![("className", JsValue::from_str(&define_class("output")))];
        if model.card.state == crate::CordisToolState::Error {
            output_pairs.push(("data-error", JsValue::TRUE));
        }
        let output = ui.tag(
            "pre",
            Some(&object(&output_pairs)?),
            &[JsValue::from_str(output)],
        )?;
        children.push(ui.tag(
            "section",
            Some(&class_props(&define_class("codeSection"))?),
            &[label, output],
        )?);
    }
    if model.card.plugin_id.is_some() {
        children.push(ui.tag(
            "div",
            Some(&class_props(&define_class("panelHint"))?),
            &[translate(props, "panel.hint", None)?],
        )?);
    }
    if let Some(inspect) = optional_function(props, "inspect")? {
        let button_props = object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&define_class("inspectButton")),
            ),
            ("onClick", inspect.into()),
        ])?;
        children.push(ui.tag(
            "button",
            Some(&button_props),
            &[inspect_icon(ui)?, JsValue::from_str("Inspect")],
        )?);
    }
    ui.tag(
        "div",
        Some(&class_props(&define_class("bodyWrap"))?),
        &children,
    )
}

fn define_source(
    ui: &ReactUi,
    props: &JsValue,
    model: &crate::CordisDefineRowModel,
    source_panel_id: &str,
    set_source: &Function,
    active_code: &str,
) -> Result<JsValue, JsValue> {
    let mut tabs = Vec::new();
    for source in [CordisSourceTab::Client, CordisSourceTab::Host] {
        let name = match source {
            CordisSourceTab::Client => "client",
            CordisSourceTab::Host => "host",
        };
        let available = match source {
            CordisSourceTab::Client => model.card.client_code.is_some(),
            CordisSourceTab::Host => model.card.host_code.is_some(),
        };
        let class_name = if source == model.active_source {
            format!(
                "{} {}",
                define_class("sourceTab"),
                define_class("sourceTabActive")
            )
        } else {
            define_class("sourceTab")
        };
        let setter = set_source.clone();
        let selected = name.to_owned();
        let on_click = Closure::wrap(Box::new(move || {
            let _ = setter.call1(&JsValue::UNDEFINED, &JsValue::from_str(&selected));
        }) as Box<dyn FnMut()>);
        let tab_props = object(&[
            ("key", JsValue::from_str(name)),
            (
                "id",
                JsValue::from_str(&format!("{source_panel_id}-{name}")),
            ),
            ("type", JsValue::from_str("button")),
            ("role", JsValue::from_str("tab")),
            ("aria-controls", JsValue::from_str(source_panel_id)),
            (
                "aria-selected",
                JsValue::from_bool(source == model.active_source),
            ),
            ("className", JsValue::from_str(&class_name)),
            ("disabled", JsValue::from_bool(!available)),
            ("onClick", on_click.into_js_value()),
        ])?;
        let label_key = match source {
            CordisSourceTab::Client => "body.clientCode",
            CordisSourceTab::Host => "body.hostCode",
        };
        tabs.push(ui.tag(
            "button",
            Some(&tab_props),
            &[translate(props, label_key, None)?],
        )?);
    }
    let tab_list_props = object(&[
        ("className", JsValue::from_str(&define_class("sourceTabs"))),
        ("role", JsValue::from_str("tablist")),
        ("aria-label", translate(props, "body.source", None)?),
    ])?;
    let tab_list = ui.tag("div", Some(&tab_list_props), &tabs)?;
    let active_name = match model.active_source {
        CordisSourceTab::Client => "client",
        CordisSourceTab::Host => "host",
    };
    let code_props = object(&[
        ("code", JsValue::from_str(active_code)),
        ("lang", JsValue::from_str("javascript")),
        ("copyLabel", translate(props, "body.copy", None)?),
        ("copiedLabel", translate(props, "body.copied", None)?),
        ("className", JsValue::from_str(&define_class("sourceCode"))),
    ])?;
    let code = ui.primitive("CodeBlock", Some(&code_props), &[])?;
    let panel_props = object(&[
        ("id", JsValue::from_str(source_panel_id)),
        ("className", JsValue::from_str(&define_class("sourcePanel"))),
        ("role", JsValue::from_str("tabpanel")),
        (
            "aria-labelledby",
            JsValue::from_str(&format!("{source_panel_id}-{active_name}")),
        ),
    ])?;
    let panel = ui.tag("div", Some(&panel_props), &[code])?;
    ui.tag(
        "section",
        Some(&class_props(&define_class("sourceCard"))?),
        &[tab_list, panel],
    )
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn object(properties: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let output = Object::new();
    for (key, value) in properties {
        set(&output, key, value)?;
    }
    Ok(output)
}

fn optional_attr(
    pairs: &mut Vec<(&'static str, JsValue)>,
    key: &'static str,
    value: Option<String>,
) {
    if let Some(value) = value {
        pairs.push((key, JsValue::from_str(&value)));
    }
}

fn enum_string<T: serde::Serialize>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn decode<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn required(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Err(js_sys::Error::new(&format!("ui-cordis requires {key:?}")).into())
    } else {
        Ok(property)
    }
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required(value, key)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::Error::new(&format!("ui-cordis requires function {key:?}")).into())
}

fn required_function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    function(value, key)
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Ok(None)
    } else {
        property
            .dyn_into::<Function>()
            .map(Some)
            .map_err(|_| js_sys::Error::new(&format!("ui-cordis {key:?} is not a function")).into())
    }
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::Error::new(&format!("ui-cordis {key:?} is not a string")).into())
    }
}

fn optional_bool(value: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() || property.is_null() {
        Ok(None)
    } else {
        property.as_bool().map(Some).ok_or_else(|| {
            js_sys::Error::new(&format!("ui-cordis {key:?} is not a boolean")).into()
        })
    }
}

fn string(value: &JsValue, key: &str) -> Result<String, JsValue> {
    required(value, key)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("ui-cordis {key:?} is not a string")).into())
}

fn number(value: &JsValue, key: &str) -> Result<u64, JsValue> {
    let number = required(value, key)?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("ui-cordis {key:?} is not a number")))?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return Err(js_sys::Error::new(&format!(
            "ui-cordis {key:?} is not a non-negative integer"
        ))
        .into());
    }
    number.to_string().parse().map_err(|_| {
        js_sys::Error::new(&format!(
            "ui-cordis {key:?} is outside the Rust sequence range"
        ))
        .into()
    })
}

fn js_number(value: u64) -> JsValue {
    JsValue::from_f64(
        value
            .to_string()
            .parse()
            .expect("u64 decimal text is a finite JavaScript number"),
    )
}

fn js_number_usize(value: usize) -> JsValue {
    JsValue::from_f64(
        value
            .to_string()
            .parse()
            .expect("usize decimal text is a finite JavaScript number"),
    )
}

fn js_error_text(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| js_sys::JsString::from(error.clone()).as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set {key:?}")).into())
    }
}
