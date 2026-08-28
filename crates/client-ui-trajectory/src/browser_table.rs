//! Controller-backed Rust/WASM React trajectory-ledger renderer.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    SelectedTrajectoryRequest, TrajectoryCell, TrajectoryCellKind, TrajectoryDetailTab,
    TrajectoryRecordState, TrajectoryRequestNumber, TrajectoryTableController,
    TrajectoryTableControllerSnapshot, TrajectoryTableRecord, TrajectoryTableScrollAction,
    TrajectoryTableScrollMetrics, TrajectoryTurnModel, VirtualizableTrajectoryRecord,
    collapse_trajectory_assistant_records, collapse_trajectory_turn_records,
    filter_trajectory_table_records, flatten_trajectory_table_records,
    group_trajectory_virtual_rows, index_trajectory_request_boundaries,
    index_trajectory_request_boundary_runs, index_trajectory_request_numbers,
    trajectory_assistant_tool_calls, trajectory_browser_modules, trajectory_detail_tabs,
    trajectory_is_tool_call_only, trajectory_record_display_text, trajectory_record_id,
    trajectory_record_result_text, trajectory_record_state, trajectory_request_key,
    trajectory_section_label, trajectory_status_label, trajectory_tool_call_text_parts,
};

const VIRTUALIZATION_THRESHOLD: usize = 100;
const VIRTUAL_OVERSCAN_ROWS: usize = 12;
const VIRTUAL_INITIAL_VIEWPORT_HEIGHT_PX: f64 = 600.0;
const HISTORY_LOAD_ROW_HEIGHT_PX: f64 = 30.0;

