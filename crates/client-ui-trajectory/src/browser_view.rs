//! Rust/WASM React composition for the complete trajectory view.

use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Map as JsMap, Object, Reflect, Set as JsSet};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    TrajectoryLocation, TrajectorySearchOffer, TrajectorySequence, TrajectorySnapshot,
    TrajectoryTimeRange, TrajectoryViewSearchController, all_trajectory_folds_selected,
    append_trajectory_partial_layout, derive_trajectory_layout, derive_trajectory_request_numbers,
    last_trajectory_cell_index, trajectory_collapsible_assistant_ids,
    trajectory_collapsible_turn_ids, trajectory_record_id, trajectory_table_component,
    trajectory_timeline_component, trajectory_timeline_focus_indexes, trajectory_timeline_mode,
    trajectory_timeline_partial, trajectory_toolbar_component,
};

const SEARCH_INDEX_THROTTLE_MS: f64 = 3_000.0;

#[derive(Clone)]
struct ViewComponents {
    react: JsValue,
    toolbar: JsValue,
    timeline: JsValue,
    table: JsValue,
}

/// Returns the compiled `TrajectoryView` React component.
///
/// # Errors
///
/// Returns before React and shared UI primitives are configured.
#[wasm_bindgen(js_name = trajectoryViewComponent)]
pub fn trajectory_view_component() -> Result<JsValue, JsValue> {
    let (react, primitives) = crate::trajectory_browser_modules()?;
    if primitives.is_none() {
        return Err(js_sys::Error::new("client-ui-trajectory View requires UI primitives").into());
    }
    let components = ViewComponents {
        react,
        toolbar: trajectory_toolbar_component()?,
        timeline: trajectory_timeline_component()?,
        table: trajectory_table_component()?,
    };
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_view(&components, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

fn search_controller_face() -> Result<JsValue, JsValue> {
    let controller = Rc::new(RefCell::new(TrajectoryViewSearchController::new()));
    let face = Object::new();

    let offer_controller = controller.clone();
    let offer = Closure::wrap(
        Box::new(move |layouts: JsValue| -> Result<String, JsValue> {
            let layouts = serde_wasm_bindgen::from_value(layouts).map_err(js_error_from_display)?;
            Ok(
                match offer_controller.borrow_mut().offer(&Rc::new(layouts)) {
                    TrajectorySearchOffer::None => "none",
                    TrajectorySearchOffer::Updated => "updated",
                    TrajectorySearchOffer::Schedule => "schedule",
                }
                .to_owned(),
            )
        }) as Box<dyn FnMut(JsValue) -> Result<String, JsValue>>,
    );
    Reflect::set(&face, &JsValue::from_str("offer"), &offer.into_js_value())?;

    let fire_controller = controller.clone();
    let fire = Closure::wrap(
        Box::new(move || fire_controller.borrow_mut().fire()) as Box<dyn FnMut() -> bool>
    );
    Reflect::set(&face, &JsValue::from_str("fire"), &fire.into_js_value())?;

    let cancel_controller = controller.clone();
    let cancel = Closure::wrap(
        Box::new(move || cancel_controller.borrow_mut().cancel()) as Box<dyn FnMut()>
    );
    Reflect::set(&face, &JsValue::from_str("cancel"), &cancel.into_js_value())?;

    let search_controller = controller;
    let search = Closure::wrap(Box::new(move |query: String| -> JsValue {
        search_controller
            .borrow()
            .search(&query)
            .map_or(JsValue::NULL, |matches| {
                let set = JsSet::new(&JsValue::UNDEFINED);
                for id in matches {
                    set.add(&JsValue::from_str(&id));
                }
                set.into()
            })
    }) as Box<dyn FnMut(String) -> JsValue>);
    Reflect::set(&face, &JsValue::from_str("search"), &search.into_js_value())?;
    Ok(face.into())
}

#[allow(clippy::too_many_lines)]
fn render_view(components: &ViewComponents, props: &JsValue) -> Result<JsValue, JsValue> {
    let empty_turns = JsSet::new(&JsValue::UNDEFINED);
    let (collapsed_turns_value, set_collapsed_turns) =
        use_state(&components.react, empty_turns.as_ref())?;
    let empty_assistants = JsSet::new(&JsValue::UNDEFINED);
    let (collapsed_assistants_value, set_collapsed_assistants) =
        use_state(&components.react, empty_assistants.as_ref())?;
    let (timeline_selection, set_timeline_selection) =
        use_state(&components.react, &JsValue::NULL)?;
    let use_duration = required_function(props, "useDuration")?;
    let identity =
        Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>);
    let actual_duration = use_duration
        .call1(&JsValue::UNDEFINED, &identity.into_js_value())?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new("useDuration must return boolean"))?;
    let (actual_time, set_actual_time) = use_state(&components.react, &JsValue::FALSE)?;
    let actual_time = actual_time.as_bool().unwrap_or(false);
    let (search_query, set_search_query) = use_state(&components.react, &JsValue::from_str(""))?;
    let search_query = search_query.as_string().unwrap_or_default();
    let (search_revision, set_search_revision) =
        use_state(&components.react, &JsValue::from_f64(0.0))?;
    let search_revision = search_revision.as_f64().unwrap_or(0.0);
    let search_ref = use_ref(&components.react, &JsValue::UNDEFINED)?;
    let mut search_controller = Reflect::get(&search_ref, &JsValue::from_str("current"))?;
    if search_controller.is_undefined() {
        search_controller = search_controller_face()?;
        Reflect::set(
            &search_ref,
            &JsValue::from_str("current"),
            &search_controller,
        )?;
    }
    let search_timer_ref = use_ref(&components.react, &JsValue::NULL)?;
    let (selected_index, set_selected_index) = use_state(&components.react, &JsValue::NULL)?;
    let selected_index = selected_index.as_f64().and_then(f64_to_usize);
    let (record_selection, set_record_selection) = use_state(&components.react, &JsValue::NULL)?;
    let (record_focus, set_record_focus) = use_state(&components.react, &JsValue::NULL)?;

    let use_session = required_function(props, "useSession")?;
    let inspection_selector = session_inspection_selector();
    let inspection_value = use_session_value(&use_session, &inspection_selector)?;
    let history_loading = use_session_bool(&use_session, "openState", Some("loading"))?;
    let older_history_loading = use_session_bool(&use_session, "loadingOlder", None)?;
    let has_older_history = use_session_bool(&use_session, "hasMore", None)?;
    let inspection = trajectory_snapshot_from_js(&inspection_value)?;
    let history_base_seq = inspection
        .event_nodes
        .first()
        .and_then(|node| node.get("seq"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let request_numbers =
        derive_trajectory_request_numbers(&inspection.event_nodes, &inspection.requests)
            .map_err(js_error_from_display)?;

    let mut finalized_snapshot = inspection.clone();
    finalized_snapshot.partial = inspection.partial.as_ref().and_then(|partial| {
        Some(serde_json::json!({
            "turn": partial.get("turn")?.clone(),
            "step": partial.get("step")?.clone(),
            "blocks": [],
        }))
    });
    let finalized = derive_trajectory_layout(&finalized_snapshot);
    let last_index = last_trajectory_cell_index(&finalized);
    let finalized_rc = finalized.iter().cloned().map(Rc::new).collect::<Vec<_>>();
    let timeline_partial =
        trajectory_timeline_partial(inspection.partial.as_ref()).map_err(js_error_from_display)?;
    let timeline_turns =
        append_trajectory_partial_layout(&finalized_rc, timeline_partial.as_ref(), last_index)
            .into_iter()
            .map(|turn| turn.as_ref().clone())
            .collect::<Vec<_>>();
    let partial_search_turns =
        append_trajectory_partial_layout(&[], inspection.partial.as_ref(), last_index)
            .into_iter()
            .map(|turn| turn.as_ref().clone())
            .collect::<Vec<_>>();
    let streaming_cells = partial_search_turns
        .iter()
        .flat_map(|turn| &turn.groups)
        .flat_map(|group| &group.cells)
        .cloned()
        .collect::<Vec<_>>();

    let layouts = Rc::new(vec![finalized.clone(), partial_search_turns.clone()]);
    install_search_effect(
        &components.react,
        &search_controller,
        &search_timer_ref,
        &set_search_revision,
        search_revision,
        &layouts,
    )?;
    let search_result = call_method(
        &search_controller,
        "search",
        &[JsValue::from_str(&search_query)],
    )?;
    let search_ids = if search_result.is_null() {
        None
    } else {
        Some(string_set(&search_result)?)
    };
    let search_indexes = search_ids.as_ref().map(|ids| {
        layouts
            .iter()
            .flat_map(|turns| turns.iter())
            .flat_map(|turn| turn.groups.iter())
            .flat_map(|group| group.cells.iter())
            .filter(|cell| ids.contains(&trajectory_record_id(cell)))
            .map(|cell| cell.index)
            .collect::<BTreeSet<_>>()
    });

    let timeline_mode = trajectory_timeline_mode(actual_duration, actual_time);
    let timeline_range = if timeline_selection.is_null() {
        None
    } else {
        Some(
            serde_wasm_bindgen::from_value::<TrajectoryTimeRange>(timeline_selection.clone())
                .map_err(js_error_from_display)?,
        )
    };
    let timeline_focus = timeline_range
        .map(|range| trajectory_timeline_focus_indexes(&timeline_turns, range, timeline_mode));

    let collapsed_turns = number_set_u64(&collapsed_turns_value)?;
    let collapsed_assistants = string_set(&collapsed_assistants_value)?;
    let collapsible_turns = trajectory_collapsible_turn_ids(&timeline_turns);
    let collapsible_assistants = trajectory_collapsible_assistant_ids(&timeline_turns);
    let all_turns_collapsed = all_trajectory_folds_selected(&collapsible_turns, &collapsed_turns);
    let all_assistants_collapsed =
        all_trajectory_folds_selected(&collapsible_assistants, &collapsed_assistants);

    let load_older = required_function(props, "loadOlder")?;
    let set_actual_duration = required_function(props, "setActualDuration")?;
    let translate = required_function(props, "t")?;

    let duration_callback = {
        let set_actual_duration = set_actual_duration.clone();
        let set_timeline_selection = set_timeline_selection.clone();
        Closure::wrap(Box::new(move |value: bool| -> Result<(), JsValue> {
            set_actual_duration.call1(&JsValue::UNDEFINED, &JsValue::from_bool(value))?;
            set_timeline_selection.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            Ok(())
        }) as Box<dyn FnMut(bool) -> Result<(), JsValue>>)
        .into_js_value()
    };
    let time_callback = {
        let set_actual_time = set_actual_time.clone();
        let set_timeline_selection = set_timeline_selection.clone();
        Closure::wrap(Box::new(move |value: bool| -> Result<(), JsValue> {
            set_actual_time.call1(&JsValue::UNDEFINED, &JsValue::from_bool(value))?;
            set_timeline_selection.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            Ok(())
        }) as Box<dyn FnMut(bool) -> Result<(), JsValue>>)
        .into_js_value()
    };
    let toggle_all_turns = fold_all_callback(
        &set_collapsed_turns,
        &collapsed_turns_value,
        &collapsible_turns,
        all_turns_collapsed,
    )?;
    let toggle_all_assistants = fold_all_string_callback(
        &set_collapsed_assistants,
        &collapsed_assistants_value,
        &collapsible_assistants,
        all_assistants_collapsed,
    )?;
    let search_callback = Closure::wrap(Box::new(move |value: String| {
        set_search_query.call1(&JsValue::UNDEFINED, &JsValue::from_str(&value))
    })
        as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>)
    .into_js_value();

    let toolbar_props = object(&[
        ("actualDuration", JsValue::from_bool(actual_duration)),
        ("onActualDurationChange", duration_callback),
        ("actualTime", JsValue::from_bool(actual_time)),
        ("onActualTimeChange", time_callback),
        ("allTurnsCollapsed", JsValue::from_bool(all_turns_collapsed)),
        ("onToggleAllTurns", toggle_all_turns.into()),
        (
            "allAssistantsCollapsed",
            JsValue::from_bool(all_assistants_collapsed),
        ),
        ("onToggleAllAssistants", toggle_all_assistants.into()),
        ("searchQuery", JsValue::from_str(&search_query)),
        ("onSearchQueryChange", search_callback),
        ("t", translate.into()),
    ])?;

    let range_callback = {
        let setter = set_timeline_selection.clone();
        Closure::wrap(
            Box::new(move |range: JsValue| setter.call1(&JsValue::UNDEFINED, &range))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value()
    };
    let timeline_select = {
        let range = set_timeline_selection.clone();
        let selection = set_record_selection.clone();
        let selected = set_selected_index.clone();
        Closure::wrap(Box::new(move |index: f64| -> Result<(), JsValue> {
            range.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            selection.call1(
                &JsValue::UNDEFINED,
                &object(&[("index", JsValue::from_f64(index))])?.into(),
            )?;
            selected.call1(&JsValue::UNDEFINED, &JsValue::from_f64(index))?;
            Ok(())
        }) as Box<dyn FnMut(f64) -> Result<(), JsValue>>)
        .into_js_value()
    };
    let timeline_record_focus = Closure::wrap(Box::new(move |index: f64| {
        set_record_focus.call1(
            &JsValue::UNDEFINED,
            &object(&[("index", JsValue::from_f64(index))])?.into(),
        )
    })
        as Box<dyn FnMut(f64) -> Result<JsValue, JsValue>>)
    .into_js_value();
    let timeline_props = object(&[
        (
            "turns",
            serde_wasm_bindgen::to_value(&timeline_turns).map_err(js_error_from_display)?,
        ),
        ("mode", JsValue::from_str(timeline_mode_name(timeline_mode))),
        (
            "range",
            timeline_range.map_or(JsValue::NULL, |range| {
                serde_wasm_bindgen::to_value(&range).unwrap_or(JsValue::NULL)
            }),
        ),
        ("hasEarlierRecords", JsValue::from_bool(has_older_history)),
        ("onLoadEarlier", load_older.clone().into()),
        (
            "selectedIndex",
            selected_index.map_or(JsValue::NULL, |index| {
                JsValue::from_f64(usize_as_f64(index))
            }),
        ),
        (
            "searchMatchIndexes",
            optional_number_set_value(search_indexes.as_ref()),
        ),
        ("onRangeChange", range_callback),
        ("onRecordSelect", timeline_select),
        ("onRecordFocus", timeline_record_focus),
    ])?;

    let toggle_turn = fold_one_number_callback(&set_collapsed_turns, &collapsed_turns_value)?;
    let toggle_assistant =
        fold_one_string_callback(&set_collapsed_assistants, &collapsed_assistants_value)?;
    let record_select = {
        let setter = set_timeline_selection.clone();
        let focus = timeline_focus.clone();
        Closure::wrap(Box::new(move |index: f64| -> Result<(), JsValue> {
            if focus.as_ref().is_some_and(|focus| {
                f64_to_usize(index).is_some_and(|index| !focus.contains(&index))
            }) {
                setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            }
            Ok(())
        }) as Box<dyn FnMut(f64) -> Result<(), JsValue>>)
        .into_js_value()
    };
    let clear_selection = Closure::wrap(Box::new(move || {
        set_timeline_selection.call1(&JsValue::UNDEFINED, &JsValue::NULL)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value();
    let inspect_call_id = optional(props, "inspect")?
        .filter(|inspect| !inspect.is_null())
        .and_then(|inspect| optional_string(&inspect, "callId"))
        .map_or(JsValue::NULL, |call_id| JsValue::from_str(&call_id));
    let table_props = object(&[
        (
            "requestNumbers",
            serde_wasm_bindgen::to_value(&request_numbers).map_err(js_error_from_display)?,
        ),
        (
            "turns",
            serde_wasm_bindgen::to_value(&timeline_turns).map_err(js_error_from_display)?,
        ),
        (
            "streamingCells",
            serde_wasm_bindgen::to_value(&streaming_cells).map_err(js_error_from_display)?,
        ),
        (
            "timelineFocusIndexes",
            optional_number_set_value(timeline_focus.as_ref()),
        ),
        (
            "searchMatchIndexes",
            optional_number_set_value(search_indexes.as_ref()),
        ),
        ("onSelectedIndexChange", set_selected_index.into()),
        ("onRecordSelect", record_select),
        ("recordSelection", record_selection),
        ("recordFocus", record_focus),
        ("historyLoading", JsValue::from_bool(history_loading)),
        (
            "olderHistoryLoading",
            JsValue::from_bool(older_history_loading),
        ),
        (
            "historyStartSeq",
            JsValue::from_f64(u64_as_f64(history_base_seq)),
        ),
        ("hasOlderRecords", JsValue::from_bool(has_older_history)),
        ("onLoadOlder", load_older.into()),
        ("onClearSelection", clear_selection),
        ("collapsedTurns", collapsed_turns_value),
        ("onToggleTurn", toggle_turn.into()),
        ("collapsedAssistants", collapsed_assistants_value),
        ("onToggleAssistant", toggle_assistant.into()),
        ("inspectCallId", inspect_call_id),
        (
            "onInspectApplied",
            optional_function(props, "onInspectDone")?.map_or(JsValue::UNDEFINED, Into::into),
        ),
    ])?;

    let toolbar = element(components, &components.toolbar, &toolbar_props, &[])?;
    let timeline = element(components, &components.timeline, &timeline_props, &[])?;
    let table = element(components, &components.table, &table_props, &[])?;
    let ledger = tag(
        components,
        "div",
        Some(&class("seekdeep-trajectory-view-ledger")?),
        &[table],
    )?;
    tag(
        components,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-trajectory-view-root"),
            ),
            ("data-conversation-composer-overlay", JsValue::from_str("")),
        ])?),
        &[toolbar, timeline, ledger],
    )
}

#[allow(clippy::too_many_arguments)]
fn install_search_effect(
    react: &JsValue,
    controller: &JsValue,
    timer_ref: &JsValue,
    set_revision: &Function,
    revision: f64,
    layouts: &Rc<Vec<Vec<crate::TrajectoryTurnModel>>>,
) -> Result<(), JsValue> {
    let layouts_value =
        serde_wasm_bindgen::to_value(layouts.as_ref()).map_err(js_error_from_display)?;
    let effect_controller = controller.clone();
    let effect_timer = timer_ref.clone();
    let effect_setter = set_revision.clone();
    let effect_layouts = layouts_value.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let offer = call_method(
            &effect_controller,
            "offer",
            std::slice::from_ref(&effect_layouts),
        )?
        .as_string()
        .unwrap_or_default();
        if offer == "updated" {
            effect_setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(revision + 1.0))?;
        } else if offer == "schedule" {
            let timer_controller = effect_controller.clone();
            let timer_ref = effect_timer.clone();
            let timer_setter = effect_setter.clone();
            let callback = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                Reflect::set(&timer_ref, &JsValue::from_str("current"), &JsValue::NULL)?;
                if call_method(&timer_controller, "fire", &[])?.as_bool() == Some(true) {
                    timer_setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(revision + 1.0))?;
                }
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let global = js_sys::global();
            let timer = required_function(&global, "setTimeout")?.call2(
                &global,
                &callback.into_js_value(),
                &JsValue::from_f64(SEARCH_INDEX_THROTTLE_MS),
            )?;
            Reflect::set(&effect_timer, &JsValue::from_str("current"), &timer)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(react, &effect.into_js_value(), &Array::of1(&layouts_value))?;

    let cleanup_controller = controller.clone();
    let cleanup_timer = timer_ref.clone();
    let cleanup_effect = Closure::wrap(Box::new(move || -> JsValue {
        let cleanup_controller = cleanup_controller.clone();
        let cleanup_timer = cleanup_timer.clone();
        Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let timer = Reflect::get(&cleanup_timer, &JsValue::from_str("current"))?;
            if !timer.is_null() && !timer.is_undefined() {
                let global = js_sys::global();
                required_function(&global, "clearTimeout")?.call1(&global, &timer)?;
                Reflect::set(
                    &cleanup_timer,
                    &JsValue::from_str("current"),
                    &JsValue::NULL,
                )?;
            }
            call_method(&cleanup_controller, "cancel", &[]).map(|_| ())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(react, &cleanup_effect.into_js_value(), &Array::new())
}

fn session_inspection_selector() -> JsValue {
    Closure::wrap(
        Box::new(move |snapshot: JsValue| -> Result<JsValue, JsValue> {
            let views = required(&snapshot, "views", "session snapshot")?;
            let trajectory = call_method(&views, "get", &[JsValue::from_str("trajectory")])?;
            if trajectory.is_undefined() {
                empty_inspection()
            } else {
                Ok(trajectory)
            }
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

fn use_session_bool(hook: &Function, key: &str, equals: Option<&str>) -> Result<bool, JsValue> {
    let key = key.to_owned();
    let equals = equals.map(ToOwned::to_owned);
    let selector = Closure::wrap(Box::new(move |snapshot: JsValue| -> Result<bool, JsValue> {
        let value = required(&snapshot, &key, "session snapshot")?;
        Ok(equals.as_ref().map_or_else(
            || value.as_bool().unwrap_or(false),
            |expected| value.as_string().as_deref() == Some(expected),
        ))
    }) as Box<dyn FnMut(JsValue) -> Result<bool, JsValue>>);
    hook.call1(&JsValue::UNDEFINED, &selector.into_js_value())?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new("useSession selector must return boolean").into())
}

fn use_session_value(hook: &Function, selector: &JsValue) -> Result<JsValue, JsValue> {
    hook.call1(&JsValue::UNDEFINED, selector)
}

fn trajectory_snapshot_from_js(value: &JsValue) -> Result<TrajectorySnapshot, JsValue> {
    Ok(TrajectorySnapshot {
        event_nodes: deserialize_property(value, "eventNodes")?,
        event_locations: location_map(&required(value, "eventLocations", "trajectory snapshot")?)?,
        requests: deserialize_property(value, "requests")?,
        call_schemas: value_map(&required(value, "callSchemas", "trajectory snapshot")?)?,
        partial: optional(value, "partial")?
            .filter(|partial| !partial.is_null())
            .map(|partial| serde_wasm_bindgen::from_value(partial).map_err(js_error_from_display))
            .transpose()?,
        running_calls: deserialize_property(value, "runningCalls")?,
    })
}

fn location_map(
    value: &JsValue,
) -> Result<IndexMap<TrajectorySequence, TrajectoryLocation>, JsValue> {
    let mut output = IndexMap::new();
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("eventLocations must be iterable"))?;
    for entry in iterator {
        let pair = Array::from(&entry?);
        let sequence = pair
            .get(0)
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| js_sys::Error::new("event location sequence must be finite"))?;
        let location =
            serde_wasm_bindgen::from_value(pair.get(1)).map_err(js_error_from_display)?;
        output.insert(TrajectorySequence::new(sequence), location);
    }
    Ok(output)
}

fn value_map(value: &JsValue) -> Result<IndexMap<String, serde_json::Value>, JsValue> {
    let mut output = IndexMap::new();
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("callSchemas must be iterable"))?;
    for entry in iterator {
        let pair = Array::from(&entry?);
        let key = pair
            .get(0)
            .as_string()
            .ok_or_else(|| js_sys::Error::new("call schema key must be a string"))?;
        let schema = serde_wasm_bindgen::from_value(pair.get(1)).map_err(js_error_from_display)?;
        output.insert(key, schema);
    }
    Ok(output)
}

fn empty_inspection() -> Result<JsValue, JsValue> {
    Ok(object(&[
        ("eventNodes", Array::new().into()),
        ("eventLocations", JsMap::new().into()),
        ("requests", Array::new().into()),
        ("callSchemas", JsMap::new().into()),
        ("partial", JsValue::NULL),
        ("runningCalls", Array::new().into()),
    ])?
    .into())
}

fn fold_one_number_callback(setter: &Function, current: &JsValue) -> Result<Function, JsValue> {
    let setter = setter.clone();
    let current = current.clone();
    let callback = Closure::wrap(Box::new(move |value: f64| -> Result<JsValue, JsValue> {
        let next = JsSet::new(&current);
        if next.has(&JsValue::from_f64(value)) {
            next.delete(&JsValue::from_f64(value));
        } else {
            next.add(&JsValue::from_f64(value));
        }
        setter.call1(&JsValue::UNDEFINED, next.as_ref())
    }) as Box<dyn FnMut(f64) -> Result<JsValue, JsValue>>);
    callback.into_js_value().dyn_into()
}

fn fold_one_string_callback(setter: &Function, current: &JsValue) -> Result<Function, JsValue> {
    let setter = setter.clone();
    let current = current.clone();
    let callback = Closure::wrap(Box::new(move |value: String| -> Result<JsValue, JsValue> {
        let next = JsSet::new(&current);
        let value = JsValue::from_str(&value);
        if next.has(&value) {
            next.delete(&value);
        } else {
            next.add(&value);
        }
        setter.call1(&JsValue::UNDEFINED, next.as_ref())
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    callback.into_js_value().dyn_into()
}

fn fold_all_callback(
    setter: &Function,
    current: &JsValue,
    available: &[u64],
    all_selected: bool,
) -> Result<Function, JsValue> {
    let setter = setter.clone();
    let current = current.clone();
    let available = available.to_vec();
    let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let next = JsSet::new(&current);
        for value in &available {
            if all_selected {
                next.delete(&JsValue::from_f64(u64_as_f64(*value)));
            } else {
                next.add(&JsValue::from_f64(u64_as_f64(*value)));
            }
        }
        setter.call1(&JsValue::UNDEFINED, next.as_ref())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    callback.into_js_value().dyn_into()
}

fn fold_all_string_callback(
    setter: &Function,
    current: &JsValue,
    available: &[String],
    all_selected: bool,
) -> Result<Function, JsValue> {
    let setter = setter.clone();
    let current = current.clone();
    let available = available.to_vec();
    let callback = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let next = JsSet::new(&current);
        for value in &available {
            if all_selected {
                next.delete(&JsValue::from_str(value));
            } else {
                next.add(&JsValue::from_str(value));
            }
        }
        setter.call1(&JsValue::UNDEFINED, next.as_ref())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    callback.into_js_value().dyn_into()
}

fn number_set_u64(value: &JsValue) -> Result<BTreeSet<u64>, JsValue> {
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("collapsedTurns must be iterable"))?;
    iterator
        .map(|entry| {
            entry.and_then(|entry| {
                entry
                    .as_f64()
                    .and_then(f64_to_u64)
                    .ok_or_else(|| js_sys::Error::new("turn id must be a u64").into())
            })
        })
        .collect()
}

fn string_set(value: &JsValue) -> Result<BTreeSet<String>, JsValue> {
    let iterator = js_sys::try_iter(value)?
        .ok_or_else(|| js_sys::Error::new("collapsedAssistants must be iterable"))?;
    iterator
        .map(|entry| {
            entry.and_then(|entry| {
                entry
                    .as_string()
                    .ok_or_else(|| js_sys::Error::new("Assistant id must be a string").into())
            })
        })
        .collect()
}

fn optional_number_set_value<T>(values: Option<&BTreeSet<T>>) -> JsValue
where
    T: Copy + IntoNumber,
{
    values.map_or(JsValue::NULL, |values| {
        let set = JsSet::new(&JsValue::UNDEFINED);
        for value in values {
            set.add(&JsValue::from_f64((*value).into_number()));
        }
        set.into()
    })
}

trait IntoNumber {
    fn into_number(self) -> f64;
}

impl IntoNumber for usize {
    fn into_number(self) -> f64 {
        usize_as_f64(self)
    }
}

fn timeline_mode_name(mode: crate::TrajectoryTimelineMode) -> &'static str {
    match mode {
        crate::TrajectoryTimelineMode::Sequence => "sequence",
        crate::TrajectoryTimelineMode::Duration => "duration",
        crate::TrajectoryTimelineMode::Time => "time",
        crate::TrajectoryTimelineMode::Actual => "actual",
    }
}

fn deserialize_property<T: serde::de::DeserializeOwned>(
    value: &JsValue,
    key: &str,
) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(required(value, key, "trajectory snapshot")?)
        .map_err(js_error_from_display)
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef")?.call1(react, initial)
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn element(
    components: &ViewComponents,
    kind: &JsValue,
    props: &Object,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props);
    for child in children {
        arguments.push(child);
    }
    required_function(&components.react, "createElement")?.apply(&components.react, &arguments)
}

fn tag(
    components: &ViewComponents,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let props = props.cloned().unwrap_or_else(Object::new);
    element(components, &JsValue::from_str(name), &props, children)
}

fn class(value: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(value))])
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry)?;
    }
    Ok(value)
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

fn optional_string(value: &JsValue, key: &str) -> Option<String> {
    Reflect::get(value, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .and_then(|value| value.as_string())
}

fn required_function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required(value, key, "object")?.dyn_into()
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    optional(value, key)?
        .filter(|value| !value.is_null())
        .map(JsCast::dyn_into)
        .transpose()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments = arguments.iter().collect::<Array>();
    method.apply(value, &arguments)
}

fn f64_to_usize(value: f64) -> Option<usize> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as usize)
}

fn f64_to_u64(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as u64)
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
