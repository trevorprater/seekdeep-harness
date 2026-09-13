//! Rust/WASM `SubagentCatalogAction` rendering and browser lifecycle.

use std::collections::BTreeSet;

use js_sys::{Array, Function, Object, Reflect, Set};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use super::{
    BrowserModules, call_method, component as react_component, f64_as_i128, fragment, object,
    optional, optional_string, required, required_bool, required_function, required_string, tag,
    translated, translated_values, use_effect, use_ref, use_state, usize_as_f64,
};
use crate::{
    DurationFormat, DurationValue, SubagentActiveTiming, SubagentDescendantSummary,
    SubagentListSummary, SubagentTiming, TokenUsage, activity_duration, format_duration,
    format_exact_duration, format_tokens, index_subagent_descendants, token_total,
};

#[derive(Clone)]
struct CatalogRender {
    modules: BrowserModules,
    catalogs: JsValue,
    summaries: JsValue,
    expanded: JsValue,
    now: f64,
    open_child: Function,
    refresh: Function,
    set_catalog_open: Function,
    observed: JsValue,
    set_expanded: Function,
    set_open: Function,
    translate: Function,
}

pub(crate) fn component(modules: &BrowserModules) -> JsValue {
    let modules = modules.clone();
    Closure::wrap(
        Box::new(move |props: JsValue| render_action(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
}

#[allow(clippy::too_many_lines)]
fn render_action(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let session_id = required_string(props, "sessionId", "SubagentCatalogAction")?;
    let use_sessions = required_function(props, "useSessions", "SubagentCatalogAction")?;
    let open_child = required_function(props, "openChild", "SubagentCatalogAction")?;
    let refresh = required_function(props, "refresh", "SubagentCatalogAction")?;
    let set_catalog_open = required_function(props, "setCatalogOpen", "SubagentCatalogAction")?;
    let translate = required_function(props, "t", "SubagentCatalogAction")?;

    let select_catalogs = Closure::wrap(Box::new(move |state: JsValue| {
        required(&state, "subagentsByParent", "Session list state")
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let catalogs = use_sessions.call1(&JsValue::UNDEFINED, &select_catalogs.into_js_value())?;
    let select_summaries =
        Closure::wrap(
            Box::new(move |state: JsValue| required(&state, "byId", "Session list state"))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
    let summaries = use_sessions.call1(&JsValue::UNDEFINED, &select_summaries.into_js_value())?;

    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let clock = Closure::wrap(Box::new(js_sys::Date::now) as Box<dyn FnMut() -> f64>);
    let (now, set_now) = use_state(&modules.react, &clock.into_js_value())?;
    let initial_expanded: JsValue = Set::new(&JsValue::UNDEFINED).into();
    let (expanded, set_expanded) = use_state(&modules.react, &initial_expanded)?;
    let root_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let trigger_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let observed_ref = use_ref(&modules.react, &Set::new(&JsValue::UNDEFINED).into())?;
    let setter_ref = use_ref(&modules.react, &set_catalog_open.clone().into())?;
    Reflect::set(
        &setter_ref,
        &JsValue::from_str("current"),
        &set_catalog_open,
    )?;

    let open = open.as_bool().unwrap_or(false);
    let now = now
        .as_f64()
        .ok_or_else(|| js_sys::Error::new("SubagentCatalogAction clock must be numeric"))?;
    let expanded = expanded.dyn_into::<Set>()?;
    let observed = required(&observed_ref, "current", "catalog observation ref")?;

    let catalog = Reflect::get(&catalogs, &JsValue::from_str(&session_id))?;
    let catalog_entries = if catalog.is_undefined() {
        Array::new()
    } else {
        Array::from(&required(&catalog, "entries", "subagent catalog")?)
    };
    let healthy = catalog_entries
        .iter()
        .filter(|entry| {
            Reflect::get(entry, &JsValue::from_str("kind"))
                .ok()
                .and_then(|kind| kind.as_string())
                .as_deref()
                == Some("child")
        })
        .count();
    let descendants = descendant_summary(&summaries, &SessionId::new(&session_id))?;
    let descendant_count = healthy.max(descendants.count);
    let summary_backed_loading = descendants.count > 0
        && (catalog.is_undefined()
            || (required_string(&catalog, "state", "subagent catalog")? == "ready"
                && catalog_entries.length() == 0));
    let state = if summary_backed_loading || catalog.is_undefined() {
        "loading".to_owned()
    } else {
        required_string(&catalog, "state", "subagent catalog")?
    };
    let presented_entries = if summary_backed_loading {
        Array::new()
    } else {
        catalog_entries
    };
    let visible = !catalog.is_undefined() || summary_backed_loading;
    let visible =
        visible && (state == "error" || presented_entries.length() > 0 || descendant_count > 0);

    install_outside_effect(
        &modules.react,
        open,
        &root_ref,
        &set_open,
        &set_expanded,
        &observed,
        &set_catalog_open,
    )?;
    install_timer_effect(&modules.react, open, descendants.running_count, &set_now)?;
    install_unmount_effect(&modules.react, &observed_ref, &setter_ref)?;
    install_visibility_effect(
        &modules.react,
        visible,
        open,
        &set_open,
        &set_expanded,
        &observed,
        &set_catalog_open,
    )?;

    if !visible {
        return Ok(JsValue::NULL);
    }

    let render = CatalogRender {
        modules: modules.clone(),
        catalogs,
        summaries,
        expanded: expanded.into(),
        now,
        open_child,
        refresh,
        set_catalog_open,
        observed,
        set_expanded,
        set_open,
        translate,
    };
    let trigger = render_trigger(
        &render,
        &session_id,
        open,
        descendant_count,
        descendants.running_count,
        &trigger_ref,
        &set_now,
    )?;
    let mut children = vec![trigger];
    if open {
        let presented = if summary_backed_loading {
            synthetic_catalog("loading", &presented_entries)?
        } else {
            catalog
        };
        children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-subagent-catalog-menu"),
                ),
                ("role", JsValue::from_str("tree")),
                ("aria-label", translated(&render.translate, "tree.aria")?),
            ])?),
            &[render_catalog_rows(&render, &session_id, &presented, 1)?],
        )?);
    }
    let navigate_render = render.clone();
    let navigate_root = root_ref.clone();
    let navigate_trigger = trigger_ref.clone();
    let navigate = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        navigate_tree(&event, &navigate_root, &navigate_trigger, &navigate_render)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-root"),
            ),
            ("ref", root_ref),
            ("onKeyDown", navigate.into_js_value()),
        ])?),
        &children,
    )
}