/// Returns the compiled controller-backed `TrajectoryTable` component.
///
/// # Errors
///
/// Returns before React and shared UI primitives are configured.
#[wasm_bindgen(js_name = trajectoryTableComponent)]
pub fn trajectory_table_component() -> Result<JsValue, JsValue> {
    let (react, primitives) = trajectory_browser_modules()?;
    let primitives = primitives
        .ok_or_else(|| js_sys::Error::new("client-ui-trajectory Table requires UI primitives"))?;
    let ui = ReactUi { react, primitives };
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_table(&ui, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

struct TableRuntime {
    controller: TrajectoryTableController,
    records: Vec<TrajectoryTableRecord>,
    applied_record_selection: JsValue,
    applied_record_focus: JsValue,
}

#[allow(clippy::too_many_lines)] // Controller methods form one auditable browser boundary.
fn controller_face() -> Result<JsValue, JsValue> {
    let runtime = Rc::new(RefCell::new(TableRuntime {
        controller: TrajectoryTableController::new(),
        records: Vec::new(),
        applied_record_selection: JsValue::NULL,
        applied_record_focus: JsValue::NULL,
    }));
    let face = Object::new();

    let state_runtime = runtime.clone();
    let snapshot = Closure::wrap(Box::new(move || {
        serde_wasm_bindgen::to_value(&state_runtime.borrow().controller.snapshot())
            .map_err(js_error_from_display)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&face, "snapshot", &snapshot.into_js_value())?;

    let turns_runtime = runtime.clone();
    let set_turns = Closure::wrap(Box::new(move |turns: JsValue| -> Result<(), JsValue> {
        let turns: Vec<TrajectoryTurnModel> =
            serde_wasm_bindgen::from_value(turns).map_err(js_error_from_display)?;
        turns_runtime.borrow_mut().records = flatten_trajectory_table_records(&turns);
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "setTurns", &set_turns.into_js_value())?;

    let select_runtime = runtime.clone();
    let select_record = Closure::wrap(Box::new(move |index: f64| {
        let Some(index) = f64_to_usize(index) else {
            return;
        };
        let mut runtime = select_runtime.borrow_mut();
        let records = runtime.records.clone();
        runtime.controller.select_record(&records, index);
    }) as Box<dyn FnMut(f64)>);
    set(&face, "selectRecord", &select_record.into_js_value())?;

    let clear_runtime = runtime.clone();
    let clear =
        Closure::wrap(
            Box::new(move || clear_runtime.borrow_mut().controller.clear_selection())
                as Box<dyn FnMut()>,
        );
    set(&face, "clearSelection", &clear.into_js_value())?;

    let tab_runtime = runtime.clone();
    let activate_tab = Closure::wrap(Box::new(move |tab: String| -> Result<(), JsValue> {
        tab_runtime
            .borrow_mut()
            .controller
            .activate_tab(parse_tab(&tab)?);
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    set(&face, "activateTab", &activate_tab.into_js_value())?;

    let request_runtime = runtime.clone();
    let select_request = Closure::wrap(Box::new(
        move |request: JsValue, tab: String| -> Result<(), JsValue> {
            let request: SelectedTrajectoryRequest =
                serde_wasm_bindgen::from_value(request).map_err(js_error_from_display)?;
            request_runtime
                .borrow_mut()
                .controller
                .select_request(request, parse_tab(&tab)?);
            Ok(())
        },
    )
        as Box<dyn FnMut(JsValue, String) -> Result<(), JsValue>>);
    set(&face, "selectRequest", &select_request.into_js_value())?;

    let selection_runtime = runtime.clone();
    let apply_selection = Closure::wrap(Box::new(move |selection: JsValue| -> bool {
        if selection.is_null() || selection.is_undefined() {
            return false;
        }
        let mut runtime = selection_runtime.borrow_mut();
        if Object::is(&runtime.applied_record_selection, &selection) {
            return false;
        }
        runtime.applied_record_selection = selection.clone();
        let Some(index) = optional_usize(&selection, "index") else {
            return false;
        };
        let records = runtime.records.clone();
        runtime.controller.select_record(&records, index);
        runtime.controller.focus_record(&records, index);
        true
    }) as Box<dyn FnMut(JsValue) -> bool>);
    set(
        &face,
        "applyRecordSelection",
        &apply_selection.into_js_value(),
    )?;

    let focus_runtime = runtime.clone();
    let apply_focus = Closure::wrap(Box::new(move |focus: JsValue| -> bool {
        if focus.is_null() || focus.is_undefined() {
            return false;
        }
        let mut runtime = focus_runtime.borrow_mut();
        if Object::is(&runtime.applied_record_focus, &focus) {
            return false;
        }
        runtime.applied_record_focus = focus.clone();
        let Some(index) = optional_usize(&focus, "index") else {
            return false;
        };
        let records = runtime.records.clone();
        runtime.controller.focus_record(&records, index);
        true
    }) as Box<dyn FnMut(JsValue) -> bool>);
    set(&face, "applyRecordFocus", &apply_focus.into_js_value())?;

    let inspect_runtime = runtime.clone();
    let inspect = Closure::wrap(Box::new(move |call_id: String| -> bool {
        let mut runtime = inspect_runtime.borrow_mut();
        let records = runtime.records.clone();
        runtime.controller.inspect_call(&records, &call_id)
    }) as Box<dyn FnMut(String) -> bool>);
    set(&face, "inspectCall", &inspect.into_js_value())?;

    let pending_runtime = runtime.clone();
    let pending_scroll = Closure::wrap(Box::new(move || -> JsValue {
        let mut runtime = pending_runtime.borrow_mut();
        let records = runtime.records.clone();
        runtime
            .controller
            .take_pending_scroll_index(&records)
            .map_or(JsValue::NULL, |index| {
                JsValue::from_f64(usize_as_f64(index))
            })
    }) as Box<dyn FnMut() -> JsValue>);
    set(&face, "takePendingScroll", &pending_scroll.into_js_value())?;

    let scroll_runtime = runtime.clone();
    let on_scroll = Closure::wrap(Box::new(move |metrics: JsValue| -> Result<bool, JsValue> {
        Ok(scroll_runtime
            .borrow_mut()
            .controller
            .on_scroll(scroll_metrics(&metrics)?))
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    set(&face, "onScroll", &on_scroll.into_js_value())?;

    let load_runtime = runtime.clone();
    let begin_load = Closure::wrap(Box::new(move |request: JsValue| -> Result<bool, JsValue> {
        let history_start_seq = optional_usize(&request, "historyStartSeq")
            .map(|value| u64::try_from(value).map_err(js_error_from_display))
            .transpose()?;
        Ok(load_runtime.borrow_mut().controller.begin_older_load(
            bool_member(&request, "hasOlderRecords")?,
            bool_member(&request, "canLoadOlder")?,
            bool_member(&request, "externallyLoading")?,
            bool_member(&request, "requireTop")?,
            history_start_seq,
            scroll_metrics(&required(&request, "metrics", "older load request")?)?,
        ))
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    set(&face, "beginOlderLoad", &begin_load.into_js_value())?;

    let result_runtime = runtime.clone();
    let load_result = Closure::wrap(Box::new(move |advanced: bool| {
        result_runtime
            .borrow_mut()
            .controller
            .record_older_load_result(advanced);
    }) as Box<dyn FnMut(bool)>);
    set(&face, "recordOlderLoadResult", &load_result.into_js_value())?;

    let finish_runtime = runtime.clone();
    let finish_load = Closure::wrap(Box::new(move || {
        finish_runtime.borrow_mut().controller.finish_older_load();
    }) as Box<dyn FnMut()>);
    set(&face, "finishOlderLoad", &finish_load.into_js_value())?;

    let reconcile_runtime = runtime.clone();
    let reconcile = Closure::wrap(
        Box::new(move |request: JsValue| -> Result<JsValue, JsValue> {
            let history_start_seq = optional_usize(&request, "historyStartSeq")
                .map(|value| u64::try_from(value).map_err(js_error_from_display))
                .transpose()?;
            let mut runtime = reconcile_runtime.borrow_mut();
            let was_ready = runtime.controller.snapshot().table_scroll_ready;
            let action = runtime.controller.reconcile_scroll(
                bool_member(&request, "historyLoading")?,
                history_start_seq,
                bool_member(&request, "virtualizationEnabled")?,
                scroll_metrics(&required(&request, "metrics", "scroll reconciliation")?)?,
            );
            let ready_changed = was_ready != runtime.controller.snapshot().table_scroll_ready;
            let output = Object::new();
            set(&output, "readyChanged", &JsValue::from_bool(ready_changed))?;
            match action {
                TrajectoryTableScrollAction::None => {
                    set(&output, "kind", &JsValue::from_str("none"))?;
                }
                TrajectoryTableScrollAction::SetScrollTop(scroll_top) => {
                    set(&output, "kind", &JsValue::from_str("set"))?;
                    set(&output, "scrollTop", &JsValue::from_f64(scroll_top))?;
                }
                TrajectoryTableScrollAction::ScrollToEnd => {
                    set(&output, "kind", &JsValue::from_str("end"))?;
                }
            }
            Ok(output.into())
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    set(&face, "reconcileScroll", &reconcile.into_js_value())?;

    let thinking_runtime = runtime.clone();
    let toggle_thinking = Closure::wrap(Box::new(move || {
        thinking_runtime.borrow_mut().controller.toggle_thinking();
    }) as Box<dyn FnMut()>);
    set(&face, "toggleThinking", &toggle_thinking.into_js_value())?;

    let resize_runtime = runtime.clone();
    let reset_resize = Closure::wrap(Box::new(move || {
        resize_runtime
            .borrow_mut()
            .controller
            .reset_details_resize();
    }) as Box<dyn FnMut()>);
    set(&face, "resetDetailsResize", &reset_resize.into_js_value())?;

    let begin_resize_runtime = runtime.clone();
    let begin_resize = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        begin_resize_runtime
            .borrow_mut()
            .controller
            .begin_details_resize(
                i32_member(&event, "pointerId")?,
                number_member(&event, "clientX")?,
                number_member(&event, "detailsWidth")?,
                number_member(&event, "splitWidth")?,
            );
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "beginDetailsResize", &begin_resize.into_js_value())?;

    let move_resize_runtime = runtime.clone();
    let move_resize = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        move_resize_runtime
            .borrow_mut()
            .controller
            .move_details_resize(
                i32_member(&event, "pointerId")?,
                number_member(&event, "clientX")?,
            );
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    set(&face, "moveDetailsResize", &move_resize.into_js_value())?;

    let end_resize_runtime = runtime.clone();
    let end_resize = Closure::wrap(Box::new(move |pointer_id: f64| {
        if let Some(pointer_id) = f64_to_i32(pointer_id) {
            end_resize_runtime
                .borrow_mut()
                .controller
                .end_details_resize(pointer_id);
        }
    }) as Box<dyn FnMut(f64)>);
    set(&face, "endDetailsResize", &end_resize.into_js_value())?;

    let cancel_resize_runtime = runtime.clone();
    let cancel_resize = Closure::wrap(Box::new(move || {
        cancel_resize_runtime
            .borrow_mut()
            .controller
            .cancel_details_resize();
    }) as Box<dyn FnMut()>);
    set(&face, "cancelDetailsResize", &cancel_resize.into_js_value())?;

    let keyboard_resize_runtime = runtime;
    let keyboard_resize = Closure::wrap(Box::new(
        move |direction: f64, current_width: f64, split_width: f64| {
            let Some(direction) = f64_to_i8(direction) else {
                return;
            };
            keyboard_resize_runtime
                .borrow_mut()
                .controller
                .keyboard_details_resize(direction, current_width, split_width);
        },
    ) as Box<dyn FnMut(f64, f64, f64)>);
    set(
        &face,
        "keyboardDetailsResize",
        &keyboard_resize.into_js_value(),
    )?;

    Ok(face.into())
}

#[derive(Clone, Copy)]
struct ViewportState {
    scroll_top: f64,
    height: f64,
}

struct RenderedRecord {
    record: TrajectoryTableRecord,
    position: usize,
    terminal_request_boundary: bool,
}

struct RenderedWindow {
    records: Vec<RenderedRecord>,
    top: f64,
    bottom: f64,
}

#[allow(clippy::too_many_lines)]
fn render_table(ui: &ReactUi, props: &JsValue) -> Result<JsValue, JsValue> {
    let turns_value = required(props, "turns", "TrajectoryTable")?;
    let turns: Vec<TrajectoryTurnModel> =
        serde_wasm_bindgen::from_value(turns_value.clone()).map_err(js_error_from_display)?;
    let all_records = flatten_trajectory_table_records(&turns);

    let controller_ref = use_ref(&ui.react, &JsValue::UNDEFINED)?;
    let mut controller = Reflect::get(&controller_ref, &JsValue::from_str("current"))?;
    if controller.is_undefined() {
        controller = controller_face()?;
        Reflect::set(&controller_ref, &JsValue::from_str("current"), &controller)?;
    }
    call_method(&controller, "setTurns", std::slice::from_ref(&turns_value))?;

    let (revision, set_revision) = use_state(&ui.react, &JsValue::from_f64(0.0))?;
    let revision = revision.as_f64().unwrap_or(0.0);
    let bump = bump_callback(&set_revision, revision);
    let pane_ref = use_ref(&ui.react, &JsValue::NULL)?;
    let (viewport, set_viewport) = use_state(
        &ui.react,
        &object(&[
            ("scrollTop", JsValue::from_f64(0.0)),
            (
                "height",
                JsValue::from_f64(VIRTUAL_INITIAL_VIEWPORT_HEIGHT_PX),
            ),
        ])?
        .into(),
    )?;
    let viewport = ViewportState {
        scroll_top: optional_number(&viewport, "scrollTop").unwrap_or(0.0),
        height: optional_number(&viewport, "height").unwrap_or(VIRTUAL_INITIAL_VIEWPORT_HEIGHT_PX),
    };

    install_external_selection_effect(ui, props, &controller, &bump, &turns_value)?;
    install_external_focus_effect(ui, props, &controller, &bump, &turns_value)?;
    install_inspect_effect(ui, props, &controller, &bump, &turns_value, &all_records)?;

    let streaming = optional_vec::<TrajectoryCell>(props, "streamingCells")?;
    let streaming_by_index = streaming
        .into_iter()
        .map(|cell| (cell.index, cell))
        .collect::<BTreeMap<_, _>>();
    let collapsed_turns = u64_set(optional(props, "collapsedTurns")?.as_ref())?;
    let collapsed_assistants = string_set(optional(props, "collapsedAssistants")?.as_ref())?;
    let search_matches = optional_number_set(props, "searchMatchIndexes")?;
    let timeline_focus = optional_number_set(props, "timelineFocusIndexes")?;
    let mut records = if let Some(matches) = &search_matches {
        filter_trajectory_table_records(&all_records, matches)
    } else {
        let turns = if collapsed_turns.is_empty() {
            all_records.clone()
        } else {
            collapse_trajectory_turn_records(&all_records, &collapsed_turns)
        };
        if collapsed_assistants.is_empty() {
            turns
        } else {
            collapse_trajectory_assistant_records(&turns, &collapsed_assistants)
        }
    };
    for record in &mut records {
        if let Some(cell) = streaming_by_index.get(&record.cell.index) {
            record.cell.clone_from(cell);
        }
    }

    let has_older = optional_bool(props, "hasOlderRecords")?.unwrap_or(false);
    let virtualization_enabled = has_older || records.len() > VIRTUALIZATION_THRESHOLD;
    let window = rendered_window(&records, virtualization_enabled, has_older, viewport);
    install_scroll_reconciliation_effect(
        ui,
        props,
        &controller,
        &pane_ref,
        &bump,
        virtualization_enabled,
        &turns_value,
    )?;

    let state = controller_snapshot(&controller)?;
    let selected_template = state.selected_record_id.as_deref().and_then(|id| {
        all_records
            .iter()
            .find(|record| trajectory_record_id(&record.cell) == id)
    });
    let selected = selected_template.map(|record| current_record(record, &streaming_by_index));
    let selected_index = selected.as_ref().map(|record| record.cell.index);
    install_selected_index_effect(ui, props, selected_index)?;
    install_pending_scroll_effect(
        ui,
        &controller,
        &pane_ref,
        &set_viewport,
        state.pending_scroll_record_id.as_deref(),
        &records,
        virtualization_enabled,
        &turns_value,
    )?;

    let request_boundaries = index_trajectory_request_boundaries(&all_records);
    let session_requests = optional_vec::<TrajectoryRequestNumber>(props, "requestNumbers")?;
    let request_numbers =
        index_trajectory_request_numbers(&all_records, &session_requests, &request_boundaries);
    let request_runs = index_trajectory_request_boundary_runs(&records);

    let on_scroll = table_scroll_handler(&controller, &bump, &set_viewport, props, true)?;
    let clear_controller = controller.clone();
    let clear_bump = bump.clone();
    let on_clear_selection = optional_function(props, "onClearSelection")?;
    let pane_click = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let target = required(&event, "target", "table pane click")?;
        let current = required(&event, "currentTarget", "table pane click")?;
        if !Object::is(&target, &current) {
            return Ok(());
        }
        call_method(&clear_controller, "clearSelection", &[])?;
        if let Some(callback) = &on_clear_selection {
            callback.call0(&JsValue::UNDEFINED)?;
        }
        clear_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let mut table_children = Vec::new();
    if has_older {
        table_children.push(history_row(
            ui,
            props,
            &controller,
            &bump,
            &pane_ref,
            state.loading_older,
        )?);
    }
    if window.top > 0.0 {
        table_children.push(virtual_spacer(ui, "top", window.top)?);
    }
    for rendered in window.records {
        table_children.push(render_table_row(
            ui,
            props,
            &controller,
            &bump,
            rendered,
            &all_records,
            &request_boundaries,
            &request_numbers,
            &request_runs,
            &session_requests,
            &collapsed_turns,
            &collapsed_assistants,
            timeline_focus.as_ref(),
            selected_index,
            state.selected_request.as_ref(),
            virtualization_enabled,
            usize::from(has_older),
        )?);
    }
    if window.bottom > 0.0 {
        table_children.push(virtual_spacer(ui, "bottom", window.bottom)?);
    }
    let tbody = ui.tag("tbody", None, &table_children)?;
    let colgroup = ui.tag(
        "colgroup",
        None,
        &[
            ui.tag(
                "col",
                Some(&class("seekdeep-trajectory-table-eventColumn")?),
                &[],
            )?,
            ui.tag(
                "col",
                Some(&class("seekdeep-trajectory-table-contentColumn")?),
                &[],
            )?,
        ],
    )?;
    let table = ui.tag(
        "table",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-table"),
            ),
            ("role", JsValue::from_str("table")),
            (
                "data-scroll-ready",
                if state.table_scroll_ready {
                    JsValue::from_str("true")
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "aria-rowcount",
                JsValue::from_f64(usize_as_f64(records.len() + usize::from(has_older))),
            ),
        ])?),
        &[colgroup, tbody],
    )?;

    let history_loading = optional_bool(props, "historyLoading")?.unwrap_or(false);
    let mut pane_children = Vec::new();
    if history_loading || !state.table_scroll_ready {
        let spinner = ui.tag(
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-trajectory-table-historyLoadingSpinner"),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?;
        let bar = ui.tag(
            "span",
            Some(&class("seekdeep-trajectory-table-historyLoadingBar")?),
            &[spinner, JsValue::from_str("Loading trajectory…")],
        )?;
        pane_children.push(ui.tag(
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-trajectory-table-historyLoading"),
                ),
                ("role", JsValue::from_str("status")),
                ("aria-live", JsValue::from_str("polite")),
            ])?),
            &[bar],
        )?);
    }
    pane_children.push(table);
    let pane = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-tablePane"),
            ),
            ("data-trajectory-scroll", JsValue::from_str("")),
            ("ref", pane_ref),
            ("onScroll", on_scroll.into()),
            ("onClick", pane_click.into_js_value()),
        ])?),
        &pane_children,
    )?;

    let mut split_children = vec![pane];
    if state.selected_request.is_some() || selected.is_some() {
        split_children.push(render_inspector(
            ui,
            props,
            &controller,
            &bump,
            &state,
            selected.as_ref(),
            &all_records,
            &session_requests,
            &request_numbers,
            &streaming_by_index,
        )?);
    }
    let split_style = state.tool_request_offset.map(|offset| {
        style(&[(
            "--trajectory-tool-request-width",
            format!("calc(58cqw - {offset}px)"),
        )])
    });
    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-split"),
            ),
            (
                "style",
                split_style
                    .transpose()?
                    .map_or(JsValue::UNDEFINED, Into::into),
            ),
        ])?),
        &split_children,
    )
}

fn rendered_window(
    records: &[TrajectoryTableRecord],
    virtualized: bool,
    has_older: bool,
    viewport: ViewportState,
) -> RenderedWindow {
    if !virtualized {
        return RenderedWindow {
            records: records
                .iter()
                .cloned()
                .enumerate()
                .map(|(position, record)| RenderedRecord {
                    terminal_request_boundary: record.cell.request_only == Some(true)
                        && position + 1 == records.len(),
                    record,
                    position,
                })
                .collect(),
            top: 0.0,
            bottom: 0.0,
        };
    }
    let virtualizable = records
        .iter()
        .map(|record| VirtualizableTrajectoryRecord {
            collapsed_summary_kind: record.collapsed_summary_kind,
            cell: record.cell.clone(),
        })
        .collect::<Vec<_>>();
    let rows = group_trajectory_virtual_rows(&virtualizable);
    let heights = rows
        .iter()
        .map(|row| f64::from(row.height))
        .collect::<Vec<_>>();
    let total = heights.iter().sum::<f64>();
    let margin = if has_older {
        HISTORY_LOAD_ROW_HEIGHT_PX
    } else {
        0.0
    };
    let top = (viewport.scroll_top - margin).max(0.0);
    let bottom = top + viewport.height.max(1.0);
    let mut offset = 0.0;
    let mut first = 0;
    while first < heights.len() && offset + heights[first] < top {
        offset += heights[first];
        first += 1;
    }
    let mut end = first;
    let mut end_offset = offset;
    while end < heights.len() && end_offset <= bottom {
        end_offset += heights[end];
        end += 1;
    }
    let start = first.saturating_sub(VIRTUAL_OVERSCAN_ROWS);
    end = (end + VIRTUAL_OVERSCAN_ROWS).min(rows.len());
    let top_spacer = heights[..start].iter().sum::<f64>();
    let bottom_spacer = total - heights[..end].iter().sum::<f64>();
    let mut rendered = Vec::new();
    for row in &rows[start..end] {
        for (entry_index, entry) in row.entries.iter().enumerate() {
            let source = &records[entry.logical_index];
            rendered.push(RenderedRecord {
                record: source.clone(),
                position: entry.logical_index,
                terminal_request_boundary: source.cell.request_only == Some(true)
                    && row
                        .entries
                        .last()
                        .is_some_and(|last| last.record.cell.request_only == Some(true))
                    && entry_index + 1 == row.entries.len(),
            });
        }
    }
    RenderedWindow {
        records: rendered,
        top: top_spacer,
        bottom: bottom_spacer.max(0.0),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_table_row(
    ui: &ReactUi,
    props: &JsValue,
    controller: &JsValue,
    bump: &Function,
    rendered: RenderedRecord,
    all_records: &[TrajectoryTableRecord],
    request_boundaries: &BTreeMap<String, usize>,
    request_numbers: &BTreeMap<String, u64>,
    request_runs: &BTreeMap<usize, usize>,
    session_requests: &[TrajectoryRequestNumber],
    collapsed_turns: &BTreeSet<u64>,
    _collapsed_assistants: &BTreeSet<String>,
    timeline_focus: Option<&BTreeSet<usize>>,
    selected_index: Option<usize>,
    selected_request: Option<&SelectedTrajectoryRequest>,
    virtualized: bool,
    history_offset: usize,
) -> Result<JsValue, JsValue> {
    let record = rendered.record;
    let collapsed = record.collapsed_summary.is_some();
    let request_only = record.cell.request_only == Some(true);
    let display = trajectory_record_display_text(&record.cell).map_err(js_error_from_display)?;
    let result = trajectory_record_result_text(&record.cell).map_err(js_error_from_display)?;
    let tool_only = trajectory_is_tool_call_only(&record.cell);
    let tool_parts = trajectory_tool_call_text_parts(record.cell.kind, &display);
    let list_display = if tool_only {
        "(tool call only)".to_owned()
    } else if let Some(parts) = &tool_parts {
        [Some(parts.name.as_str()), parts.arguments.as_deref()]
            .into_iter()
            .flatten()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        display.clone()
    };
    let key = trajectory_request_key(record.turn, &record.group);
    let request = (request_boundaries.get(&key) == Some(&record.cell.index)
        && !collapsed
        && record
            .turn
            .is_none_or(|turn| !collapsed_turns.contains(&turn)))
    .then(|| request_numbers.get(&key).copied())
    .flatten();
    let request_info = request.and_then(|number| {
        session_requests
            .iter()
            .find(|candidate| candidate.number == number)
    });
    let request_selected = request.is_some()
        && selected_request
            .is_some_and(|selected| selected.turn == record.turn && selected.group == record.group);

    let mut event_children = Vec::new();
    if let Some(request) = request {
        let run = request_runs.get(&record.cell.index).copied().unwrap_or(0);
        let label = format!(
            "Request #{request}{}",
            request_info
                .filter(|request| {
                    request.purpose == crate::TrajectoryRequestPurpose::Compaction
                })
                .map_or("", |_| " · Compaction")
        );
        let request_controller = controller.clone();
        let request_bump = bump.clone();
        let request_group = record.group.clone();
        let request_turn = record.turn;
        let request_seq = request_info.and_then(|request| request.seq);
        let on_click = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            stop_propagation(&event)?;
            let request = SelectedTrajectoryRequest {
                turn: request_turn,
                group: request_group.clone(),
                seq: request_seq,
            };
            call_method(
                &request_controller,
                "selectRequest",
                &[
                    serde_wasm_bindgen::to_value(&request).map_err(js_error_from_display)?,
                    JsValue::from_str("overview"),
                ],
            )?;
            request_bump.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        event_children.push(ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(if request_selected {
                        "seekdeep-trajectory-table-requestBoundaryControl seekdeep-trajectory-table-requestBoundaryControlActive"
                    } else {
                        "seekdeep-trajectory-table-requestBoundaryControl"
                    }),
                ),
                ("aria-label", JsValue::from_str(&label)),
                ("aria-pressed", JsValue::from_bool(request_selected)),
                ("data-label", JsValue::from_str(&label)),
                (
                    "data-request-run-index",
                    JsValue::from_f64(usize_as_f64(run)),
                ),
                (
                    "data-request-status",
                    request_info
                        .and_then(|request| request.status)
                        .or_else(|| {
                            (record.cell.is_error == Some(true))
                                .then_some(TrajectoryRecordState::Error)
                        })
                        .map_or(JsValue::UNDEFINED, |state| {
                            JsValue::from_str(match state {
                                TrajectoryRecordState::Complete => "complete",
                                TrajectoryRecordState::Running => "running",
                                TrajectoryRecordState::Error => "error",
                            })
                        }),
                ),
                (
                    "style",
                    style(&[(
                        "--request-boundary-offset",
                        format!("{}px", run * 8),
                    )])?
                    .into(),
                ),
                ("onClick", on_click.into_js_value()),
            ])?),
            &[],
        )?);
    }
    let section_active = selected_request.is_some_and(|selected| selected.turn == record.turn)
        || selected_index.is_some_and(|selected| {
            all_records.iter().any(|candidate| {
                candidate.cell.index == selected
                    && candidate.turn == record.turn
                    && (record.turn.is_some() || candidate.section == record.section)
            })
        });
    if !collapsed && !request_only && record.turn_start {
        let label = trajectory_section_label(record.turn);
        let turn_children = if let Some(turn) = record.turn {
            vec![
                ui.tag(
                    "span",
                    Some(&class("seekdeep-trajectory-table-turnLabelFull")?),
                    &[JsValue::from_str(&format!("Turn {turn}"))],
                )?,
                ui.tag(
                    "span",
                    Some(&class("seekdeep-trajectory-table-turnLabelCompact")?),
                    &[JsValue::from_str(&format!("#{turn}"))],
                )?,
            ]
        } else {
            vec![JsValue::from_str(&label)]
        };
        event_children.push(ui.tag(
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(if section_active {
                        "seekdeep-trajectory-table-turnLabel seekdeep-trajectory-table-turnLabelActive"
                    } else {
                        "seekdeep-trajectory-table-turnLabel"
                    }),
                ),
                ("aria-label", JsValue::from_str(&label)),
            ])?),
            &turn_children,
        )?);
    }
    if record.turn.is_some() && section_active && !request_only {
        event_children.push(ui.tag(
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-trajectory-table-turnRail"),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?);
    }
    if !collapsed && !request_only && selected_index == Some(record.cell.index) {
        event_children.push(ui.tag(
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-trajectory-table-selectionRail"),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?);
    }
    if !collapsed && !request_only {
        let role = role_tag(ui, record.cell.kind)?;
        let slot = ui.tag(
            "span",
            Some(&class("seekdeep-trajectory-table-kindSlot")?),
            &[role],
        )?;
        event_children.push(ui.tag(
            "div",
            Some(&class("seekdeep-trajectory-table-eventInner")?),
            &[slot],
        )?);
    }
    let event_cell = ui.tag(
        "td",
        Some(&class("seekdeep-trajectory-table-event")?),
        &event_children,
    )?;
    let content = if request_only {
        JsValue::NULL
    } else if let Some(summary) = &record.collapsed_summary {
        ui.tag(
            "span",
            Some(&class("seekdeep-trajectory-table-collapsedTurnContent")?),
            &[JsValue::from_str(&format!("…{summary}"))],
        )?
    } else {
        let mut children = vec![JsValue::from_str(&list_display)];
        if let Some(result) = &result {
            children.push(JsValue::from_str(&format!(" → {result}")));
        }
        ui.tag(
            "span",
            Some(&class("seekdeep-trajectory-table-contentText")?),
            &children,
        )?
    };
    let content_cell = ui.tag(
        "td",
        Some(&class("seekdeep-trajectory-table-content")?),
        &[content],
    )?;

    let select_controller = controller.clone();
    let select_bump = bump.clone();
    let select_callback = optional_function(props, "onRecordSelect")?;
    let index = record.cell.index;
    let summary_kind = record.collapsed_summary_kind;
    let turn = record.turn;
    let assistant_id = trajectory_record_id(&record.cell);
    let toggle_turn = required_function(props, "onToggleTurn")?;
    let toggle_assistant = required_function(props, "onToggleAssistant")?;
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if request_only {
            return Ok(());
        }
        if summary_kind.is_some() {
            if summary_kind == Some(crate::CollapsedSummaryKind::Turn) {
                if let Some(turn) = turn {
                    toggle_turn.call1(&JsValue::UNDEFINED, &JsValue::from_f64(u64_as_f64(turn)))?;
                }
            } else {
                toggle_assistant.call1(&JsValue::UNDEFINED, &JsValue::from_str(&assistant_id))?;
            }
            return Ok(());
        }
        call_method(
            &select_controller,
            "selectRecord",
            &[JsValue::from_f64(usize_as_f64(index))],
        )?;
        if let Some(callback) = &select_callback {
            callback.call1(&JsValue::UNDEFINED, &JsValue::from_f64(usize_as_f64(index)))?;
        }
        select_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let key_controller = controller.clone();
    let key_bump = bump.clone();
    let key_callback = optional_function(props, "onRecordSelect")?;
    let key_toggle_turn = required_function(props, "onToggleTurn")?;
    let key_toggle_assistant = required_function(props, "onToggleAssistant")?;
    let key_assistant_id = trajectory_record_id(&record.cell);
    let on_key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if request_only {
            return Ok(());
        }
        let key = required_string(&event, "key", "row key event")?;
        if key != "Enter" && key != " " {
            return Ok(());
        }
        prevent_default(&event)?;
        if let Some(summary_kind) = summary_kind {
            if summary_kind == crate::CollapsedSummaryKind::Turn {
                if let Some(turn) = turn {
                    key_toggle_turn
                        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(u64_as_f64(turn)))?;
                }
            } else {
                key_toggle_assistant
                    .call1(&JsValue::UNDEFINED, &JsValue::from_str(&key_assistant_id))?;
            }
            return Ok(());
        }
        call_method(
            &key_controller,
            "selectRecord",
            &[JsValue::from_f64(usize_as_f64(index))],
        )?;
        if let Some(callback) = &key_callback {
            callback.call1(&JsValue::UNDEFINED, &JsValue::from_f64(usize_as_f64(index)))?;
        }
        key_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let selected = !collapsed && !request_only && selected_index == Some(index);
    let double_toggle_turn = required_function(props, "onToggleTurn")?;
    let double_toggle_assistant = required_function(props, "onToggleAssistant")?;
    let double_assistant_id = trajectory_record_id(&record.cell);
    let double_kind = record.cell.kind;
    let double_turn_start = record.turn_start;
    let double_turn = record.turn;
    let double_turn_collapsed = double_turn.is_some_and(|turn| collapsed_turns.contains(&turn));
    let double_has_calls = !trajectory_assistant_tool_calls(all_records, index).is_empty();
    let double_turn_content = double_turn.map_or(0, |turn| {
        all_records
            .iter()
            .filter(|candidate| {
                candidate.turn == Some(turn)
                    && candidate.cell.request_only != Some(true)
                    && candidate.cell.kind != TrajectoryCellKind::System
            })
            .count()
    });
    let on_double_click = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if collapsed || request_only {
            return Ok(());
        }
        if double_turn_collapsed {
            if let Some(turn) = double_turn {
                prevent_default(&event)?;
                double_toggle_turn
                    .call1(&JsValue::UNDEFINED, &JsValue::from_f64(u64_as_f64(turn)))?;
            }
            return Ok(());
        }
        if double_kind == TrajectoryCellKind::Message && double_has_calls {
            prevent_default(&event)?;
            double_toggle_assistant.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&double_assistant_id),
            )?;
            return Ok(());
        }
        if double_turn_start
            && double_turn_content > 1
            && let Some(turn) = double_turn
        {
            prevent_default(&event)?;
            double_toggle_turn.call1(&JsValue::UNDEFINED, &JsValue::from_f64(u64_as_f64(turn)))?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let aria_label = if let Some(summary) = &record.collapsed_summary {
        format!(
            "Collapsed {} summary, {summary}",
            match record.collapsed_summary_kind {
                Some(crate::CollapsedSummaryKind::Turn) => "turn",
                Some(crate::CollapsedSummaryKind::Assistant) => "assistant",
                None => "",
            }
        )
    } else if request_only {
        format!("Request {}, compaction", request.unwrap_or(0))
    } else {
        format!(
            "{}{}, {}, {}",
            request.map_or_else(String::new, |request| format!("Request {request}, ")),
            kind_label(record.cell.kind),
            list_display,
            result.as_deref().unwrap_or("")
        )
    };
    ui.tag(
        "tr",
        Some(&object(&[
            ("role", JsValue::from_str("row")),
            (
                "tabIndex",
                JsValue::from_f64(if request_only { -1.0 } else { 0.0 }),
            ),
            (
                "aria-rowindex",
                JsValue::from_f64(usize_as_f64(rendered.position + 1 + history_offset)),
            ),
            (
                "aria-label",
                JsValue::from_str(aria_label.trim_end_matches(", ")),
            ),
            ("aria-selected", JsValue::from_bool(selected)),
            ("data-kind", JsValue::from_str(record.cell.kind.as_str())),
            (
                "data-trajectory-row-key",
                JsValue::from_str(&trajectory_record_id(&record.cell)),
            ),
            (
                "data-virtual-position",
                if virtualized {
                    JsValue::from_f64(usize_as_f64(rendered.position))
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "data-record-index",
                if collapsed || request_only {
                    JsValue::UNDEFINED
                } else {
                    JsValue::from_f64(usize_as_f64(index))
                },
            ),
            ("data-request-only", bool_data(request_only)),
            (
                "data-terminal-request-boundary",
                bool_data(rendered.terminal_request_boundary),
            ),
            ("data-group-start", bool_data(record.group_start)),
            ("data-turn-start", bool_data(record.turn_start)),
            ("data-error", bool_data(record.cell.is_error == Some(true))),
            (
                "data-running",
                bool_data(trajectory_record_state(&record) == TrajectoryRecordState::Running),
            ),
            ("data-turn-end", bool_data(record.turn_end)),
            (
                "data-collapsed-summary",
                record
                    .collapsed_summary_kind
                    .map_or(JsValue::UNDEFINED, |kind| {
                        JsValue::from_str(match kind {
                            crate::CollapsedSummaryKind::Turn => "turn",
                            crate::CollapsedSummaryKind::Assistant => "assistant",
                        })
                    }),
            ),
            ("data-selected", bool_data(selected)),
            (
                "data-timeline-focus",
                if collapsed {
                    JsValue::UNDEFINED
                } else {
                    timeline_focus.map_or(JsValue::UNDEFINED, |focus| {
                        JsValue::from_str(if focus.contains(&index) {
                            "inside"
                        } else {
                            "outside"
                        })
                    })
                },
            ),
            ("onClick", on_click.into_js_value()),
            ("onDoubleClick", on_double_click.into_js_value()),
            ("onKeyDown", on_key_down.into_js_value()),
        ])?),
        &[event_cell, content_cell],
    )
}

fn role_tag(ui: &ReactUi, kind: TrajectoryCellKind) -> Result<JsValue, JsValue> {
    let icon_name = match kind {
        TrajectoryCellKind::Context => "information",
        TrajectoryCellKind::Compacted => "compacted",
        TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool => "wrench",
        TrajectoryCellKind::System => "settings",
        TrajectoryCellKind::User => "user",
        TrajectoryCellKind::Message => "sparkle",
    };
    let icon = ui.tag(
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-kindTagIcon"),
            ),
            ("data-role-icon", JsValue::from_str(icon_name)),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[JsValue::from_str(match kind {
            TrajectoryCellKind::Context => "i",
            TrajectoryCellKind::Compacted => "↯",
            TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool => "⌕",
            TrajectoryCellKind::System => "⚙",
            TrajectoryCellKind::User => "●",
            TrajectoryCellKind::Message => "✦",
        })],
    )?;
    let tooltip = ui.primitive(
        "Tooltip",
        Some(&object(&[
            ("label", JsValue::from_str(kind_label(kind))),
            ("side", JsValue::from_str("right")),
        ])?),
        &[icon],
    )?;
    let color_class = match kind {
        TrajectoryCellKind::System => "seekdeep-trajectory-table-systemNeutral",
        TrajectoryCellKind::User => "seekdeep-trajectory-table-user",
        TrajectoryCellKind::Context => "seekdeep-trajectory-table-contextGreen",
        TrajectoryCellKind::Compacted => "seekdeep-trajectory-table-compacted",
        TrajectoryCellKind::Message => "seekdeep-trajectory-table-assistantVioletBright",
        TrajectoryCellKind::Tool => "seekdeep-trajectory-table-toolAmber",
        TrajectoryCellKind::Subtool => "seekdeep-trajectory-table-subtoolAmber",
    };
    ui.tag(
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&format!("seekdeep-trajectory-table-kindTag {color_class}")),
            ),
            ("data-role-kind", JsValue::from_str(kind.as_str())),
        ])?),
        &[
            tooltip,
            ui.tag(
                "span",
                Some(&class("seekdeep-trajectory-table-kindTagLabel")?),
                &[JsValue::from_str(kind_label(kind))],
            )?,
        ],
    )
}

