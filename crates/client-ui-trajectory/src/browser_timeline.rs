//! Controller-backed Rust/WASM React Timeline renderer.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    TimelineControllerSnapshot, TimelinePointerOutcome, TimelineViewportController, TrajectoryCell,
    TrajectoryCellKind, TrajectoryTimeRange, TrajectoryTimelineMode, TrajectoryTimelineModel,
    TrajectoryTurnModel, derive_trajectory_timeline, format_timeline_offset,
    trajectory_browser_modules,
};

const TOOLTIP_DELAY_MS: f64 = 500.0;

/// Returns the compiled controller-backed `TrajectoryTimeline` component.
///
/// # Errors
///
/// Returns before React and Tooltip primitives are configured.
#[wasm_bindgen(js_name = trajectoryTimelineComponent)]
pub fn trajectory_timeline_component() -> Result<JsValue, JsValue> {
    let (react, primitives) = trajectory_browser_modules()?;
    let primitives = primitives.ok_or_else(|| {
        js_sys::Error::new("client-ui-trajectory Timeline requires UI primitives")
    })?;
    let ui = ReactUi { react, primitives };
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_timeline(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

#[allow(clippy::too_many_lines)]
fn controller_face(
    model: TrajectoryTimelineModel,
    mode: TrajectoryTimelineMode,
) -> Result<JsValue, JsValue> {
    let controller = Rc::new(RefCell::new(TimelineViewportController::new(model, mode)));
    let face = Object::new();

    let state_controller = controller.clone();
    let state = Closure::wrap(Box::new(move || {
        serde_wasm_bindgen::to_value(&state_controller.borrow().snapshot())
            .map_err(js_error_from_display)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "snapshot", &state.into_js_value())?;

    let model_controller = controller.clone();
    let set_model = Closure::wrap(Box::new(move |model: JsValue, mode: String| {
        let model: TrajectoryTimelineModel =
            serde_wasm_bindgen::from_value(model).map_err(js_error_from_display)?;
        let mode = parse_mode(&mode)?;
        let mut controller = model_controller.borrow_mut();
        controller.set_model(model);
        controller.set_mode(mode);
        Ok(())
    })
        as Box<dyn FnMut(JsValue, String) -> Result<(), JsValue>>);
    set(&face, "setModel", &set_model.into_js_value())?;

    let wheel_controller = controller.clone();
    let wheel = Closure::wrap(Box::new(move |fraction: f64, delta: f64| {
        wheel_controller.borrow_mut().wheel(fraction, delta);
    }) as Box<dyn FnMut(f64, f64)>);
    set(&face, "wheel", &wheel.into_js_value())?;

    let down_controller = controller.clone();
    let down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        down_controller.borrow_mut().pointer_down(
            i16_member(&event, "button")?,
            i32_member(&event, "pointerId")?,
            number_member(&event, "clientX")?,
            number_member(&event, "left")?,
            number_member(&event, "width")?,
            optional_usize(&event, "recordIndex"),
        );
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "pointerDown", &down.into_js_value())?;

    let move_controller = controller.clone();
    let moved = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        move_controller.borrow_mut().pointer_move(
            i32_member(&event, "pointerId")?,
            number_member(&event, "clientX")?,
            number_member(&event, "left")?,
            number_member(&event, "width")?,
            optional_usize(&event, "recordIndex"),
        );
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "pointerMove", &moved.into_js_value())?;

    let up_controller = controller.clone();
    let up = Closure::wrap(Box::new(move |event: JsValue| -> Result<JsValue, JsValue> {
        let outcome = up_controller.borrow_mut().pointer_up(
            i32_member(&event, "pointerId")?,
            number_member(&event, "clientX")?,
            number_member(&event, "left")?,
            number_member(&event, "width")?,
            optional_usize(&event, "recordIndex"),
        );
        outcome_to_js(&outcome)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    set(&face, "pointerUp", &up.into_js_value())?;

    let cancel_controller = controller.clone();
    let cancel = Closure::wrap(
        Box::new(move || cancel_controller.borrow_mut().pointer_cancel()) as Box<dyn FnMut()>,
    );
    set(&face, "cancel", &cancel.into_js_value())?;
    let leave_controller = controller.clone();
    let leave = Closure::wrap(
        Box::new(move || leave_controller.borrow_mut().pointer_leave()) as Box<dyn FnMut()>,
    );
    set(&face, "leave", &leave.into_js_value())?;
    let hover_controller = controller.clone();
    let clear_hover = Closure::wrap(
        Box::new(move || hover_controller.borrow_mut().clear_hover()) as Box<dyn FnMut()>,
    );
    set(&face, "clearHover", &clear_hover.into_js_value())?;
    let reveal_controller = controller.clone();
    let reveal = Closure::wrap(Box::new(move |index: f64| {
        if let Some(index) = f64_to_usize(index) {
            reveal_controller.borrow_mut().reveal_selected(index);
        }
    }) as Box<dyn FnMut(f64)>);
    set(&face, "reveal", &reveal.into_js_value())?;
    let outside_controller = controller;
    let outside = Closure::wrap(Box::new(move |range: JsValue| -> Result<bool, JsValue> {
        let range: TrajectoryTimeRange =
            serde_wasm_bindgen::from_value(range).map_err(js_error_from_display)?;
        Ok(outside_controller.borrow().range_is_outside(range))
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    set(&face, "rangeOutside", &outside.into_js_value())?;
    Ok(face.into())
}

#[allow(clippy::too_many_lines)]
fn render_timeline(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let turns: Vec<TrajectoryTurnModel> =
        serde_wasm_bindgen::from_value(required(props, "turns", "TrajectoryTimeline")?)
            .map_err(js_error_from_display)?;
    let mode_name = required_string(props, "mode", "TrajectoryTimeline")?;
    let mode = parse_mode(&mode_name)?;
    let model = derive_trajectory_timeline(&turns, mode);
    let fallback_model = TrajectoryTimelineModel {
        start: 0.0,
        end: 0.0,
        spans: Vec::new(),
        turn_boundaries: Vec::new(),
    };

    let controller_ref = use_ref(&ui.react, &JsValue::NULL)?;
    let mut controller = Reflect::get(&controller_ref, &JsValue::from_str("current"))?;
    if controller.is_null() || controller.is_undefined() {
        controller = controller_face(
            model.clone().unwrap_or_else(|| fallback_model.clone()),
            mode,
        )?;
        Reflect::set(&controller_ref, &JsValue::from_str("current"), &controller)?;
    } else {
        call_method(
            &controller,
            "setModel",
            &[
                serde_wasm_bindgen::to_value(model.as_ref().unwrap_or(&fallback_model))
                    .map_err(js_error_from_display)?,
                JsValue::from_str(&mode_name),
            ],
        )?;
    }
    if let Some(selected) = optional(props, "selectedIndex")?
        .filter(|value| !value.is_null())
        .and_then(|value| value.as_f64())
    {
        call_method(&controller, "reveal", &[JsValue::from_f64(selected)])?;
    }
    let state: TimelineControllerSnapshot =
        serde_wasm_bindgen::from_value(call_method(&controller, "snapshot", &[])?)
            .map_err(js_error_from_display)?;
    let (revision, set_revision) = use_state(&ui.react, &JsValue::from_f64(0.0))?;
    let revision = revision.as_f64().unwrap_or_default();
    let (loading, set_loading) = use_state(&ui.react, &JsValue::FALSE)?;
    let loading = loading.as_bool().unwrap_or(false);

    let range: Option<TrajectoryTimeRange> = optional(props, "range")?
        .filter(|value| !value.is_null())
        .map(|value| serde_wasm_bindgen::from_value(value).map_err(js_error_from_display))
        .transpose()?;
    let on_range =
        required(props, "onRangeChange", "TrajectoryTimeline")?.dyn_into::<Function>()?;
    let outside = matches!((&model, range), (Some(model), Some(range))
        if range.end < model.start || range.start > model.end);
    let outside_range = on_range.clone();
    let reconcile = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if outside {
            outside_range.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let dependencies = Array::new();
    let model_dependency = JsValue::from_bool(model.is_some());
    let range_dependency = range.map_or(JsValue::NULL, |range| {
        serde_wasm_bindgen::to_value(&range).unwrap_or(JsValue::NULL)
    });
    dependencies.push(&model_dependency);
    dependencies.push(&range_dependency);
    dependencies.push(on_range.as_ref());
    use_effect(&ui.react, &reconcile.into_js_value(), &dependencies)?;

    let has_earlier = optional(props, "hasEarlierRecords")?
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let load_earlier = optional(props, "onLoadEarlier")?;
    if model.is_none() {
        return render_empty_timeline(
            ui,
            has_earlier,
            loading,
            load_earlier,
            &set_loading,
            &set_revision,
            revision,
        );
    }
    let model = model.expect("checked");
    render_timeline_model(
        ui,
        props,
        &turns,
        &model,
        &controller,
        state,
        range,
        has_earlier,
        loading,
        load_earlier,
        &set_loading,
        &set_revision,
        revision,
        &on_range,
        mode,
    )
}

fn render_empty_timeline(
    ui: &ReactUi,
    has_earlier: bool,
    loading: bool,
    load_earlier: Option<JsValue>,
    set_loading: &Function,
    set_revision: &Function,
    revision: f64,
) -> Result<JsValue, JsValue> {
    let labels = lane_labels(ui)?;
    let mut track_children = vec![ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-timeline-empty")?),
        &[JsValue::from_str("No timing data")],
    )?];
    if has_earlier {
        track_children.push(earlier_history_boundary(
            ui,
            loading,
            load_earlier,
            set_loading,
            set_revision,
            revision,
            None,
        )?);
    }
    let track = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-timeline-track")?),
        &track_children,
    )?;
    let plot = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-timeline-plot")?),
        &[labels, track],
    )?;
    ui.tag(
        "section",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline"),
            ),
            ("aria-label", JsValue::from_str("Trajectory timeline")),
        ])?),
        &[plot],
    )
}

#[allow(clippy::float_cmp, clippy::too_many_arguments, clippy::too_many_lines)]
fn render_timeline_model(
    ui: &ReactUi,
    props: &JsValue,
    turns: &[TrajectoryTurnModel],
    model: &TrajectoryTimelineModel,
    controller: &JsValue,
    state: TimelineControllerSnapshot,
    range: Option<TrajectoryTimeRange>,
    has_earlier: bool,
    loading: bool,
    load_earlier: Option<JsValue>,
    set_loading: &Function,
    set_revision: &Function,
    revision: f64,
    on_range: &Function,
    mode: TrajectoryTimelineMode,
) -> Result<JsValue, JsValue> {
    let domain_duration = state.domain.end - state.domain.start;
    let domain_style = style(&[
        (
            "--trajectory-domain-left",
            format!(
                "{}%",
                -(state.domain.start - model.start) / domain_duration * 100.0
            ),
        ),
        (
            "--trajectory-domain-width",
            format!("{}%", state.full_duration / domain_duration * 100.0),
        ),
    ])?;
    let selected_index = optional(props, "selectedIndex")?
        .filter(|value| !value.is_null())
        .and_then(|value| value.as_f64())
        .and_then(f64_to_usize);
    let search_matches = optional(props, "searchMatchIndexes")?.filter(|value| !value.is_null());
    let active_range = state.draft.or(range);
    let committed = range.map(|range| range_fraction(range, state.domain, model));
    let visible = state
        .draft
        .map(|draft| range_fraction(draft, state.domain, model))
        .or(committed);
    let details = detail_by_index(turns);

    let bump = bump_callback(set_revision, revision);
    let wheel_controller = controller.clone();
    let wheel_bump = bump.clone();
    let on_wheel = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        prevent_default(&event)?;
        let (left, width) = event_rect(&event)?;
        let client_x = number_member(&event, "clientX")?;
        call_method(
            &wheel_controller,
            "wheel",
            &[
                JsValue::from_f64(((client_x - left) / width.max(1.0)).clamp(0.0, 1.0)),
                JsValue::from_f64(number_member(&event, "deltaY")?),
            ],
        )?;
        wheel_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let down_controller = controller.clone();
    let down_bump = bump.clone();
    let on_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let input = pointer_input(&event)?;
        call_method(&down_controller, "pointerDown", &[input])?;
        capture_pointer(&event)?;
        down_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let move_controller = controller.clone();
    let move_bump = bump.clone();
    let on_move = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call_method(&move_controller, "pointerMove", &[pointer_input(&event)?])?;
        move_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let up_controller = controller.clone();
    let up_bump = bump.clone();
    let up_range = on_range.clone();
    let select =
        optional(props, "onRecordSelect")?.and_then(|value| value.dyn_into::<Function>().ok());
    let focus =
        optional(props, "onRecordFocus")?.and_then(|value| value.dyn_into::<Function>().ok());
    let on_up = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let outcome = call_method(&up_controller, "pointerUp", &[pointer_input(&event)?])?;
        if bool_member(&outcome, "hasRangeChange")? {
            up_range.call1(
                &JsValue::UNDEFINED,
                &Reflect::get(&outcome, &JsValue::from_str("range"))?,
            )?;
        }
        if let Some(index) = optional_number(&outcome, "recordSelect").and_then(f64_to_usize)
            && let Some(select) = &select
        {
            select.call1(&JsValue::UNDEFINED, &JsValue::from_f64(usize_as_f64(index)))?;
        }
        if let Some(index) = optional_number(&outcome, "recordFocus").and_then(f64_to_usize)
            && let Some(focus) = &focus
        {
            focus.call1(&JsValue::UNDEFINED, &JsValue::from_f64(usize_as_f64(index)))?;
        }
        up_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let cancel_controller = controller.clone();
    let cancel_bump = bump.clone();
    let on_cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&cancel_controller, "cancel", &[])?;
        cancel_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let leave_controller = controller.clone();
    let leave_bump = bump.clone();
    let on_leave = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&leave_controller, "leave", &[])?;
        leave_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let key_range = on_range.clone();
    let on_key = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if required_string(&event, "key", "keyboard event")? == "Escape" && range.is_some() {
            prevent_default(&event)?;
            key_range.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let clear_range = on_range.clone();
    let on_double = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        prevent_default(&event)?;
        clear_range.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let context_menu = Closure::wrap(Box::new(move |event: JsValue| prevent_default(&event))
        as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let mut track_children = Vec::new();
    if has_earlier && state.domain.start == model.start {
        track_children.push(earlier_history_boundary(
            ui,
            loading,
            load_earlier,
            set_loading,
            set_revision,
            revision,
            Some((controller.clone(), bump.clone())),
        )?);
    }
    if let Some(hover) = state.hover.filter(|hover| hover.record_index.is_none())
        && state.draft.is_none()
    {
        track_children.push(
            ui.tag(
                "div",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-trajectory-timeline-hover-line"),
                    ),
                    ("data-timeline-hover-line", JsValue::from_str("")),
                    ("aria-hidden", JsValue::TRUE),
                    (
                        "style",
                        style(&[(
                            "--trajectory-hover-left",
                            format!("{}%", hover.fraction * 100.0),
                        )])?
                        .into(),
                    ),
                ])?),
                &[],
            )?,
        );
    }
    if let Some(visible) = visible {
        for class_name in [
            "seekdeep-trajectory-timeline-selection",
            "seekdeep-trajectory-timeline-selection-edges",
        ] {
            track_children.push(
                ui.tag(
                    "div",
                    Some(&object(&[
                        ("className", JsValue::from_str(class_name)),
                        (
                            "data-dragging",
                            state
                                .draft
                                .map_or(JsValue::UNDEFINED, |_| JsValue::from_str("true")),
                        ),
                        ("aria-hidden", JsValue::TRUE),
                        (
                            "style",
                            style(&[
                                (
                                    "--trajectory-selection-left",
                                    format!("{}%", visible.start * 100.0),
                                ),
                                (
                                    "--trajectory-selection-width",
                                    format!("{}%", (visible.end - visible.start) * 100.0),
                                ),
                            ])?
                            .into(),
                        ),
                    ])?),
                    &[],
                )?,
            );
        }
    }
    track_children.push(turn_boundaries(ui, model, state, &domain_style)?);
    track_children.push(lanes(
        ui,
        model,
        state,
        &domain_style,
        selected_index,
        search_matches.as_ref(),
        active_range,
        &details,
        mode,
    )?);
    let track = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline-track"),
            ),
            (
                "data-panning",
                if state.panning {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-label",
                JsValue::from_str("Timeline overview; drag horizontally to focus events"),
            ),
            ("tabIndex", JsValue::from_f64(0.0)),
            ("onPointerDown", on_down.into_js_value()),
            ("onPointerMove", on_move.into_js_value()),
            ("onPointerUp", on_up.into_js_value()),
            ("onPointerCancel", on_cancel.into_js_value()),
            ("onPointerLeave", on_leave.into_js_value()),
            ("onKeyDown", on_key.into_js_value()),
            ("onDoubleClick", on_double.into_js_value()),
            ("onContextMenu", context_menu.into_js_value()),
        ])?),
        &track_children,
    )?;
    let plot = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-timeline-plot")?),
        &[lane_labels(ui)?, track],
    )?;
    ui.tag(
        "section",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline"),
            ),
            ("aria-label", JsValue::from_str("Trajectory timeline")),
            ("onWheel", on_wheel.into_js_value()),
        ])?),
        &[plot],
    )
}