fn synthetic_catalog(state: &str, entries: &Array) -> Result<JsValue, JsValue> {
    object(&[
        ("entries", entries.clone().into()),
        ("state", JsValue::from_str(state)),
        ("error", JsValue::NULL),
        ("parentAvailable", JsValue::FALSE),
    ])
    .map(Into::into)
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn render_trigger(
    render: &CatalogRender,
    session_id: &str,
    open: bool,
    count: usize,
    running_count: usize,
    trigger_ref: &JsValue,
    set_now: &Function,
) -> Result<JsValue, JsValue> {
    let total_key = if count == 1 {
        "count.total.one"
    } else {
        "count.total.other"
    };
    let running_key = if running_count == 1 {
        "count.running.one"
    } else {
        "count.running.other"
    };
    let total = translated_values(
        &render.translate,
        total_key,
        &[("count", JsValue::from_f64(usize_as_f64(count)))],
    )?;
    let aria = translated_values(
        &render.translate,
        if running_count > 0 {
            running_key
        } else {
            total_key
        },
        &[(
            "count",
            JsValue::from_f64(usize_as_f64(if running_count > 0 {
                running_count
            } else {
                count
            })),
        )],
    )?;
    let mut activity = Vec::new();
    if running_count > 0 {
        activity.push(react_component(
            &render.modules.react,
            &render.modules.state_dot,
            Some(&object(&[("state", JsValue::from_str("ongoing"))])?),
            &[],
        )?);
    }
    let activity = tag(
        &render.modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-subagent-catalog-activitySlot"),
        )])?),
        &activity,
    )?;
    let count_node = tag(
        &render.modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-subagent-catalog-count"),
        )])?),
        &[total],
    )?;
    let chevron = react_component(
        &render.modules.react,
        &render.modules.chevron_down,
        Some(&object(&[(
            "className",
            if open {
                JsValue::from_str("seekdeep-subagent-catalog-triggerOpen")
            } else {
                JsValue::UNDEFINED
            },
        )])?),
        &[],
    )?;

    let click_render = render.clone();
    let click_session = session_id.to_owned();
    let click_now = set_now.clone();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        change_open(&click_render, &click_session, !open, &click_now, None)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let key_render = render.clone();
    let key_session = session_id.to_owned();
    let key_now = set_now.clone();
    let key_root = trigger_ref.clone();
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if required_string(&event, "key", "keyboard event")? != "ArrowDown" {
            return Ok(());
        }
        call_method(&event, "preventDefault", &[])?;
        if !open {
            change_open(&key_render, &key_session, true, &key_now, None)?;
        }
        let root = key_root.clone();
        queue_microtask(move || focus_at(&root, 0))
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    tag(
        &render.modules.react,
        "button",
        Some(&object(&[
            ("ref", trigger_ref.clone()),
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-trigger"),
            ),
            ("aria-haspopup", JsValue::from_str("tree")),
            ("aria-expanded", JsValue::from_bool(open)),
            ("aria-label", aria),
            ("onClick", click.into_js_value()),
            ("onKeyDown", keydown.into_js_value()),
        ])?),
        &[activity, count_node, chevron],
    )
}