fn history_row(
    ui: &ReactUi,
    props: &JsValue,
    controller: &JsValue,
    bump: &Function,
    pane_ref: &JsValue,
    locally_loading: bool,
) -> Result<JsValue, JsValue> {
    let external = optional_bool(props, "olderHistoryLoading")?.unwrap_or(false);
    let busy = external || locally_loading;
    let on_load = optional_function(props, "onLoadOlder")?;
    let load_controller = controller.clone();
    let load_bump = bump.clone();
    let load_props = props.clone();
    let load_ref = pane_ref.clone();
    let on_click = Closure::wrap(Box::new(move || -> Result<Promise, JsValue> {
        let pane = Reflect::get(&load_ref, &JsValue::from_str("current"))?;
        request_older(&load_controller, &load_bump, &load_props, &pane, false)
    }) as Box<dyn FnMut() -> Result<Promise, JsValue>>);
    let status = ui.tag(
        "span",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-visuallyHidden"),
            ),
            ("role", JsValue::from_str("status")),
            ("aria-live", JsValue::from_str("polite")),
        ])?),
        &[JsValue::from_str(if busy {
            "Loading earlier history…"
        } else {
            ""
        })],
    )?;
    let spinner = busy
        .then(|| {
            ui.tag(
                "span",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-trajectory-table-historyLoadingSpinner"),
                    ),
                    ("aria-hidden", JsValue::TRUE),
                ])?),
                &[],
            )
        })
        .transpose()?;
    let mut button_children = Vec::new();
    if let Some(spinner) = spinner {
        button_children.push(spinner);
    }
    button_children.push(JsValue::from_str(if busy {
        "Loading earlier history…"
    } else {
        "Load earlier history"
    }));
    button_children.push(status);
    let button = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-historyLoadButton"),
            ),
            ("disabled", JsValue::from_bool(busy || on_load.is_none())),
            (
                "aria-label",
                JsValue::from_str(if busy {
                    "Loading earlier history…"
                } else {
                    "Load earlier history"
                }),
            ),
            ("onClick", on_click.into_js_value()),
        ])?),
        &button_children,
    )?;
    let cell = ui.tag(
        "td",
        Some(&object(&[("colSpan", JsValue::from_f64(2.0))])?),
        &[button],
    )?;
    ui.tag(
        "tr",
        Some(&object(&[
            ("role", JsValue::from_str("row")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-historyLoadRow"),
            ),
            ("data-history-load", JsValue::from_str("")),
            ("aria-rowindex", JsValue::from_f64(1.0)),
        ])?),
        &[cell],
    )
}