fn lane_labels(ui: &ReactUi) -> Result<JsValue, JsValue> {
    let labels = ["Input", "Model", "Tools"]
        .into_iter()
        .map(|label| ui.tag("span", None, &[JsValue::from_str(label)]))
        .collect::<Result<Vec<_>, _>>()?;
    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline-labels"),
            ),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &labels,
    )
}

#[allow(clippy::too_many_arguments)]
fn earlier_history_boundary(
    ui: &ReactUi,
    loading: bool,
    load: Option<JsValue>,
    set_loading: &Function,
    set_revision: &Function,
    revision: f64,
    hover: Option<(JsValue, Function)>,
) -> Result<JsValue, JsValue> {
    let on_load = load
        .filter(|_| !loading)
        .and_then(|load| load.dyn_into::<Function>().ok())
        .map(|load| {
            let set_loading = set_loading.clone();
            let set_revision = set_revision.clone();
            Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                set_loading.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
                let pending = load.call0(&JsValue::UNDEFINED)?;
                let finish_loading = set_loading.clone();
                let finish_revision = set_revision.clone();
                let finally = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                    finish_loading.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
                    finish_revision
                        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(revision + 1.0))?;
                    Ok(())
                })
                    as Box<dyn FnMut() -> Result<(), JsValue>>);
                call_method(
                    &Promise::resolve(&pending).into(),
                    "finally",
                    &[finally.into_js_value()],
                )?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        });
    let enter = hover.map(|(controller, bump)| {
        Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            stop_propagation(&event)?;
            call_method(&controller, "clearHover", &[])?;
            bump.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    });
    let stop_move = Closure::wrap(Box::new(move |event: JsValue| stop_propagation(&event))
        as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let stop_down = Closure::wrap(Box::new(move |event: JsValue| stop_propagation(&event))
        as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let button = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline-earlier"),
            ),
            ("data-earlier-history", JsValue::from_str("")),
            (
                "data-loading",
                if loading {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-label",
                JsValue::from_str(if loading {
                    "Loading earlier history"
                } else {
                    "Load earlier history"
                }),
            ),
            (
                "aria-disabled",
                JsValue::from_bool(loading || on_load.is_none()),
            ),
            (
                "onClick",
                on_load.map_or(JsValue::UNDEFINED, Closure::into_js_value),
            ),
            (
                "onPointerEnter",
                enter.map_or(JsValue::UNDEFINED, Closure::into_js_value),
            ),
            ("onPointerMove", stop_move.into_js_value()),
            ("onPointerDown", stop_down.into_js_value()),
        ])?),
        &[JsValue::from_str("…")],
    )?;
    ui.primitive(
        "Tooltip",
        Some(&object(&[
            (
                "label",
                JsValue::from_str(if loading {
                    "Loading earlier history…"
                } else {
                    "Click to load earlier history"
                }),
            ),
            ("side", JsValue::from_str("right")),
            ("delayMs", JsValue::from_f64(TOOLTIP_DELAY_MS)),
        ])?),
        &[button],
    )
}