fn change_open(
    render: &CatalogRender,
    session_id: &str,
    next: bool,
    set_now: &Function,
    restore_focus: Option<JsValue>,
) -> Result<(), JsValue> {
    render
        .set_open
        .call1(&JsValue::UNDEFINED, &JsValue::from_bool(next))?;
    if next {
        set_now.call1(&JsValue::UNDEFINED, &JsValue::from_f64(js_sys::Date::now()))?;
        observe_catalog(render, session_id, true)?;
    } else {
        close_all_catalogs(render)?;
    }
    if let Some(trigger_ref) = restore_focus {
        queue_microtask(move || {
            let trigger = Reflect::get(&trigger_ref, &JsValue::from_str("current"))?;
            if !trigger.is_null() {
                call_method(&trigger, "focus", &[])?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn render_catalog_rows(
    render: &CatalogRender,
    parent_session_id: &str,
    catalog: &JsValue,
    level: usize,
) -> Result<JsValue, JsValue> {
    let entries = Array::from(&required(catalog, "entries", "subagent catalog")?);
    let state = required_string(catalog, "state", "subagent catalog")?;
    let mut rows = Vec::new();
    if state == "loading" && entries.length() == 0 {
        rows.extend(render_loading_rows(render, parent_session_id, level)?);
    }
    if state == "error" {
        rows.push(render_error(render, parent_session_id, catalog)?);
    }
    let reserve_disclosure = entries.iter().any(|entry| {
        Reflect::get(&entry, &JsValue::from_str("kind"))
            .ok()
            .and_then(|value| value.as_string())
            .as_deref()
            == Some("child")
            && Reflect::get(&entry, &JsValue::from_str("hasChildren"))
                .ok()
                .and_then(|value| value.as_bool())
                == Some(true)
    });
    for entry in entries.iter() {
        match required_string(&entry, "kind", "subagent catalog entry")?.as_str() {
            "diagnostic" => rows.push(render_diagnostic(
                render,
                &entry,
                level,
                reserve_disclosure,
            )?),
            "child" => rows.push(render_child(
                render,
                parent_session_id,
                &entry,
                level,
                reserve_disclosure,
            )?),
            other => {
                return Err(js_sys::Error::new(&format!(
                    "unknown subagent catalog entry kind {other:?}"
                ))
                .into());
            }
        }
    }
    fragment(&render.modules.react, &rows)
}

fn render_loading_rows(
    render: &CatalogRender,
    parent_session_id: &str,
    level: usize,
) -> Result<Vec<JsValue>, JsValue> {
    let summaries = Object::values(&Object::from(render.summaries.clone()));
    let mut rows = Vec::new();
    for summary in summaries.iter() {
        if optional_string(&summary, "origin")?.as_deref() != Some("subagent")
            || optional_string(&summary, "parentId")?.as_deref() != Some(parent_session_id)
        {
            continue;
        }
        let id = required_string(&summary, "id", "Session summary")?;
        let state = if required_bool(&summary, "running", "Session summary")? {
            "ongoing"
        } else {
            "done"
        };
        let row = tag(
            &render.modules.react,
            "div",
            Some(&object(&[
                ("key", JsValue::from_str(&id)),
                (
                    "className",
                    JsValue::from_str("seekdeep-subagent-catalog-node"),
                ),
            ])?),
            &[tag(
                &render.modules.react,
                "div",
                Some(&object(&[
                    ("role", JsValue::from_str("treeitem")),
                    ("aria-disabled", JsValue::TRUE),
                    ("aria-level", JsValue::from_f64(usize_as_f64(level))),
                    ("aria-label", translated(&render.translate, "loading.aria")?),
                    (
                        "className",
                        JsValue::from_str(
                            "seekdeep-subagent-catalog-row seekdeep-subagent-catalog-disabled seekdeep-subagent-catalog-loadingRow",
                        ),
                    ),
                ])?),
                &[
                    tag(
                        &render.modules.react,
                        "span",
                        Some(&object(&[(
                            "className",
                            JsValue::from_str("seekdeep-subagent-catalog-disclosureSpace"),
                        )])?),
                        &[],
                    )?,
                    react_component(
                        &render.modules.react,
                        &render.modules.state_dot,
                        Some(&object(&[("state", JsValue::from_str(state))])?),
                        &[],
                    )?,
                    tag(
                        &render.modules.react,
                        "span",
                        Some(&object(&[(
                            "className",
                            JsValue::from_str("seekdeep-subagent-catalog-content"),
                        )])?),
                        &[tag(
                            &render.modules.react,
                            "span",
                            Some(&object(&[(
                                "className",
                                JsValue::from_str("seekdeep-subagent-catalog-label"),
                            )])?),
                            &[translated(&render.translate, "loading.label")?],
                        )?],
                    )?,
                ],
            )?],
        )?;
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(tag(
            &render.modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-notice"),
            )])?),
            &[translated(&render.translate, "loading.label")?],
        )?);
    }
    Ok(rows)
}

fn render_error(
    render: &CatalogRender,
    parent_session_id: &str,
    catalog: &JsValue,
) -> Result<JsValue, JsValue> {
    let error = optional(catalog, "error")?;
    let message = error
        .as_ref()
        .map(|error| required_string(error, "message", "subagent catalog error"))
        .transpose()?
        .map_or_else(
            || translated(&render.translate, "load.error"),
            |message| Ok(JsValue::from_str(&message)),
        )?;
    let refresh = render.refresh.clone();
    let parent = parent_session_id.to_owned();
    let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        refresh
            .call1(&JsValue::UNDEFINED, &JsValue::from_str(&parent))
            .map(|_| ())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    tag(
        &render.modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-subagent-catalog-error"),
        )])?),
        &[
            tag(&render.modules.react, "span", None, &[message])?,
            tag(
                &render.modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-subagent-catalog-refresh"),
                    ),
                    ("onClick", click.into_js_value()),
                ])?),
                &[
                    react_component(&render.modules.react, &render.modules.refresh, None, &[])?,
                    translated(&render.translate, "retry")?,
                ],
            )?,
        ],
    )
}