fn virtual_spacer(ui: &ReactUi, side: &str, height: f64) -> Result<JsValue, JsValue> {
    let cell = ui.tag(
        "td",
        Some(&object(&[
            ("colSpan", JsValue::from_f64(2.0)),
            (
                "style",
                style(&[("--trajectory-virtual-spacer-height", format!("{height}px"))])?.into(),
            ),
        ])?),
        &[],
    )?;
    ui.tag(
        "tr",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-virtualSpacer"),
            ),
            ("data-virtual-spacer", JsValue::from_str(side)),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[cell],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_inspector(
    ui: &ReactUi,
    _props: &JsValue,
    controller: &JsValue,
    bump: &Function,
    state: &TrajectoryTableControllerSnapshot,
    selected: Option<&TrajectoryTableRecord>,
    all_records: &[TrajectoryTableRecord],
    session_requests: &[TrajectoryRequestNumber],
    request_numbers: &BTreeMap<String, u64>,
    streaming: &BTreeMap<usize, TrajectoryCell>,
) -> Result<JsValue, JsValue> {
    let close_controller = controller.clone();
    let close_bump = bump.clone();
    let close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&close_controller, "clearSelection", &[])?;
        close_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let header_text = if let Some(request) = &state.selected_request {
        format!(
            "{} · {}",
            trajectory_section_label(request.turn),
            request.group
        )
    } else {
        selected.map_or_else(String::new, |record| {
            format!(
                "{} · {}",
                trajectory_section_label(record.turn),
                record.group
            )
        })
    };
    let mut header_identity = Vec::new();
    if let Some(request) = &state.selected_request {
        let number = request_numbers
            .get(&trajectory_request_key(request.turn, &request.group))
            .copied();
        header_identity.push(ui.tag(
            "span",
            Some(&class("seekdeep-trajectory-table-requestDetailsName")?),
            &[JsValue::from_str(&number.map_or_else(
                || "Request #—".to_owned(),
                |number| format!("Request #{number}"),
            ))],
        )?);
    }
    header_identity.push(ui.tag(
        "span",
        Some(&class("seekdeep-trajectory-table-detailsLocation")?),
        &[JsValue::from_str(&header_text)],
    )?);
    let identity = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-table-detailsTitle")?),
        &header_identity,
    )?;
    let header = ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-table-detailsHeader")?),
        &[
            identity,
            ui.tag(
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-trajectory-table-close"),
                    ),
                    ("aria-label", JsValue::from_str("Close details")),
                    ("onClick", close.into_js_value()),
                ])?),
                &[JsValue::from_str("×")],
            )?,
        ],
    )?;
    let selected_request_info = state.selected_request.as_ref().and_then(|request| {
        session_requests.iter().find(|candidate| {
            request.seq.map_or_else(
                || candidate.turn == request.turn && candidate.group == request.group,
                |seq| candidate.seq == Some(seq),
            )
        })
    });
    let tabs = if state.selected_request.is_some() {
        let mut tabs = vec![(TrajectoryDetailTab::Overview, "Summary")];
        if selected_request_info
            .and_then(|request| request.request_config.as_ref())
            .is_some()
        {
            tabs.push((TrajectoryDetailTab::Options, "Options"));
        }
        tabs.extend([
            (TrajectoryDetailTab::Usage, "Usage"),
            (TrajectoryDetailTab::Timing, "Timing"),
        ]);
        tabs
    } else {
        selected.map_or_else(Vec::new, |record| {
            trajectory_detail_tabs(record)
                .into_iter()
                .map(|tab| (tab.id, tab.label))
                .collect()
        })
    };
    let mut tab_nodes = Vec::new();
    for (tab, label) in tabs {
        let tab_controller = controller.clone();
        let tab_bump = bump.clone();
        let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(
                &tab_controller,
                "activateTab",
                &[JsValue::from_str(tab.as_str())],
            )?;
            tab_bump.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        tab_nodes.push(ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(if state.active_tab == tab {
                        "seekdeep-trajectory-table-detailTab seekdeep-trajectory-table-detailTabActive"
                    } else {
                        "seekdeep-trajectory-table-detailTab"
                    }),
                ),
                ("role", JsValue::from_str("tab")),
                ("aria-selected", JsValue::from_bool(state.active_tab == tab)),
                ("aria-label", JsValue::from_str(label)),
                ("onClick", on_click.into_js_value()),
            ])?),
            &[JsValue::from_str(label)],
        )?);
    }
    let tabs = ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-detailTabs"),
            ),
            ("role", JsValue::from_str("tablist")),
            ("aria-label", JsValue::from_str("Event details")),
        ])?),
        &tab_nodes,
    )?;
    let body = render_inspector_body(
        ui,
        controller,
        bump,
        state,
        selected,
        all_records,
        session_requests,
        request_numbers,
        streaming,
    )?;
    let resize_handle = details_resize_handle(ui, controller, bump)?;
    ui.tag(
        "aside",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-details"),
            ),
            ("role", JsValue::from_str("complementary")),
            ("aria-label", JsValue::from_str("Event details")),
            (
                "style",
                state.details_width.map_or(JsValue::UNDEFINED, |width| {
                    style(&[("width", format!("{width}px"))])
                        .map(Into::into)
                        .unwrap_or(JsValue::UNDEFINED)
                }),
            ),
        ])?),
        &[resize_handle, header, tabs, body],
    )
}