fn turn_boundaries(
    ui: &ReactUi,
    model: &TrajectoryTimelineModel,
    state: TimelineControllerSnapshot,
    domain_style: &Object,
) -> Result<JsValue, JsValue> {
    let children = model
        .turn_boundaries
        .iter()
        .filter(|boundary| {
            boundary.time > model.start
                && boundary.time >= state.domain.start
                && boundary.time <= state.domain.end
        })
        .map(|boundary| {
            ui.tag(
                "span",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-trajectory-timeline-turn-boundary"),
                    ),
                    ("data-turn", JsValue::from_f64(u64_as_f64(boundary.turn))),
                    (
                        "style",
                        style(&[(
                            "--trajectory-turn-left",
                            format!(
                                "{}%",
                                (boundary.time - model.start) / state.full_duration * 100.0
                            ),
                        )])?
                        .into(),
                    ),
                ])?),
                &[],
            )
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline-turn-boundaries"),
            ),
            (
                "data-animate-viewport",
                if state.animate_viewport {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("aria-hidden", JsValue::TRUE),
            ("style", domain_style.clone().into()),
        ])?),
        &children,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn lanes(
    ui: &ReactUi,
    model: &TrajectoryTimelineModel,
    state: TimelineControllerSnapshot,
    domain_style: &Object,
    selected_index: Option<usize>,
    search_matches: Option<&JsValue>,
    active_range: Option<TrajectoryTimeRange>,
    details: &std::collections::BTreeMap<usize, TimelineRecordDetail>,
    mode: TrajectoryTimelineMode,
) -> Result<JsValue, JsValue> {
    let children = model
        .spans
        .iter()
        .filter(|span| {
            Some(span.index) == selected_index
                || (span.end >= state.domain.start && span.start <= state.domain.end)
        })
        .map(|span| {
            let left = (span.start - model.start) / state.full_duration;
            let width = (span.end - span.start) / state.full_duration;
            let width_percent = width * 100.0;
            let detail = details.get(&span.index);
            let timing_fraction =
                detail.and_then(|detail| match (detail.ttft_ms, detail.decoding_ms) {
                    (Some(ttft), Some(decoding)) if ttft + decoding > 0.0 => {
                        Some(ttft / (ttft + decoding))
                    }
                    _ => None,
                });
            let search_match = search_matches.map(|matches| {
                call_method(
                    matches,
                    "has",
                    &[JsValue::from_f64(usize_as_f64(span.index))],
                )
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            });
            let selected =
                active_range.map(|range| span.start <= range.end && span.end >= range.start);
            let mut styles = vec![
                ("--trajectory-span-left", format!("{}%", left * 100.0)),
                ("--trajectory-span-width", format!("{width_percent}%")),
                (
                    "--trajectory-span-gap",
                    format!("min({}%, 1px)", width_percent * 0.08),
                ),
                ("--trajectory-span-lane", span.lane.to_string()),
            ];
            if let Some(fraction) = timing_fraction {
                styles.push((
                    "--trajectory-assistant-ttft",
                    format!("{}%", fraction * 100.0),
                ));
            }
            let node = ui.tag(
                "span",
                Some(&object(&[
                    ("aria-hidden", JsValue::TRUE),
                    (
                        "className",
                        JsValue::from_str("seekdeep-trajectory-timeline-span"),
                    ),
                    ("data-timeline-span", JsValue::from_str(span.kind.as_str())),
                    (
                        "data-timeline-record-index",
                        JsValue::from_f64(usize_as_f64(span.index)),
                    ),
                    (
                        "data-assistant-timing",
                        timing_fraction.map_or(JsValue::UNDEFINED, |_| JsValue::from_str("true")),
                    ),
                    (
                        "data-error",
                        if span.is_error {
                            JsValue::TRUE
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                    (
                        "data-equal-duration",
                        if matches!(mode, TrajectoryTimelineMode::Time) {
                            JsValue::TRUE
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                    (
                        "data-current",
                        if Some(span.index) == selected_index {
                            JsValue::TRUE
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                    (
                        "data-hovered",
                        if state.hover.and_then(|hover| hover.record_index) == Some(span.index) {
                            JsValue::TRUE
                        } else {
                            JsValue::UNDEFINED
                        },
                    ),
                    (
                        "data-search-match",
                        search_match.map_or(JsValue::UNDEFINED, |matched| {
                            JsValue::from_str(if matched { "true" } else { "false" })
                        }),
                    ),
                    (
                        "data-selected",
                        selected.map_or(JsValue::UNDEFINED, |selected| {
                            JsValue::from_str(if selected { "true" } else { "false" })
                        }),
                    ),
                    ("style", style_owned(&styles)?.into()),
                ])?),
                &[],
            )?;
            ui.primitive(
                "Tooltip",
                Some(&object(&[
                    (
                        "label",
                        JsValue::from_str(&timeline_tooltip_label(span.kind, detail)),
                    ),
                    ("side", JsValue::from_str("bottom")),
                    ("delayMs", JsValue::from_f64(TOOLTIP_DELAY_MS)),
                ])?),
                &[node],
            )
        })
        .collect::<Result<Vec<_>, JsValue>>()?;
    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-timeline-lanes"),
            ),
            ("data-timeline-domain", JsValue::from_str("")),
            (
                "data-animate-viewport",
                if state.animate_viewport {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            ),
            ("style", domain_style.clone().into()),
        ])?),
        &children,
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct TimelineRecordDetail {
    duration_ms: Option<f64>,
    started_at: Option<f64>,
    ttft_ms: Option<f64>,
    decoding_ms: Option<f64>,
}

fn detail_by_index(
    turns: &[TrajectoryTurnModel],
) -> std::collections::BTreeMap<usize, TimelineRecordDetail> {
    turns
        .iter()
        .flat_map(|turn| &turn.groups)
        .flat_map(|group| &group.cells)
        .map(|cell| (cell.index, timeline_record_detail(cell)))
        .collect()
}

fn timeline_record_detail(cell: &TrajectoryCell) -> TimelineRecordDetail {
    let duration_ms = cell
        .time_seconds
        .filter(|value| value.is_finite())
        .map(|value| (value * 1_000.0).max(0.0));
    let started_at = cell.started_at.filter(|value| value.is_finite());
    let (ttft_ms, decoding_ms) = cell
        .assistant_metrics
        .as_ref()
        .map_or((None, None), |metrics| {
            let (Some(start), Some(first), Some(completed)) = (
                metrics.step_start_time.filter(|value| value.is_finite()),
                metrics.first_token_time.filter(|value| value.is_finite()),
                metrics.completed_time.filter(|value| value.is_finite()),
            ) else {
                return (None, None);
            };
            if !metrics.timing_recorded || first < start || completed < first {
                (None, None)
            } else {
                (Some(first - start), Some(completed - first))
            }
        });
    TimelineRecordDetail {
        duration_ms,
        started_at,
        ttft_ms,
        decoding_ms,
    }
}

fn timeline_tooltip_label(
    kind: TrajectoryCellKind,
    detail: Option<&TimelineRecordDetail>,
) -> String {
    let heading = match kind {
        TrajectoryCellKind::System => "SYSTEM",
        TrajectoryCellKind::User => "USER",
        TrajectoryCellKind::Context => "CONTEXT",
        TrajectoryCellKind::Compacted => "COMPACTED",
        TrajectoryCellKind::Message => "ASSISTANT",
        TrajectoryCellKind::Tool => "TOOL",
        TrajectoryCellKind::Subtool => "SUBTOOL",
    };
    let Some(detail) = detail else {
        return heading.to_owned();
    };
    let duration = detail
        .duration_ms
        .map(|duration| format!("Total {}", format_timeline_offset(duration)));
    let range = detail.started_at.map(|started| {
        detail.duration_ms.map_or_else(
            || format!("Started {}", recorded_time(started)),
            |duration| {
                format!(
                    "{} → {}",
                    recorded_time(started),
                    recorded_time(started + duration)
                )
            },
        )
    });
    let timing = match (detail.ttft_ms, detail.decoding_ms) {
        (Some(ttft), Some(decoding)) => Some(format!(
            "TTFT {} · Decoding {}",
            format_timeline_offset(ttft),
            format_timeline_offset(decoding)
        )),
        _ => None,
    };
    let timing = [duration, timing]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    [
        Some(heading.to_owned()),
        range,
        (!timing.is_empty()).then_some(timing),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

fn recorded_time(timestamp: f64) -> String {
    let function = Function::new_with_args(
        "timestamp",
        "return new Date(timestamp).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit', fractionalSecondDigits: 3 })",
    );
    function
        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(timestamp))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

fn range_fraction(
    range: TrajectoryTimeRange,
    domain: TrajectoryTimeRange,
    model: &TrajectoryTimelineModel,
) -> TrajectoryTimeRange {
    let duration = domain.end - domain.start;
    let start = range.start.max(model.start).min(model.end);
    let end = range.end.max(model.start).min(model.end);
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    TrajectoryTimeRange {
        start: (start - domain.start) / duration,
        end: (end - domain.start) / duration,
    }
}

fn pointer_input(event: &JsValue) -> Result<JsValue, JsValue> {
    let (left, width) = event_rect(event)?;
    object(&[
        ("button", Reflect::get(event, &JsValue::from_str("button"))?),
        (
            "pointerId",
            Reflect::get(event, &JsValue::from_str("pointerId"))?,
        ),
        (
            "clientX",
            Reflect::get(event, &JsValue::from_str("clientX"))?,
        ),
        ("left", JsValue::from_f64(left)),
        ("width", JsValue::from_f64(width)),
        (
            "recordIndex",
            record_index_at(event).map_or(JsValue::NULL, |index| {
                JsValue::from_f64(usize_as_f64(index))
            }),
        ),
    ])
    .map(Into::into)
}

fn event_rect(event: &JsValue) -> Result<(f64, f64), JsValue> {
    let current = required(event, "currentTarget", "pointer event")?;
    let rect = call_method(&current, "getBoundingClientRect", &[])?;
    Ok((
        number_member(&rect, "left")?,
        number_member(&rect, "width")?.max(1.0),
    ))
}

fn record_index_at(event: &JsValue) -> Option<usize> {
    let target = Reflect::get(event, &JsValue::from_str("target")).ok()?;
    let closest = call_method(
        &target,
        "closest",
        &[JsValue::from_str("[data-timeline-record-index]")],
    )
    .ok()?;
    if closest.is_null() {
        return None;
    }
    let dataset = Reflect::get(&closest, &JsValue::from_str("dataset")).ok()?;
    let value = Reflect::get(&dataset, &JsValue::from_str("timelineRecordIndex"))
        .ok()?
        .as_string()?
        .parse::<f64>()
        .ok()?;
    f64_to_usize(value)
}

fn capture_pointer(event: &JsValue) -> Result<(), JsValue> {
    let current = required(event, "currentTarget", "pointer event")?;
    let capture = Reflect::get(&current, &JsValue::from_str("setPointerCapture"))?;
    if let Ok(capture) = capture.dyn_into::<Function>() {
        capture.call1(
            &current,
            &Reflect::get(event, &JsValue::from_str("pointerId"))?,
        )?;
    }
    Ok(())
}

fn outcome_to_js(outcome: &TimelinePointerOutcome) -> Result<JsValue, JsValue> {
    let value = Object::new();
    set(
        &value,
        "hasRangeChange",
        &JsValue::from_bool(outcome.range_change.is_some()),
    )?;
    let range = match outcome.range_change {
        Some(Some(range)) => serde_wasm_bindgen::to_value(&range).map_err(js_error_from_display)?,
        Some(None) | None => JsValue::NULL,
    };
    set(&value, "range", &range)?;
    set_optional_index(&value, "recordSelect", outcome.record_select)?;
    set_optional_index(&value, "recordFocus", outcome.record_focus)?;
    Ok(value.into())
}

fn bump_callback(setter: &Function, revision: f64) -> Function {
    let setter = setter.clone();
    Closure::wrap(Box::new(move || {
        setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(revision + 1.0))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value()
    .unchecked_into()
}

fn prevent_default(event: &JsValue) -> Result<(), JsValue> {
    call_method(event, "preventDefault", &[]).map(|_| ())
}

fn stop_propagation(event: &JsValue) -> Result<(), JsValue> {
    call_method(event, "stopPropagation", &[]).map(|_| ())
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef")?.call1(react, initial)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&function(react, "useState")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into::<Function>()?))
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    function(react, "useEffect")?
        .call2(react, effect, dependencies)
        .map(|_| ())
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
        let arguments = Array::new();
        arguments.push(kind);
        arguments.push(props.map_or(&JsValue::NULL, AsRef::as_ref));
        for child in children {
            arguments.push(child);
        }
        function(&self.react, "createElement")?.apply(&self.react, &arguments)
    }
}

fn style(entries: &[(&str, String)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, &JsValue::from_str(entry))?;
    }
    Ok(value)
}

fn style_owned(entries: &[(impl AsRef<str>, String)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key.as_ref(), &JsValue::from_str(entry))?;
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

fn set_optional_index(value: &Object, key: &str, index: Option<usize>) -> Result<(), JsValue> {
    set(
        value,
        key,
        &index.map_or(JsValue::NULL, |index| {
            JsValue::from_f64(usize_as_f64(index))
        }),
    )
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    if entry.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(entry)
    }
}

fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let entry = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!entry.is_undefined()).then_some(entry))
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new(&format!("{owner} {key:?} must be a string")).into())
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required(value, key, "object")?.dyn_into::<Function>()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn number_member(value: &JsValue, key: &str) -> Result<f64, JsValue> {
    required(value, key, "event")?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("event {key:?} must be a number")).into())
}

fn i32_member(value: &JsValue, key: &str) -> Result<i32, JsValue> {
    let value = number_member(value, key)?;
    #[allow(clippy::cast_possible_truncation)]
    Ok(value as i32)
}

fn i16_member(value: &JsValue, key: &str) -> Result<i16, JsValue> {
    let value = number_member(value, key)?;
    #[allow(clippy::cast_possible_truncation)]
    Ok(value as i16)
}

fn optional_number(value: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_f64())
}

fn optional_usize(value: &JsValue, key: &str) -> Option<usize> {
    optional_number(value, key).and_then(f64_to_usize)
}

fn bool_member(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    required(value, key, "object")?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new(&format!("object {key:?} must be boolean")).into())
}

fn parse_mode(value: &str) -> Result<TrajectoryTimelineMode, JsValue> {
    match value {
        "sequence" => Ok(TrajectoryTimelineMode::Sequence),
        "duration" => Ok(TrajectoryTimelineMode::Duration),
        "time" => Ok(TrajectoryTimelineMode::Time),
        "actual" => Ok(TrajectoryTimelineMode::Actual),
        _ => Err(js_sys::Error::new(&format!("unknown Timeline mode {value:?}")).into()),
    }
}

fn f64_to_usize(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as usize)
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