fn render_diagnostic(
    render: &CatalogRender,
    entry: &JsValue,
    level: usize,
    reserve_disclosure: bool,
) -> Result<JsValue, JsValue> {
    let id = required_string(entry, "id", "subagent diagnostic")?;
    let key = match required_string(entry, "reason", "subagent diagnostic")?.as_str() {
        "corrupt" => "diagnostic.corrupt",
        "unsupported" => "diagnostic.unsupported",
        "unavailable" => "diagnostic.unavailable",
        other => {
            return Err(js_sys::Error::new(&format!(
                "unknown subagent diagnostic reason {other:?}"
            ))
            .into());
        }
    };
    let reason = translated(&render.translate, key)?;
    let reason_text = reason
        .as_string()
        .ok_or_else(|| js_sys::Error::new("subagent diagnostic must translate to a string"))?;
    let mut row = Vec::new();
    if reserve_disclosure {
        row.push(tag(
            &render.modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-disclosureSpace"),
            )])?),
            &[],
        )?);
    }
    row.push(react_component(
        &render.modules.react,
        &render.modules.state_dot,
        Some(&object(&[("state", JsValue::from_str("error"))])?),
        &[],
    )?);
    row.push(render_content(render, &id, &reason_text)?);
    tag(
        &render.modules.react,
        "div",
        Some(&object(&[
            ("key", JsValue::from_str(&id)),
            (
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-node"),
            ),
        ])?),
        &[tag(
            &render.modules.react,
            "div",
            Some(&object(&[
                ("role", JsValue::from_str("treeitem")),
                ("aria-disabled", JsValue::TRUE),
                ("aria-level", JsValue::from_f64(usize_as_f64(level))),
                (
                    "aria-label",
                    JsValue::from_str(&format!("{id} {reason_text}")),
                ),
                (
                    "className",
                    JsValue::from_str(
                        "seekdeep-subagent-catalog-row seekdeep-subagent-catalog-disabled",
                    ),
                ),
                ("title", reason),
            ])?),
            &row,
        )?],
    )
}