#[allow(clippy::too_many_lines)] // Pointer, keyboard, cancel, and reset paths share measurements.
fn details_resize_handle(
    ui: &ReactUi,
    controller: &JsValue,
    bump: &Function,
) -> Result<JsValue, JsValue> {
    let reset_controller = controller.clone();
    let reset_bump = bump.clone();
    let on_double_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&reset_controller, "resetDetailsResize", &[])?;
        reset_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let down_controller = controller.clone();
    let on_pointer_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if i32_member(&event, "button")? != 0 {
            return Ok(());
        }
        let current = required(&event, "currentTarget", "resize pointer")?;
        let details = required(&current, "parentElement", "resize handle")?;
        let split = required(&details, "parentElement", "details panel")?;
        let details_rect = call_method(&details, "getBoundingClientRect", &[])?;
        let split_rect = call_method(&split, "getBoundingClientRect", &[])?;
        let input = object(&[
            (
                "pointerId",
                required(&event, "pointerId", "resize pointer")?,
            ),
            ("clientX", required(&event, "clientX", "resize pointer")?),
            (
                "detailsWidth",
                required(&details_rect, "width", "details rectangle")?,
            ),
            (
                "splitWidth",
                required(&split_rect, "width", "split rectangle")?,
            ),
        ])?;
        call_method(&down_controller, "beginDetailsResize", &[input.into()])?;
        capture_pointer(&event)?;
        prevent_default(&event)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let move_controller = controller.clone();
    let move_bump = bump.clone();
    let on_pointer_move = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let input = object(&[
            (
                "pointerId",
                required(&event, "pointerId", "resize pointer")?,
            ),
            ("clientX", required(&event, "clientX", "resize pointer")?),
        ])?;
        call_method(&move_controller, "moveDetailsResize", &[input.into()])?;
        move_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let end_controller = controller.clone();
    let on_pointer_up = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let pointer = required(&event, "pointerId", "resize pointer")?;
        call_method(
            &end_controller,
            "endDetailsResize",
            std::slice::from_ref(&pointer),
        )?;
        release_pointer(&event)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let cancel_controller = controller.clone();
    let on_pointer_cancel = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(&cancel_controller, "cancelDetailsResize", &[]).map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);

    let key_controller = controller.clone();
    let key_bump = bump.clone();
    let on_key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required_string(&event, "key", "resize key event")?;
        let direction = match key.as_str() {
            "ArrowLeft" => 1.0,
            "ArrowRight" => -1.0,
            _ => return Ok(()),
        };
        let current = required(&event, "currentTarget", "resize key event")?;
        let details = required(&current, "parentElement", "resize handle")?;
        let split = required(&details, "parentElement", "details panel")?;
        let details_rect = call_method(&details, "getBoundingClientRect", &[])?;
        let split_rect = call_method(&split, "getBoundingClientRect", &[])?;
        call_method(
            &key_controller,
            "keyboardDetailsResize",
            &[
                JsValue::from_f64(direction),
                required(&details_rect, "width", "details rectangle")?,
                required(&split_rect, "width", "split rectangle")?,
            ],
        )?;
        key_bump.call0(&JsValue::UNDEFINED)?;
        prevent_default(&event)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    ui.tag(
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-detailsResizeHandle"),
            ),
            ("role", JsValue::from_str("separator")),
            ("aria-label", JsValue::from_str("Resize event details")),
            ("aria-orientation", JsValue::from_str("vertical")),
            ("tabIndex", JsValue::from_f64(0.0)),
            (
                "title",
                JsValue::from_str("Drag to resize. Double-click to reset."),
            ),
            ("onDoubleClick", on_double_click.into_js_value()),
            ("onPointerDown", on_pointer_down.into_js_value()),
            ("onPointerMove", on_pointer_move.into_js_value()),
            ("onPointerUp", on_pointer_up.into_js_value()),
            ("onPointerCancel", on_pointer_cancel.into_js_value()),
            ("onKeyDown", on_key_down.into_js_value()),
        ])?),
        &[],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One inspector switch shares projection context.
fn render_inspector_body(
    ui: &ReactUi,
    controller: &JsValue,
    bump: &Function,
    state: &TrajectoryTableControllerSnapshot,
    selected: Option<&TrajectoryTableRecord>,
    all_records: &[TrajectoryTableRecord],
    session_requests: &[TrajectoryRequestNumber],
    request_numbers: &BTreeMap<String, u64>,
    streaming: &BTreeMap<usize, TrajectoryCell>,
) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    if let Some(record) = selected {
        match state.active_tab {
            TrajectoryDetailTab::Overview => {
                let mut rows = vec![
                    (
                        "Status",
                        trajectory_status_label(trajectory_record_state(record)).to_owned(),
                    ),
                    (
                        "Duration",
                        crate::format_elapsed_seconds(record.cell.time_seconds),
                    ),
                ];
                if record.cell.kind == TrajectoryCellKind::Message {
                    rows.push((
                        "Tokens",
                        record
                            .cell
                            .output
                            .map_or_else(|| "—".to_owned(), |tokens| format!("{tokens} tok")),
                    ));
                    if let Some(reasoning) = record.cell.think {
                        rows.push(("Reasoning", format!("{reasoning} tok")));
                        if let Some(output) = record.cell.output {
                            rows.push((
                                "Content",
                                format!("{} tok", output.saturating_sub(reasoning)),
                            ));
                        }
                    }
                }
                children.push(definition_list(ui, &rows)?);
                if matches!(
                    record.cell.kind,
                    TrajectoryCellKind::User
                        | TrajectoryCellKind::Context
                        | TrajectoryCellKind::Message
                ) {
                    children.push(summary_preview(ui, controller, bump, state, record)?);
                }
                if record.cell.kind == TrajectoryCellKind::Message {
                    let request_controller = controller.clone();
                    let request_bump = bump.clone();
                    let turn = record.turn;
                    let group = record.group.clone();
                    let open_timing = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                        let request = SelectedTrajectoryRequest {
                            turn,
                            group: group.clone(),
                            seq: None,
                        };
                        call_method(
                            &request_controller,
                            "selectRequest",
                            &[
                                serde_wasm_bindgen::to_value(&request)
                                    .map_err(js_error_from_display)?,
                                JsValue::from_str("timing"),
                            ],
                        )?;
                        request_bump.call0(&JsValue::UNDEFINED)?;
                        Ok(())
                    })
                        as Box<dyn FnMut() -> Result<(), JsValue>>);
                    let button = ui.tag(
                        "button",
                        Some(&object(&[
                            ("type", JsValue::from_str("button")),
                            ("aria-label", JsValue::from_str("Request Timing")),
                            ("onClick", open_timing.into_js_value()),
                        ])?),
                        &[JsValue::from_str("Request Timing")],
                    )?;
                    children.push(ui.tag(
                        "section",
                        Some(&object(&[(
                            "data-summary-scroll-region",
                            JsValue::from_str(""),
                        )])?),
                        &[button],
                    )?);
                }
                if record.cell.kind == TrajectoryCellKind::Compacted
                    && record.cell.output_detail.is_some()
                {
                    children.push(render_markdown_record(
                        ui, controller, bump, state, record, true, true,
                    )?);
                }
            }
            TrajectoryDetailTab::Timing => {
                if let Some(metrics) = &record.cell.assistant_metrics {
                    children.push(definition_list(
                        ui,
                        &[
                            (
                                "Total duration",
                                crate::trajectory_assistant_total_time(metrics),
                            ),
                            ("TTFT", crate::trajectory_assistant_ttft(metrics)),
                            (
                                "Generation",
                                crate::trajectory_assistant_generation_time(metrics),
                            ),
                            (
                                "Throughput",
                                crate::trajectory_assistant_throughput(metrics),
                            ),
                        ],
                    )?);
                } else {
                    children.push(definition_list(
                        ui,
                        &[(
                            "Duration",
                            crate::format_elapsed_seconds(record.cell.time_seconds),
                        )],
                    )?);
                }
            }
            TrajectoryDetailTab::Output => {
                children.push(render_record_payload(ui, record, false)?);
            }
            TrajectoryDetailTab::Input => {
                children.push(render_record_payload(ui, record, true)?);
            }
            TrajectoryDetailTab::Rendered | TrajectoryDetailTab::Raw => {
                children.push(render_markdown_record(
                    ui,
                    controller,
                    bump,
                    state,
                    record,
                    state.active_tab == TrajectoryDetailTab::Rendered,
                    false,
                )?);
            }
            TrajectoryDetailTab::Source => children.push(render_message_source(ui, record)?),
            TrajectoryDetailTab::Schema => children.push(render_record_schema(ui, record)?),
            TrajectoryDetailTab::SystemPrompt => {
                children.push(render_system_prompt(ui, record)?);
            }
            TrajectoryDetailTab::Tools => children.push(render_tool_catalog(ui, record)?),
            TrajectoryDetailTab::Diff => children.push(render_prompt_diff(ui, record)?),
            _ => children.push(ui.tag("p", None, &[JsValue::from_str("No payload captured")])?),
        }
    } else if let Some(request) = &state.selected_request {
        let request_records = all_records
            .iter()
            .filter(|record| record.turn == request.turn && record.group == request.group)
            .map(|record| current_record(record, streaming))
            .collect::<Vec<_>>();
        let assistant = request_records
            .iter()
            .find(|record| record.cell.kind == TrajectoryCellKind::Message);
        let number = request_numbers
            .get(&trajectory_request_key(request.turn, &request.group))
            .copied();
        let info = session_requests.iter().find(|candidate| {
            request.seq.map_or_else(
                || candidate.turn == request.turn && candidate.group == request.group,
                |seq| candidate.seq == Some(seq),
            )
        });
        let assistant_usage = assistant.map(|record| crate::TrajectoryUsage {
            input: record.cell.input,
            cache_read: record.cell.cache_read,
            cache_write: record.cell.cache_write,
            output: record.cell.output,
            reasoning: record.cell.think,
        });
        let usage = info.and_then(|request| request.usage).or(assistant_usage);
        let cumulative = info.and_then(|request| request.cumulative_usage).or(usage);
        match state.active_tab {
            TrajectoryDetailTab::Options => {
                children.push(render_request_options(
                    ui,
                    info.and_then(|request| request.request_config.as_ref()),
                )?);
            }
            TrajectoryDetailTab::Usage => {
                children.push(render_request_usage(ui, usage, cumulative)?);
            }
            TrajectoryDetailTab::Timing => {
                if let Some(metrics) =
                    assistant.and_then(|record| record.cell.assistant_metrics.as_ref())
                {
                    children.push(definition_list(
                        ui,
                        &[
                            (
                                "Total duration",
                                crate::trajectory_assistant_total_time(metrics),
                            ),
                            ("TTFT", crate::trajectory_assistant_ttft(metrics)),
                            (
                                "Generation",
                                crate::trajectory_assistant_generation_time(metrics),
                            ),
                            (
                                "Throughput",
                                crate::trajectory_assistant_throughput(metrics),
                            ),
                        ],
                    )?);
                } else {
                    children.push(definition_list(
                        ui,
                        &[(
                            "Duration",
                            info.and_then(|request| {
                                request.completed_at.zip(request.started_at).map(
                                    |(completed, started)| {
                                        crate::format_elapsed_seconds(Some(
                                            (completed - started).max(0.0) / 1_000.0,
                                        ))
                                    },
                                )
                            })
                            .unwrap_or_else(|| crate::format_elapsed_seconds(None)),
                        )],
                    )?);
                }
            }
            _ => {
                let mut rows = vec![
                    (
                        "Status",
                        info.and_then(|request| request.status).map_or_else(
                            || "Completed".to_owned(),
                            |state| trajectory_status_label(state).to_owned(),
                        ),
                    ),
                    (
                        "Request",
                        number.map_or_else(|| "#—".to_owned(), |number| format!("#{number}")),
                    ),
                    (
                        "Tool calls",
                        request_records
                            .iter()
                            .filter(|record| record.cell.kind == TrajectoryCellKind::Tool)
                            .count()
                            .to_string(),
                    ),
                ];
                if let Some(provider) = info
                    .and_then(|request| request.provider.as_deref())
                    .or_else(|| {
                        info.and_then(|request| request.request_config.as_ref())
                            .and_then(|config| config.get("provider"))
                            .and_then(serde_json::Value::as_str)
                    })
                {
                    rows.push(("Provider", provider.to_owned()));
                }
                if let Some(model) =
                    info.and_then(|request| request.model.as_deref())
                        .or_else(|| {
                            info.and_then(|request| request.request_config.as_ref())
                                .and_then(|config| config.get("model"))
                                .and_then(serde_json::Value::as_str)
                        })
                {
                    rows.push(("Model", model.to_owned()));
                }
                if let Some(error) = info.and_then(|request| request.error.as_ref()) {
                    rows.push(("Error", error.clone()));
                }
                if let Some(retry) = info.and_then(|request| request.retry) {
                    let maximum = info.and_then(|request| request.max_retries);
                    rows.push((
                        "Retry",
                        maximum.map_or_else(
                            || format!("Scheduled {retry}"),
                            |maximum| format!("Scheduled {retry} of {maximum}"),
                        ),
                    ));
                }
                children.push(definition_list(ui, &rows)?);
            }
        }
    }
    ui.tag(
        "div",
        Some(&object(&[
            ("role", JsValue::from_str("tabpanel")),
            (
                "className",
                JsValue::from_str(if state.active_tab == TrajectoryDetailTab::Overview {
                    "seekdeep-trajectory-table-detailBody seekdeep-trajectory-table-detailBodySummary"
                } else {
                    "seekdeep-trajectory-table-detailBody"
                }),
            ),
        ])?),
        &children,
    )
}

fn summary_preview(
    ui: &ReactUi,
    controller: &JsValue,
    bump: &Function,
    state: &TrajectoryTableControllerSnapshot,
    record: &TrajectoryTableRecord,
) -> Result<JsValue, JsValue> {
    let tab_controller = controller.clone();
    let tab_bump = bump.clone();
    let open = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        call_method(
            &tab_controller,
            "activateTab",
            &[JsValue::from_str("rendered")],
        )?;
        tab_bump.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let heading = ui.tag(
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("aria-label", JsValue::from_str("Preview")),
            ("onClick", open.into_js_value()),
        ])?),
        &[JsValue::from_str("Preview")],
    )?;
    let content = render_markdown_record(ui, controller, bump, state, record, true, true)?;
    ui.tag(
        "section",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-overviewSection"),
            ),
            ("data-summary-scroll-region", JsValue::from_str("")),
        ])?),
        &[heading, content],
    )
}

fn render_markdown_record(
    ui: &ReactUi,
    controller: &JsValue,
    bump: &Function,
    state: &TrajectoryTableControllerSnapshot,
    record: &TrajectoryTableRecord,
    rendered: bool,
    preview: bool,
) -> Result<JsValue, JsValue> {
    if let Some(thinking) = &record.cell.thinking_detail {
        if !rendered {
            let source = [
                Some(thinking.as_str()),
                record.cell.output_detail.as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
            return ui.tag("pre", None, &[JsValue::from_str(&source)]);
        }
        let toggle_controller = controller.clone();
        let toggle_bump = bump.clone();
        let toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            call_method(&toggle_controller, "toggleThinking", &[])?;
            toggle_bump.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let mut children = vec![ui.tag(
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                ("aria-label", JsValue::from_str("Thinking")),
                ("aria-expanded", JsValue::from_bool(state.thinking_expanded)),
                ("onClick", toggle.into_js_value()),
            ])?),
            &[JsValue::from_str("Thinking")],
        )?];
        if state.thinking_expanded {
            children.push(markdown_text(ui, thinking, preview)?);
        }
        if let Some(output) = record
            .cell
            .output_detail
            .as_deref()
            .filter(|output| !output.is_empty())
        {
            children.push(markdown_text(ui, output, preview)?);
        }
        return ui.tag(
            "div",
            Some(&class("seekdeep-trajectory-table-assistantContent")?),
            &children,
        );
    }
    let source = match record.cell.kind {
        TrajectoryCellKind::User | TrajectoryCellKind::Context => {
            record.cell.input_detail.as_deref()
        }
        TrajectoryCellKind::Message | TrajectoryCellKind::Compacted => {
            record.cell.output_detail.as_deref()
        }
        _ => None,
    };
    if source.is_none_or(str::is_empty) {
        return ui.tag(
            "p",
            Some(&class("seekdeep-trajectory-table-noPayload")?),
            &[JsValue::from_str(
                if trajectory_is_tool_call_only(&record.cell) {
                    "Tool call only"
                } else if record.cell.text.is_empty() {
                    "No content"
                } else {
                    &record.cell.text
                },
            )],
        );
    }
    if rendered {
        markdown_text(ui, source.unwrap_or_default(), preview)
    } else {
        ui.tag(
            "pre",
            Some(&class("seekdeep-trajectory-table-markdownPayload")?),
            &[JsValue::from_str(source.unwrap_or_default())],
        )
    }
}

fn markdown_text(ui: &ReactUi, text: &str, preview: bool) -> Result<JsValue, JsValue> {
    ui.primitive(
        "MarkdownText",
        Some(&object(&[
            ("text", JsValue::from_str(text)),
            ("data-preview", bool_data(preview)),
        ])?),
        &[],
    )
}

fn render_record_payload(
    ui: &ReactUi,
    record: &TrajectoryTableRecord,
    input: bool,
) -> Result<JsValue, JsValue> {
    let value = if input {
        record.cell.input_detail.as_deref()
    } else {
        record.cell.output_detail.as_deref()
    };
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return ui.tag(
            "p",
            Some(&class("seekdeep-trajectory-table-noPayload")?),
            &[JsValue::from_str(if input {
                "No payload captured"
            } else {
                "No result captured"
            })],
        );
    };
    let error = !input && record.cell.is_error == Some(true);
    let class_name = if error {
        "seekdeep-trajectory-table-payload seekdeep-trajectory-table-errorPayload"
    } else {
        "seekdeep-trajectory-table-payload"
    };
    let single_text_result = !input
        && record.cell.output_blocks.len() == 1
        && record.cell.output_blocks[0].kind == "text";
    if (single_text_result || record.cell.output_blocks.is_empty())
        && let Some(json) = crate::parse_trajectory_json_container(value)
    {
        return ui.primitive(
            "JsonTree",
            Some(&object(&[
                (
                    "data",
                    serde_wasm_bindgen::to_value(&json).map_err(js_error_from_display)?,
                ),
                (
                    "label",
                    JsValue::from_str(if input { "Payload JSON" } else { "Result JSON" }),
                ),
                ("className", JsValue::from_str(class_name)),
            ])?),
            &[],
        );
    }
    let blocks = if input {
        &record.cell.source_blocks
    } else {
        &record.cell.output_blocks
    };
    if !blocks.is_empty() {
        let mut children = Vec::new();
        for block in blocks {
            if let Some(source) = &block.image_src {
                children.push(ui.tag(
                    "img",
                    Some(&object(&[
                        ("src", JsValue::from_str(source)),
                        (
                            "alt",
                            JsValue::from_str(block.image_alt.as_deref().unwrap_or("")),
                        ),
                    ])?),
                    &[],
                )?);
            } else if !block.content.is_empty() {
                children.push(ui.tag("pre", None, &[JsValue::from_str(&block.content)])?);
            }
        }
        return ui.tag(
            "div",
            Some(&object(&[("className", JsValue::from_str(class_name))])?),
            &children,
        );
    }
    ui.tag(
        "pre",
        Some(&object(&[("className", JsValue::from_str(class_name))])?),
        &[JsValue::from_str(value)],
    )
}

fn render_message_source(ui: &ReactUi, record: &TrajectoryTableRecord) -> Result<JsValue, JsValue> {
    let Some(source) = &record.cell.message_source else {
        return ui.tag("p", None, &[JsValue::from_str("Source not recorded")]);
    };
    ui.primitive(
        "JsonTree",
        Some(&object(&[
            (
                "data",
                serde_wasm_bindgen::to_value(source).map_err(js_error_from_display)?,
            ),
            ("label", JsValue::from_str("Message source JSON")),
        ])?),
        &[],
    )
}

fn render_record_schema(ui: &ReactUi, record: &TrajectoryTableRecord) -> Result<JsValue, JsValue> {
    let Some(schema) = record.cell.schema_detail.as_deref() else {
        return ui.tag("p", None, &[JsValue::from_str("Schema unavailable")]);
    };
    let Some(parsed) = crate::parse_trajectory_tool_schema(schema) else {
        return ui.tag("pre", None, &[JsValue::from_str(schema)]);
    };
    ui.tag(
        "section",
        None,
        &[
            ui.tag("h3", None, &[JsValue::from_str(&parsed.name)])?,
            ui.tag("p", None, &[JsValue::from_str(&parsed.description)])?,
            ui.primitive(
                "JsonTree",
                Some(&object(&[
                    (
                        "data",
                        serde_wasm_bindgen::to_value(&parsed.parameters)
                            .map_err(js_error_from_display)?,
                    ),
                    (
                        "label",
                        JsValue::from_str(&format!("{} parameters JSON", parsed.name)),
                    ),
                ])?),
                &[],
            )?,
        ],
    )
}

fn render_system_prompt(ui: &ReactUi, record: &TrajectoryTableRecord) -> Result<JsValue, JsValue> {
    let prompt = record
        .cell
        .prompt_detail
        .as_ref()
        .and_then(|prompt| prompt.get("system"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if prompt.is_empty() {
        return ui.tag(
            "p",
            Some(&class("seekdeep-trajectory-table-noPayload")?),
            &[JsValue::from_str("No system prompt in this request")],
        );
    }
    ui.tag(
        "div",
        Some(&class(
            "seekdeep-trajectory-table-markdownPayload seekdeep-trajectory-table-systemPrompt",
        )?),
        &[markdown_text(ui, prompt, false)?],
    )
}

fn render_tool_catalog(ui: &ReactUi, record: &TrajectoryTableRecord) -> Result<JsValue, JsValue> {
    let tools = record
        .cell
        .prompt_detail
        .as_ref()
        .and_then(|prompt| prompt.get("tools"))
        .and_then(serde_json::Value::as_array);
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else {
        return ui.tag(
            "p",
            Some(&class("seekdeep-trajectory-table-noPayload")?),
            &[JsValue::from_str("No tools in this request")],
        );
    };
    let mut children = Vec::new();
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let description = tool
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let parameters = tool
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let summary = ui.tag(
            "summary",
            Some(&class("seekdeep-trajectory-table-toolCatalogSummary")?),
            &[
                ui.tag(
                    "span",
                    Some(&class("seekdeep-trajectory-table-toolCatalogName")?),
                    &[JsValue::from_str(name)],
                )?,
                ui.tag(
                    "span",
                    Some(&class("seekdeep-trajectory-table-toolCatalogDescription")?),
                    &[JsValue::from_str(description)],
                )?,
            ],
        )?;
        let definition = ui.primitive(
            "JsonTree",
            Some(&object(&[
                (
                    "data",
                    serde_wasm_bindgen::to_value(&parameters).map_err(js_error_from_display)?,
                ),
                (
                    "label",
                    JsValue::from_str(&format!("{name} parameters JSON")),
                ),
                (
                    "className",
                    JsValue::from_str("seekdeep-trajectory-table-toolCatalogTree"),
                ),
            ])?),
            &[],
        )?;
        children.push(ui.tag(
            "details",
            Some(&class("seekdeep-trajectory-table-toolCatalogItem")?),
            &[summary, definition],
        )?);
    }
    ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-table-toolCatalog")?),
        &children,
    )
}