#[allow(clippy::too_many_lines)]
fn render_child(
    render: &CatalogRender,
    parent_session_id: &str,
    entry: &JsValue,
    level: usize,
    reserve_disclosure: bool,
) -> Result<JsValue, JsValue> {
    let id = required_string(entry, "id", "subagent child")?;
    let mode = required_string(entry, "mode", "subagent child")?;
    let activity = required_string(entry, "activity", "subagent child")?;
    let has_children = required_bool(entry, "hasChildren", "subagent child")?;
    let expanded = set_has(&render.expanded, &id)?;
    let child_catalog = Reflect::get(&render.catalogs, &JsValue::from_str(&id))?;
    let child_loading = child_catalog.is_undefined()
        || (required_string(&child_catalog, "state", "subagent child catalog")? == "loading"
            && Array::from(&required(
                &child_catalog,
                "entries",
                "subagent child catalog",
            )?)
            .length()
                == 0);
    let summary = Reflect::get(&render.summaries, &JsValue::from_str(&id))?;
    let label = optional_string(entry, "label")?.unwrap_or_else(|| id.clone());
    let mode_copy = translated(
        &render.translate,
        if mode == "one-shot" {
            "mode.oneShot"
        } else {
            "mode.continuable"
        },
    )?
    .as_string()
    .ok_or_else(|| js_sys::Error::new("subagent mode must translate to a string"))?;
    let activity_copy = translated(
        &render.translate,
        if activity == "running" {
            "activity.running"
        } else {
            "activity.inactive"
        },
    )?
    .as_string()
    .ok_or_else(|| js_sys::Error::new("subagent activity must translate to a string"))?;
    let mut secondary = Vec::new();
    if !summary.is_undefined()
        && let Some(title) = optional_string(&summary, "title")?
    {
        secondary.push(title);
    }
    secondary.push(mode_copy);
    secondary.push(activity_copy);
    let secondary = secondary.join(" · ");
    let total_tokens = if summary.is_undefined() {
        None
    } else {
        parse_token_usage(&summary)?.and_then(|usage| token_total(Some(usage)))
    };
    let duration_ms = if summary.is_undefined() {
        None
    } else {
        activity_duration(
            parse_timing(&summary)?,
            activity == "running",
            f64_as_i128(render.now),
        )
    };
    let token_metric = total_tokens.map(|value| format!("{} tok", format_tokens(value)));
    let duration_metric = duration_ms
        .map(|value| {
            Ok::<_, JsValue>((
                render_duration(&format_duration(value), &render.translate)?,
                render_duration(&format_exact_duration(value), &render.translate)?,
            ))
        })
        .transpose()?;
    let metrics = [
        token_metric.as_deref(),
        duration_metric.as_ref().map(|(_, exact)| exact.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");

    let open_render = render.clone();
    let open_parent = parent_session_id.to_owned();
    let open_id = id.clone();
    let open_mode = mode.clone();
    let open = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let address = object(&[
            ("parentSessionId", JsValue::from_str(&open_parent)),
            ("childSessionId", JsValue::from_str(&open_id)),
            ("mode", JsValue::from_str(&open_mode)),
        ])?;
        open_render
            .open_child
            .call1(&JsValue::UNDEFINED, &address.into())?;
        open_render
            .set_open
            .call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        close_all_catalogs(&open_render)
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();

    let key_open = open.clone();
    let key_render = render.clone();
    let key_id = id.clone();
    let keydown = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required_string(&event, "key", "keyboard event")?;
        if matches!(key.as_str(), "Enter" | " ") {
            call_method(&event, "preventDefault", &[])?;
            call_method(&event, "stopPropagation", &[])?;
            key_open
                .clone()
                .dyn_into::<Function>()?
                .call0(&JsValue::UNDEFINED)?;
        } else if (key == "ArrowRight" && has_children && !expanded)
            || (key == "ArrowLeft" && expanded)
        {
            call_method(&event, "preventDefault", &[])?;
            call_method(&event, "stopPropagation", &[])?;
            toggle_branch(&key_render, &key_id, expanded)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);

    let mut row_children = Vec::new();
    if has_children {
        row_children.push(render_disclosure(render, &id, &label, expanded)?);
    } else if reserve_disclosure {
        row_children.push(tag(
            &render.modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-disclosureSpace"),
            )])?),
            &[],
        )?);
    }
    let mut clickarea = vec![
        react_component(
            &render.modules.react,
            &render.modules.state_dot,
            Some(&object(&[(
                "state",
                JsValue::from_str(if activity == "running" {
                    "ongoing"
                } else {
                    "done"
                }),
            )])?),
            &[],
        )?,
        render_content(render, &label, &secondary)?,
    ];
    if token_metric.is_some() || duration_metric.is_some() {
        let mut metric_nodes = Vec::new();
        if let Some(token) = token_metric {
            metric_nodes.push(tag(
                &render.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-subagent-catalog-metricToken"),
                )])?),
                &[JsValue::from_str(&token)],
            )?);
        }
        if let Some((compact, exact)) = duration_metric {
            metric_nodes.push(tag(
                &render.modules.react,
                "span",
                Some(&object(&[
                    (
                        "className",
                        JsValue::from_str("seekdeep-subagent-catalog-metricDuration"),
                    ),
                    (
                        "title",
                        translated_values(
                            &render.translate,
                            "duration.exactTitle",
                            &[("duration", JsValue::from_str(&exact))],
                        )?,
                    ),
                ])?),
                &[JsValue::from_str(&compact)],
            )?);
        }
        clickarea.push(tag(
            &render.modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-metrics"),
            )])?),
            &metric_nodes,
        )?);
    }
    row_children.push(tag(
        &render.modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-subagent-catalog-clickarea"),
        )])?),
        &clickarea,
    )?);
    let mut node_children = vec![tag(
        &render.modules.react,
        "div",
        Some(&object(&[
            ("role", JsValue::from_str("treeitem")),
            ("tabIndex", JsValue::from_f64(0.0)),
            ("aria-level", JsValue::from_f64(usize_as_f64(level))),
            (
                "aria-label",
                JsValue::from_str(
                    &[label.as_str(), secondary.as_str(), metrics.as_str()]
                        .into_iter()
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ),
            (
                "aria-expanded",
                if has_children {
                    JsValue::from_bool(expanded)
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-row"),
            ),
            ("onClick", open),
            ("onKeyDown", keydown.into_js_value()),
        ])?),
        &row_children,
    )?];
    if expanded && has_children {
        let body = if child_catalog.is_undefined() {
            fragment(
                &render.modules.react,
                &render_loading_rows(render, &id, level + 1)?,
            )?
        } else {
            render_catalog_rows(render, &id, &child_catalog, level + 1)?
        };
        node_children.push(tag(
            &render.modules.react,
            "div",
            Some(&object(&[
                ("role", JsValue::from_str("group")),
                (
                    "className",
                    JsValue::from_str("seekdeep-subagent-catalog-children"),
                ),
                (
                    "aria-busy",
                    if child_loading {
                        JsValue::TRUE
                    } else {
                        JsValue::UNDEFINED
                    },
                ),
            ])?),
            &[body],
        )?);
    }
    tag(
        &render.modules.react,
        "div",
        Some(&object(&[
            ("key", JsValue::from_str(&id)),
            (
                "className",
                JsValue::from_str("seekdeep-subagent-catalog-node"),
            ),
        ])?),
        &node_children,
    )
}

fn render_content(render: &CatalogRender, label: &str, summary: &str) -> Result<JsValue, JsValue> {
    tag(
        &render.modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str("seekdeep-subagent-catalog-content"),
        )])?),
        &[
            tag(
                &render.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-subagent-catalog-label"),
                )])?),
                &[JsValue::from_str(label)],
            )?,
            tag(
                &render.modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str("seekdeep-subagent-catalog-summary"),
                )])?),
                &[JsValue::from_str(summary)],
            )?,
        ],
    )
}