fn render_prompt_diff(ui: &ReactUi, record: &TrajectoryTableRecord) -> Result<JsValue, JsValue> {
    let before = record
        .cell
        .previous_prompt_detail
        .clone()
        .unwrap_or_else(|| serde_json::json!({"system": "", "tools": []}));
    let after = record
        .cell
        .prompt_detail
        .clone()
        .unwrap_or_else(|| serde_json::json!({"system": "", "tools": []}));
    let before = serde_json::to_string_pretty(&before).map_err(js_error_from_display)?;
    let after = serde_json::to_string_pretty(&after).map_err(js_error_from_display)?;
    ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-table-promptDiffSections")?),
        &[
            ui.tag(
                "h3",
                Some(&class("seekdeep-trajectory-table-promptDiffTitle")?),
                &[JsValue::from_str("Prompt Update")],
            )?,
            ui.tag(
                "pre",
                Some(&class("seekdeep-trajectory-table-promptDiff")?),
                &[JsValue::from_str(&format!(
                    "--- before\n{before}\n+++ after\n{after}"
                ))],
            )?,
        ],
    )
}

fn render_request_options(
    ui: &ReactUi,
    options: Option<&serde_json::Value>,
) -> Result<JsValue, JsValue> {
    let Some(options) = options else {
        return ui.tag(
            "p",
            Some(&class("seekdeep-trajectory-table-noPayload")?),
            &[JsValue::from_str("Options not recorded")],
        );
    };
    ui.primitive(
        "JsonTree",
        Some(&object(&[
            (
                "data",
                serde_wasm_bindgen::to_value(options).map_err(js_error_from_display)?,
            ),
            ("label", JsValue::from_str("Request options JSON")),
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-table-jsonPayload"),
            ),
        ])?),
        &[],
    )
}

fn render_request_usage(
    ui: &ReactUi,
    usage: Option<crate::TrajectoryUsage>,
    cumulative: Option<crate::TrajectoryUsage>,
) -> Result<JsValue, JsValue> {
    let request = ui.tag(
        "section",
        Some(&class("seekdeep-trajectory-table-usageGroup")?),
        &[
            ui.tag(
                "h4",
                Some(&class("seekdeep-trajectory-table-usageHeading")?),
                &[JsValue::from_str("This request")],
            )?,
            usage_rows(ui, usage)?,
        ],
    )?;
    let cumulative = ui.tag(
        "section",
        Some(&class("seekdeep-trajectory-table-usageGroup")?),
        &[
            ui.tag(
                "h4",
                Some(&class("seekdeep-trajectory-table-usageHeading")?),
                &[JsValue::from_str("Session cumulative")],
            )?,
            usage_rows(ui, cumulative)?,
        ],
    )?;
    ui.tag(
        "div",
        Some(&class("seekdeep-trajectory-table-usagePanel")?),
        &[request, cumulative],
    )
}

fn usage_rows(ui: &ReactUi, usage: Option<crate::TrajectoryUsage>) -> Result<JsValue, JsValue> {
    let Some(usage) = usage else {
        return ui.tag(
            "p",
            Some(&class("seekdeep-trajectory-table-noPayload")?),
            &[JsValue::from_str("Usage not reported")],
        );
    };
    let mut rows = Vec::new();
    if let Some(input) = crate::trajectory_input_total(usage) {
        rows.push(("Input", format!("{input} tok")));
    }
    if let Some(cached) = usage.cache_read {
        rows.push(("Cached", format!("{cached} tok")));
    }
    if let Some(created) = usage.cache_write {
        rows.push(("Cache created", format!("{created} tok")));
    }
    if let Some(other) = usage.input {
        rows.push(("Other", format!("{other} tok")));
    }
    if let Some(output) = usage.output {
        rows.push(("Output", format!("{output} tok")));
    }
    if let Some(reasoning) = usage.reasoning {
        rows.push(("Reasoning", format!("{reasoning} tok")));
        if let Some(output) = usage.output {
            rows.push((
                "Content",
                format!("{} tok", output.saturating_sub(reasoning)),
            ));
        }
    }
    definition_list(ui, &rows)
}

fn definition_list(ui: &ReactUi, rows: &[(&str, String)]) -> Result<JsValue, JsValue> {
    let mut children = Vec::new();
    for (label, value) in rows {
        children.push(ui.tag(
            "div",
            None,
            &[
                ui.tag("dt", None, &[JsValue::from_str(label)])?,
                ui.tag("dd", None, &[JsValue::from_str(value)])?,
            ],
        )?);
    }
    ui.tag(
        "dl",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(
                    "seekdeep-trajectory-table-overview seekdeep-trajectory-table-summaryScrollRegion",
                ),
            ),
            ("data-summary-scroll-region", JsValue::from_str("")),
        ])?),
        &children,
    )
}

fn install_external_selection_effect(
    ui: &ReactUi,
    props: &JsValue,
    controller: &JsValue,
    bump: &Function,
    turns: &JsValue,
) -> Result<(), JsValue> {
    let selection = optional(props, "recordSelection")?.unwrap_or(JsValue::NULL);
    let effect_controller = controller.clone();
    let effect_bump = bump.clone();
    let effect_selection = selection.clone();
    let on_record_select = optional_function(props, "onRecordSelect")?;
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if call_method(
            &effect_controller,
            "applyRecordSelection",
            std::slice::from_ref(&effect_selection),
        )?
        .as_bool()
            == Some(true)
        {
            if let Some(callback) = &on_record_select
                && let Some(index) = optional_number(&effect_selection, "index")
            {
                callback.call1(&JsValue::UNDEFINED, &JsValue::from_f64(index))?;
            }
            effect_bump.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &ui.react,
        &effect.into_js_value(),
        &Array::of2(&selection, turns),
    )
}

fn install_external_focus_effect(
    ui: &ReactUi,
    props: &JsValue,
    controller: &JsValue,
    bump: &Function,
    turns: &JsValue,
) -> Result<(), JsValue> {
    let focus = optional(props, "recordFocus")?.unwrap_or(JsValue::NULL);
    let effect_controller = controller.clone();
    let effect_bump = bump.clone();
    let effect_focus = focus.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if call_method(
            &effect_controller,
            "applyRecordFocus",
            std::slice::from_ref(&effect_focus),
        )?
        .as_bool()
            == Some(true)
        {
            effect_bump.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &ui.react,
        &effect.into_js_value(),
        &Array::of2(&focus, turns),
    )
}

fn install_inspect_effect(
    ui: &ReactUi,
    props: &JsValue,
    controller: &JsValue,
    bump: &Function,
    turns: &JsValue,
    records: &[TrajectoryTableRecord],
) -> Result<(), JsValue> {
    let inspect = optional(props, "inspectCallId")?.unwrap_or(JsValue::NULL);
    let acknowledge = optional_function(props, "onInspectApplied")?;
    let toggle_turn = required_function(props, "onToggleTurn")?;
    let toggle_assistant = required_function(props, "onToggleAssistant")?;
    let collapsed_turns = u64_set(optional(props, "collapsedTurns")?.as_ref())?;
    let collapsed_assistants = string_set(optional(props, "collapsedAssistants")?.as_ref())?;
    let target = inspect.as_string().and_then(|call_id| {
        records
            .iter()
            .position(|record| record.cell.call_id.as_deref() == Some(&call_id))
            .map(|at| {
                let record = &records[at];
                let turn = record.turn.filter(|turn| collapsed_turns.contains(turn));
                let assistant = matches!(
                    record.cell.kind,
                    TrajectoryCellKind::Tool | TrajectoryCellKind::Subtool
                )
                .then(|| {
                    records[..at]
                        .iter()
                        .rev()
                        .take_while(|candidate| candidate.turn == record.turn)
                        .find(|candidate| candidate.cell.kind == TrajectoryCellKind::Message)
                        .map(|assistant| trajectory_record_id(&assistant.cell))
                })
                .flatten()
                .filter(|assistant| collapsed_assistants.contains(assistant));
                (turn, assistant)
            })
    });
    let effect_controller = controller.clone();
    let effect_bump = bump.clone();
    let effect_inspect = inspect.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let Some(call_id) = effect_inspect.as_string() else {
            return Ok(());
        };
        if let Some((turn, assistant)) = &target {
            if let Some(turn) = turn {
                toggle_turn.call1(&JsValue::UNDEFINED, &JsValue::from_f64(u64_as_f64(*turn)))?;
            }
            if let Some(assistant) = assistant {
                toggle_assistant.call1(&JsValue::UNDEFINED, &JsValue::from_str(assistant))?;
            }
        }
        if call_method(
            &effect_controller,
            "inspectCall",
            &[JsValue::from_str(&call_id)],
        )?
        .as_bool()
            == Some(true)
        {
            if let Some(callback) = &acknowledge {
                callback.call0(&JsValue::UNDEFINED)?;
            }
            effect_bump.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &ui.react,
        &effect.into_js_value(),
        &Array::of2(&inspect, turns),
    )
}

fn install_selected_index_effect(
    ui: &ReactUi,
    props: &JsValue,
    selected_index: Option<usize>,
) -> Result<(), JsValue> {
    let callback = optional_function(props, "onSelectedIndexChange")?;
    let selected = selected_index.map_or(JsValue::NULL, |index| {
        JsValue::from_f64(usize_as_f64(index))
    });
    let selected_effect = selected.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if let Some(callback) = &callback {
            callback.call1(&JsValue::UNDEFINED, &selected_effect)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(&ui.react, &effect.into_js_value(), &Array::of1(&selected))
}

#[allow(clippy::too_many_arguments)]
fn install_pending_scroll_effect(
    ui: &ReactUi,
    controller: &JsValue,
    pane_ref: &JsValue,
    set_viewport: &Function,
    pending_id: Option<&str>,
    records: &[TrajectoryTableRecord],
    virtualized: bool,
    turns: &JsValue,
) -> Result<(), JsValue> {
    let pending = pending_id.map_or(JsValue::NULL, JsValue::from_str);
    let target = pending_id.and_then(|id| {
        records
            .iter()
            .position(|record| {
                record.collapsed_summary.is_none() && trajectory_record_id(&record.cell) == id
            })
            .map(|position| (position, records[position].cell.index))
    });
    let effect_controller = controller.clone();
    let effect_ref = pane_ref.clone();
    let effect_setter = set_viewport.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let Some((position, index)) = target else {
            return Ok(());
        };
        let pane = Reflect::get(&effect_ref, &JsValue::from_str("current"))?;
        if pane.is_null() || pane.is_undefined() {
            return Ok(());
        }
        if virtualized {
            let scroll_top = usize_as_f64(position) * 30.0;
            Reflect::set(
                &pane,
                &JsValue::from_str("scrollTop"),
                &JsValue::from_f64(scroll_top),
            )?;
            let height = number_member(&pane, "clientHeight")?;
            effect_setter.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("scrollTop", JsValue::from_f64(scroll_top)),
                    ("height", JsValue::from_f64(height)),
                ])?
                .into(),
            )?;
        } else {
            let selector = format!("tr[data-record-index=\"{index}\"]");
            let query = Reflect::get(&pane, &JsValue::from_str("querySelector"))?;
            if query.is_function() {
                let row = query
                    .dyn_into::<Function>()?
                    .call1(&pane, &JsValue::from_str(&selector))?;
                if !row.is_null() && !row.is_undefined() {
                    let method = Reflect::get(&row, &JsValue::from_str("scrollIntoView"))?;
                    if method.is_function() {
                        method.dyn_into::<Function>()?.call1(
                            &row,
                            &object(&[
                                ("behavior", JsValue::from_str("smooth")),
                                ("block", JsValue::from_str("center")),
                            ])?
                            .into(),
                        )?;
                    }
                }
            }
        }
        call_method(&effect_controller, "takePendingScroll", &[])?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &ui.react,
        &effect.into_js_value(),
        &Array::of2(&pending, turns),
    )
}