fn render_disclosure(
    render: &CatalogRender,
    id: &str,
    label: &str,
    expanded: bool,
) -> Result<JsValue, JsValue> {
    let toggle_render = render.clone();
    let toggle_id = id.to_owned();
    let toggle = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call_method(&event, "preventDefault", &[])?;
        call_method(&event, "stopPropagation", &[])?;
        toggle_branch(&toggle_render, &toggle_id, expanded)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let aria = translated_values(
        &render.translate,
        if expanded {
            "branch.collapse"
        } else {
            "branch.expand"
        },
        &[("label", JsValue::from_str(label))],
    )?;
    tag(
        &render.modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            ("tabIndex", JsValue::from_f64(-1.0)),
            (
                "className",
                JsValue::from_str(if expanded {
                    "seekdeep-subagent-catalog-disclosure seekdeep-subagent-catalog-disclosureOpen"
                } else {
                    "seekdeep-subagent-catalog-disclosure"
                }),
            ),
            ("aria-label", aria),
            ("onClick", toggle.into_js_value()),
        ])?),
        &[react_component(
            &render.modules.react,
            &render.modules.chevron_right,
            None,
            &[],
        )?],
    )
}

fn toggle_branch(render: &CatalogRender, id: &str, expanded: bool) -> Result<(), JsValue> {
    if expanded {
        let mut closing = BTreeSet::new();
        collect_closing(render, id, &mut closing)?;
        for closing_id in &closing {
            observe_catalog(render, closing_id, false)?;
        }
        let closing = closing.into_iter().collect::<Vec<_>>();
        let setter = render.set_expanded.clone();
        let update = Closure::wrap(
            Box::new(move |current: JsValue| -> Result<JsValue, JsValue> {
                let next = clone_set(&current)?;
                for id in &closing {
                    next.delete(&JsValue::from_str(id));
                }
                Ok(next.into())
            }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
        setter.call1(&JsValue::UNDEFINED, &update.into_js_value())?;
    } else {
        let added = id.to_owned();
        let update = Closure::wrap(
            Box::new(move |current: JsValue| -> Result<JsValue, JsValue> {
                let next = clone_set(&current)?;
                next.add(&JsValue::from_str(&added));
                Ok(next.into())
            }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
        render
            .set_expanded
            .call1(&JsValue::UNDEFINED, &update.into_js_value())?;
        observe_catalog(render, id, true)?;
    }
    Ok(())
}

fn collect_closing(
    render: &CatalogRender,
    id: &str,
    closing: &mut BTreeSet<String>,
) -> Result<(), JsValue> {
    if closing.contains(id) || !set_has(&render.expanded, id)? {
        return Ok(());
    }
    closing.insert(id.to_owned());
    let catalog = Reflect::get(&render.catalogs, &JsValue::from_str(id))?;
    if catalog.is_undefined() {
        return Ok(());
    }
    for entry in Array::from(&required(&catalog, "entries", "subagent catalog")?).iter() {
        if required_string(&entry, "kind", "subagent catalog entry")? == "child" {
            collect_closing(
                render,
                &required_string(&entry, "id", "subagent child")?,
                closing,
            )?;
        }
    }
    Ok(())
}

fn observe_catalog(render: &CatalogRender, id: &str, open: bool) -> Result<(), JsValue> {
    if open {
        call_method(&render.observed, "add", &[JsValue::from_str(id)])?;
    } else {
        call_method(&render.observed, "delete", &[JsValue::from_str(id)])?;
    }
    render.set_catalog_open.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(id),
        &JsValue::from_bool(open),
    )?;
    Ok(())
}

fn close_all_catalogs(render: &CatalogRender) -> Result<(), JsValue> {
    for value in Array::from(&render.observed).iter() {
        render
            .set_catalog_open
            .call2(&JsValue::UNDEFINED, &value, &JsValue::FALSE)?;
    }
    call_method(&render.observed, "clear", &[])?;
    render
        .set_expanded
        .call1(&JsValue::UNDEFINED, &Set::new(&JsValue::UNDEFINED).into())?;
    Ok(())
}

fn parse_token_usage(summary: &JsValue) -> Result<Option<TokenUsage>, JsValue> {
    let Some(projections) = optional(summary, "projectionValues")? else {
        return Ok(None);
    };
    let Some(usage) = optional(&projections, "tokenUsage")? else {
        return Ok(None);
    };
    Ok(Some(TokenUsage {
        uncached_input_tokens: required_u64(&usage, "uncachedInputTokens")?,
        output_tokens: required_u64(&usage, "outputTokens")?,
        cache_read_tokens: required_u64(&usage, "cacheReadTokens")?,
        cache_write_tokens: required_u64(&usage, "cacheWriteTokens")?,
    }))
}

fn parse_timing(summary: &JsValue) -> Result<Option<SubagentTiming>, JsValue> {
    let Some(projections) = optional(summary, "projectionValues")? else {
        return Ok(None);
    };
    let Some(timing) = optional(&projections, "subagentTiming")? else {
        return Ok(None);
    };
    let active = optional(&timing, "active")?
        .map(|active| {
            Ok::<_, JsValue>(SubagentActiveTiming {
                since: required_i128(&active, "since")?,
                through: required_i128(&active, "through")?,
            })
        })
        .transpose()?;
    Ok(Some(SubagentTiming {
        settled_ms: required_i128(&timing, "settledMs")?,
        active,
    }))
}

fn required_u64(value: &JsValue, key: &str) -> Result<u64, JsValue> {
    let number = required(value, key, "numeric projection")?
        .as_f64()
        .ok_or_else(|| js_sys::Error::new(&format!("projection {key:?} must be numeric")))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as u64)
}

fn required_i128(value: &JsValue, key: &str) -> Result<i128, JsValue> {
    required(value, key, "timing projection")?
        .as_f64()
        .map(f64_as_i128)
        .ok_or_else(|| js_sys::Error::new(&format!("timing {key:?} must be numeric")).into())
}

fn render_duration(format: &DurationFormat, translate: &Function) -> Result<String, JsValue> {
    let values = format
        .values
        .iter()
        .map(|(key, value)| {
            (
                *key,
                match value {
                    DurationValue::Number(value) => JsValue::from_f64(u64_as_f64(*value)),
                    DurationValue::Text(value) => JsValue::from_str(value),
                },
            )
        })
        .collect::<Vec<_>>();
    translated_values(translate, &format!("duration.{}", format.key), &values)?
        .as_string()
        .ok_or_else(|| js_sys::Error::new("subagent duration must translate to a string").into())
}

fn descendant_summary(
    summaries: &JsValue,
    parent: &SessionId,
) -> Result<SubagentDescendantSummary, JsValue> {
    let summaries = Object::from(summaries.clone());
    let mut parsed = Vec::new();
    for key in Object::keys(&summaries).iter() {
        let key = key
            .as_string()
            .ok_or_else(|| js_sys::Error::new("Session summary key must be a string"))?;
        let summary = Reflect::get(&summaries, &JsValue::from_str(&key))?;
        let id = SessionId::new(required_string(&summary, "id", "Session summary")?);
        parsed.push(SubagentListSummary {
            id,
            parent_id: optional_string(&summary, "parentId")?.map(SessionId::new),
            subagent_origin: optional_string(&summary, "origin")?.as_deref() == Some("subagent"),
            running: required_bool(&summary, "running", "Session summary")?,
            display_title: required_string(&summary, "displayTitle", "Session summary")?,
        });
    }
    Ok(index_subagent_descendants(&parsed)
        .get(parent)
        .copied()
        .unwrap_or_default())
}

fn install_outside_effect(
    react: &JsValue,
    open: bool,
    root_ref: &JsValue,
    set_open: &Function,
    set_expanded: &Function,
    observed: &JsValue,
    set_catalog_open: &Function,
) -> Result<(), JsValue> {
    let root = root_ref.clone();
    let close = set_open.clone();
    let clear_expanded = set_expanded.clone();
    let observed = observed.clone();
    let set_catalog_open = set_catalog_open.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open {
            return Ok(JsValue::UNDEFINED);
        }
        let document = required(&js_sys::global(), "document", "global")?;
        let listener_root = root.clone();
        let listener_close = close.clone();
        let listener_expanded = clear_expanded.clone();
        let listener_observed = observed.clone();
        let listener_catalog = set_catalog_open.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            let target = Reflect::get(&event, &JsValue::from_str("target"))?;
            if !target.is_instance_of::<web_sys::Node>() {
                return Ok(());
            }
            let current = Reflect::get(&listener_root, &JsValue::from_str("current"))?;
            let contained = current
                .dyn_ref::<web_sys::Node>()
                .zip(target.dyn_ref::<web_sys::Node>())
                .is_some_and(|(root, target)| root.contains(Some(target)));
            if !contained {
                listener_close.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
                close_observed(&listener_observed, &listener_catalog, &listener_expanded)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
        .into_js_value();
        call_method(
            &document,
            "addEventListener",
            &[JsValue::from_str("pointerdown"), listener.clone()],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &document,
                "removeEventListener",
                &[JsValue::from_str("pointerdown"), listener.clone()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of1(&JsValue::from_bool(open)),
    )
}

fn install_timer_effect(
    react: &JsValue,
    open: bool,
    running_count: usize,
    set_now: &Function,
) -> Result<(), JsValue> {
    let setter = set_now.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !open || running_count == 0 {
            return Ok(JsValue::UNDEFINED);
        }
        let tick_setter = setter.clone();
        let tick = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            tick_setter.call1(&JsValue::UNDEFINED, &JsValue::from_f64(js_sys::Date::now()))?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let timer = call_method(
            &js_sys::global(),
            "setInterval",
            &[tick.clone(), JsValue::from_f64(1_000.0)],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &js_sys::global(),
                "clearInterval",
                std::slice::from_ref(&timer),
            );
            drop(tick.clone());
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(
            &JsValue::from_bool(open),
            &JsValue::from_f64(usize_as_f64(running_count)),
        ),
    )
}

fn install_unmount_effect(
    react: &JsValue,
    observed_ref: &JsValue,
    setter_ref: &JsValue,
) -> Result<(), JsValue> {
    let observed_ref = observed_ref.clone();
    let setter_ref = setter_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        let observed_ref = observed_ref.clone();
        let setter_ref = setter_ref.clone();
        Closure::wrap(Box::new(move || {
            let observed = Reflect::get(&observed_ref, &JsValue::from_str("current"))
                .unwrap_or(JsValue::UNDEFINED);
            let setter = Reflect::get(&setter_ref, &JsValue::from_str("current"))
                .ok()
                .and_then(|value| value.dyn_into::<Function>().ok());
            if let Some(setter) = setter {
                for id in Array::from(&observed).iter() {
                    let _ = setter.call2(&JsValue::UNDEFINED, &id, &JsValue::FALSE);
                }
            }
            let _ = call_method(&observed, "clear", &[]);
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(react, &effect.into_js_value(), &Array::new())
}

#[allow(clippy::too_many_arguments)]
fn install_visibility_effect(
    react: &JsValue,
    visible: bool,
    open: bool,
    set_open: &Function,
    set_expanded: &Function,
    observed: &JsValue,
    set_catalog_open: &Function,
) -> Result<(), JsValue> {
    let close = set_open.clone();
    let expanded = set_expanded.clone();
    let observed = observed.clone();
    let catalog = set_catalog_open.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !visible && open {
            close.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            close_observed(&observed, &catalog, &expanded)?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        react,
        &effect.into_js_value(),
        &Array::of2(&JsValue::from_bool(visible), &JsValue::from_bool(open)),
    )
}

fn close_observed(
    observed: &JsValue,
    set_catalog_open: &Function,
    set_expanded: &Function,
) -> Result<(), JsValue> {
    for id in Array::from(observed).iter() {
        set_catalog_open.call2(&JsValue::UNDEFINED, &id, &JsValue::FALSE)?;
    }
    call_method(observed, "clear", &[])?;
    set_expanded.call1(&JsValue::UNDEFINED, &Set::new(&JsValue::UNDEFINED).into())?;
    Ok(())
}

fn navigate_tree(
    event: &JsValue,
    root_ref: &JsValue,
    trigger_ref: &JsValue,
    render: &CatalogRender,
) -> Result<(), JsValue> {
    let key = required_string(event, "key", "keyboard event")?;
    let items = tree_items(root_ref)?;
    let document = required(&js_sys::global(), "document", "global")?;
    let active = Reflect::get(&document, &JsValue::from_str("activeElement"))?;
    let index = items
        .iter()
        .position(|item| Object::is(item, &active))
        .and_then(|index| isize::try_from(index).ok())
        .unwrap_or(-1);
    match key.as_str() {
        "Escape" => {
            call_method(event, "preventDefault", &[])?;
            render
                .set_open
                .call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            close_all_catalogs(render)?;
            let trigger_ref = trigger_ref.clone();
            queue_microtask(move || {
                let trigger = Reflect::get(&trigger_ref, &JsValue::from_str("current"))?;
                if !trigger.is_null() {
                    call_method(&trigger, "focus", &[])?;
                }
                Ok(())
            })?;
        }
        "Home" => {
            call_method(event, "preventDefault", &[])?;
            focus_items(&items, 0)?;
        }
        "End" => {
            call_method(event, "preventDefault", &[])?;
            focus_items(&items, isize::try_from(items.len()).unwrap_or(0) - 1)?;
        }
        "ArrowDown" => {
            call_method(event, "preventDefault", &[])?;
            focus_items(&items, index + 1)?;
        }
        "ArrowUp" => {
            call_method(event, "preventDefault", &[])?;
            focus_items(
                &items,
                if index < 0 {
                    isize::try_from(items.len()).unwrap_or(0) - 1
                } else {
                    index - 1
                },
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn focus_at(trigger_ref: &JsValue, index: isize) -> Result<(), JsValue> {
    let root = Reflect::get(trigger_ref, &JsValue::from_str("current"))?;
    if root.is_null() {
        return Ok(());
    }
    let parent = Reflect::get(&root, &JsValue::from_str("parentElement"))?;
    focus_items(&tree_items_value(&parent)?, index)
}

fn tree_items(root_ref: &JsValue) -> Result<Vec<JsValue>, JsValue> {
    let root = Reflect::get(root_ref, &JsValue::from_str("current"))?;
    tree_items_value(&root)
}

fn tree_items_value(root: &JsValue) -> Result<Vec<JsValue>, JsValue> {
    if root.is_null() || root.is_undefined() {
        return Ok(Vec::new());
    }
    Ok(Array::from(&call_method(
        root,
        "querySelectorAll",
        &[JsValue::from_str(
            "[role=\"treeitem\"]:not([aria-disabled=\"true\"])",
        )],
    )?)
    .iter()
    .collect())
}

fn focus_items(items: &[JsValue], index: isize) -> Result<(), JsValue> {
    if items.is_empty() {
        return Ok(());
    }
    let len = isize::try_from(items.len()).unwrap_or(isize::MAX);
    let wrapped = index.rem_euclid(len);
    let wrapped = usize::try_from(wrapped).unwrap_or(0);
    call_method(&items[wrapped], "focus", &[]).map(|_| ())
}

fn queue_microtask(task: impl FnMut() -> Result<(), JsValue> + 'static) -> Result<(), JsValue> {
    let task = Closure::wrap(Box::new(task) as Box<dyn FnMut() -> Result<(), JsValue>>);
    call_method(&js_sys::global(), "queueMicrotask", &[task.into_js_value()])?;
    Ok(())
}

fn set_has(set: &JsValue, id: &str) -> Result<bool, JsValue> {
    call_method(set, "has", &[JsValue::from_str(id)])?
        .as_bool()
        .ok_or_else(|| js_sys::Error::new("Set.has must return a boolean").into())
}

fn clone_set(current: &JsValue) -> Result<Set, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("Set"))?.dyn_into::<Function>()?;
    let arguments = Array::of1(current);
    Reflect::construct(&constructor, &arguments)?.dyn_into()
}

fn u64_as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}