#[allow(clippy::too_many_arguments)]
fn install_scroll_reconciliation_effect(
    ui: &ReactUi,
    props: &JsValue,
    controller: &JsValue,
    pane_ref: &JsValue,
    bump: &Function,
    virtualized: bool,
    turns: &JsValue,
) -> Result<(), JsValue> {
    let history_loading = optional_bool(props, "historyLoading")?.unwrap_or(false);
    let history_start = optional_number(props, "historyStartSeq");
    let effect_controller = controller.clone();
    let effect_ref = pane_ref.clone();
    let effect_bump = bump.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let pane = Reflect::get(&effect_ref, &JsValue::from_str("current"))?;
        if pane.is_null() || pane.is_undefined() {
            return Ok(());
        }
        let request = object(&[
            ("historyLoading", JsValue::from_bool(history_loading)),
            (
                "historyStartSeq",
                history_start.map_or(JsValue::UNDEFINED, JsValue::from_f64),
            ),
            ("virtualizationEnabled", JsValue::from_bool(virtualized)),
            ("metrics", metrics_value(&pane)?),
        ])?;
        let outcome = call_method(&effect_controller, "reconcileScroll", &[request.into()])?;
        match required_string(&outcome, "kind", "scroll outcome")?.as_str() {
            "set" => Reflect::set(
                &pane,
                &JsValue::from_str("scrollTop"),
                &required(&outcome, "scrollTop", "scroll outcome")?,
            )
            .map(|_| ())?,
            "end" => {
                let height = number_member(&pane, "scrollHeight")?;
                Reflect::set(
                    &pane,
                    &JsValue::from_str("scrollTop"),
                    &JsValue::from_f64(height),
                )?;
            }
            _ => {}
        }
        if bool_member(&outcome, "readyChanged")? {
            effect_bump.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&JsValue::from_bool(history_loading));
    dependencies.push(&history_start.map_or(JsValue::UNDEFINED, JsValue::from_f64));
    dependencies.push(turns);
    dependencies.push(&JsValue::from_bool(virtualized));
    use_layout_effect(&ui.react, &effect.into_js_value(), &dependencies)
}

fn table_scroll_handler(
    controller: &JsValue,
    bump: &Function,
    set_viewport: &Function,
    props: &JsValue,
    require_top: bool,
) -> Result<Function, JsValue> {
    let scroll_controller = controller.clone();
    let scroll_bump = bump.clone();
    let viewport_setter = set_viewport.clone();
    let scroll_props = props.clone();
    let handler = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let pane = required(&event, "currentTarget", "table scroll")?;
        let metrics = metrics_value(&pane)?;
        let top = call_method(
            &scroll_controller,
            "onScroll",
            std::slice::from_ref(&metrics),
        )?
        .as_bool()
        .unwrap_or(false);
        viewport_setter.call1(
            &JsValue::UNDEFINED,
            &object(&[
                ("scrollTop", required(&pane, "scrollTop", "table pane")?),
                ("height", required(&pane, "clientHeight", "table pane")?),
            ])?
            .into(),
        )?;
        if top {
            let _ = request_older(
                &scroll_controller,
                &scroll_bump,
                &scroll_props,
                &pane,
                require_top,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    handler.into_js_value().dyn_into::<Function>()
}

fn request_older(
    controller: &JsValue,
    bump: &Function,
    props: &JsValue,
    pane: &JsValue,
    require_top: bool,
) -> Result<Promise, JsValue> {
    let on_load = optional_function(props, "onLoadOlder")?;
    let request = object(&[
        (
            "hasOlderRecords",
            JsValue::from_bool(optional_bool(props, "hasOlderRecords")?.unwrap_or(false)),
        ),
        ("canLoadOlder", JsValue::from_bool(on_load.is_some())),
        (
            "externallyLoading",
            JsValue::from_bool(optional_bool(props, "olderHistoryLoading")?.unwrap_or(false)),
        ),
        ("requireTop", JsValue::from_bool(require_top)),
        (
            "historyStartSeq",
            optional_number(props, "historyStartSeq").map_or(JsValue::UNDEFINED, JsValue::from_f64),
        ),
        ("metrics", metrics_value(pane)?),
    ])?;
    let began = call_method(controller, "beginOlderLoad", &[request.into()])?
        .as_bool()
        .unwrap_or(false);
    if !began {
        return Ok(Promise::resolve(&JsValue::UNDEFINED));
    }
    bump.call0(&JsValue::UNDEFINED)?;
    let Some(on_load) = on_load else {
        return Ok(Promise::resolve(&JsValue::UNDEFINED));
    };
    let returned = on_load.call0(&JsValue::UNDEFINED)?;
    let promise = Promise::resolve(&returned);
    let async_controller = controller.clone();
    let async_bump = bump.clone();
    Ok(future_to_promise(async move {
        let settlement = JsFuture::from(promise).await;
        if let Ok(value) = &settlement {
            call_method(
                &async_controller,
                "recordOlderLoadResult",
                &[JsValue::from_bool(value.as_bool().unwrap_or(false))],
            )?;
        }
        call_method(&async_controller, "finishOlderLoad", &[])?;
        async_bump.call0(&JsValue::UNDEFINED)?;
        settlement.map(|_| JsValue::UNDEFINED)
    }))
}

fn controller_snapshot(controller: &JsValue) -> Result<TrajectoryTableControllerSnapshot, JsValue> {
    serde_wasm_bindgen::from_value(call_method(controller, "snapshot", &[])?)
        .map_err(js_error_from_display)
}

fn current_record(
    record: &TrajectoryTableRecord,
    streaming: &BTreeMap<usize, TrajectoryCell>,
) -> TrajectoryTableRecord {
    let mut current = record.clone();
    if let Some(cell) = streaming.get(&record.cell.index) {
        current.cell.clone_from(cell);
    }
    current
}

fn metrics_value(pane: &JsValue) -> Result<JsValue, JsValue> {
    Ok(object(&[
        ("scrollTop", required(pane, "scrollTop", "table pane")?),
        (
            "scrollHeight",
            required(pane, "scrollHeight", "table pane")?,
        ),
        (
            "clientHeight",
            required(pane, "clientHeight", "table pane")?,
        ),
    ])?
    .into())
}

fn scroll_metrics(value: &JsValue) -> Result<TrajectoryTableScrollMetrics, JsValue> {
    Ok(TrajectoryTableScrollMetrics {
        scroll_top: number_member(value, "scrollTop")?,
        scroll_height: number_member(value, "scrollHeight")?,
        client_height: number_member(value, "clientHeight")?,
    })
}

fn optional_number_set(props: &JsValue, key: &str) -> Result<Option<BTreeSet<usize>>, JsValue> {
    let Some(value) = optional(props, key)? else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    number_set(Some(&value)).map(Some)
}

fn number_set(value: Option<&JsValue>) -> Result<BTreeSet<usize>, JsValue> {
    let Some(value) = value.filter(|value| !value.is_null() && !value.is_undefined()) else {
        return Ok(BTreeSet::new());
    };
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("expected an iterable number set"))?;
    iterator
        .map(|entry| {
            entry.and_then(|entry| {
                entry.as_f64().and_then(f64_to_usize).ok_or_else(|| {
                    js_sys::Error::new("set entry must be a non-negative integer").into()
                })
            })
        })
        .collect()
}

fn u64_set(value: Option<&JsValue>) -> Result<BTreeSet<u64>, JsValue> {
    number_set(value)?
        .into_iter()
        .map(|entry| u64::try_from(entry).map_err(js_error_from_display))
        .collect()
}

fn string_set(value: Option<&JsValue>) -> Result<BTreeSet<String>, JsValue> {
    let Some(value) = value.filter(|value| !value.is_null() && !value.is_undefined()) else {
        return Ok(BTreeSet::new());
    };
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("expected an iterable string set"))?;
    iterator
        .map(|entry| {
            entry.and_then(|entry| {
                entry
                    .as_string()
                    .ok_or_else(|| js_sys::Error::new("set entry must be a string").into())
            })
        })
        .collect()
}

fn optional_vec<T: serde::de::DeserializeOwned>(
    props: &JsValue,
    key: &str,
) -> Result<Vec<T>, JsValue> {
    optional(props, key)?.map_or_else(
        || Ok(Vec::new()),
        |value| serde_wasm_bindgen::from_value(value).map_err(js_error_from_display),
    )
}

fn kind_label(kind: TrajectoryCellKind) -> &'static str {
    match kind {
        TrajectoryCellKind::System => "SYSTEM",
        TrajectoryCellKind::User => "USER",
        TrajectoryCellKind::Context => "CONTEXT",
        TrajectoryCellKind::Compacted => "COMPACTED",
        TrajectoryCellKind::Message => "ASSISTANT",
        TrajectoryCellKind::Tool => "TOOL",
        TrajectoryCellKind::Subtool => "SUBTOOL",
    }
}

fn bool_data(value: bool) -> JsValue {
    if value {
        JsValue::from_str("true")
    } else {
        JsValue::UNDEFINED
    }
}

fn parse_tab(value: &str) -> Result<TrajectoryDetailTab, JsValue> {
    match value {
        "system-prompt" => Ok(TrajectoryDetailTab::SystemPrompt),
        "tools" => Ok(TrajectoryDetailTab::Tools),
        "overview" => Ok(TrajectoryDetailTab::Overview),
        "rendered" => Ok(TrajectoryDetailTab::Rendered),
        "raw" => Ok(TrajectoryDetailTab::Raw),
        "source" => Ok(TrajectoryDetailTab::Source),
        "input" => Ok(TrajectoryDetailTab::Input),
        "output" => Ok(TrajectoryDetailTab::Output),
        "schema" => Ok(TrajectoryDetailTab::Schema),
        "options" => Ok(TrajectoryDetailTab::Options),
        "usage" => Ok(TrajectoryDetailTab::Usage),
        "timing" => Ok(TrajectoryDetailTab::Timing),
        "diff" => Ok(TrajectoryDetailTab::Diff),
        _ => Err(js_sys::Error::new(&format!("unknown trajectory detail tab {value:?}")).into()),
    }
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

fn use_layout_effect(
    react: &JsValue,
    effect: &JsValue,
    dependencies: &Array,
) -> Result<(), JsValue> {
    let layout = Reflect::get(react, &JsValue::from_str("useLayoutEffect"))?;
    let function = if layout.is_undefined() {
        function(react, "useEffect")?
    } else {
        layout.dyn_into::<Function>()?
    };
    function.call2(react, effect, dependencies).map(|_| ())
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

fn required_function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required(value, key, "TrajectoryTable props")?.dyn_into::<Function>()
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    optional(value, key)?
        .filter(|entry| !entry.is_null())
        .map(JsCast::dyn_into::<Function>)
        .transpose()
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
    required(value, key, "object")?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("object {key:?} must be a number")).into())
}

fn bool_member(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    required(value, key, "object")?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new(&format!("object {key:?} must be boolean")).into())
}

fn optional_number(value: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_f64())
}

fn optional_bool(value: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    optional(value, key)?.map_or_else(
        || Ok(None),
        |value| {
            value
                .as_bool()
                .map(Some)
                .ok_or_else(|| js_sys::Error::new(&format!("{key:?} must be boolean")).into())
        },
    )
}

fn optional_usize(value: &JsValue, key: &str) -> Option<usize> {
    optional_number(value, key).and_then(f64_to_usize)
}

fn prevent_default(event: &JsValue) -> Result<(), JsValue> {
    call_method(event, "preventDefault", &[]).map(|_| ())
}

fn stop_propagation(event: &JsValue) -> Result<(), JsValue> {
    call_method(event, "stopPropagation", &[]).map(|_| ())
}

fn capture_pointer(event: &JsValue) -> Result<(), JsValue> {
    let current = required(event, "currentTarget", "pointer event")?;
    let pointer = required(event, "pointerId", "pointer event")?;
    let method = Reflect::get(&current, &JsValue::from_str("setPointerCapture"))?;
    if method.is_function() {
        method.dyn_into::<Function>()?.call1(&current, &pointer)?;
    }
    Ok(())
}

fn release_pointer(event: &JsValue) -> Result<(), JsValue> {
    let current = required(event, "currentTarget", "pointer event")?;
    let pointer = required(event, "pointerId", "pointer event")?;
    let method = Reflect::get(&current, &JsValue::from_str("releasePointerCapture"))?;
    if method.is_function() {
        method.dyn_into::<Function>()?.call1(&current, &pointer)?;
    }
    Ok(())
}

fn i32_member(value: &JsValue, key: &str) -> Result<i32, JsValue> {
    f64_to_i32(number_member(value, key)?)
        .ok_or_else(|| js_sys::Error::new(&format!("object {key:?} must be an i32")).into())
}

fn f64_to_usize(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as usize)
}

fn f64_to_i32(value: f64) -> Option<i32> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value < f64::from(i32::MIN)
        || value > f64::from(i32::MAX)
    {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(value as i32)
}

fn f64_to_i8(value: f64) -> Option<i8> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value < f64::from(i8::MIN)
        || value > f64::from(i8::MAX)
    {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(value as i8)
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
