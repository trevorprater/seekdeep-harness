//! Compiled grouped, flat, and search bodies for the Workspace browser.

use std::{collections::BTreeSet, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_runtime::{ClientWorkspaceView, RuntimeSessionListState, SessionListPhase};
use seekdeep_identity::{SessionId, WorkspaceId};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

use crate::{
    DropHalf, FLAT_SESSION_ORDER_KEY, GroupNode, SessionOrderBy, TreeView, UNGROUPED_KEY,
    derive_flat, derive_groups, derive_search_results, next_session_order_account,
    reconciled_session_order, resolve_session_drop, resolve_workspace_drop,
};

use crate::{
    browser::{
        BrowserModules, call, class, component, element, function, object, required, tag,
        translated, use_state,
    },
    browser_model::{
        parse_json, search_items, session_ids, session_list, string_lists, timestamp_accounts,
        to_js, workspaces,
    },
    project_row_item_component, search_result_item_component, session_node_item_component,
};

const COLLAPSED_SESSION_LIMIT: usize = 5;

#[derive(Clone)]
pub(crate) struct ListComponents {
    pub(crate) tree: JsValue,
    pub(crate) flat: JsValue,
    pub(crate) search: JsValue,
}

pub(crate) fn configure_lists(modules: &BrowserModules) -> ListComponents {
    ListComponents {
        tree: component(modules, render_session_tree),
        flat: component(modules, render_flat_list),
        search: component(modules, render_search_results),
    }
}

fn identity_selector() -> JsValue {
    Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>)
        .into_js_value()
}

fn use_effect(react: &JsValue, effect: JsValue, deps: &Array) -> Result<(), JsValue> {
    let result = function(react, "useEffect", "React")?
        .call2(react, &effect, deps)
        .map(|_| ());
    drop(effect);
    result
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    function(react, "useRef", "React")?.call1(react, initial)
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn bool_property(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a boolean")).into())
}

fn string_property(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn number_property(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn translated_string(
    translate: &Function,
    key: &str,
    variables: Option<&js_sys::Object>,
) -> Result<String, JsValue> {
    translated(translate, key, variables)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Workspace translator must return a string").into())
}

fn order_by(value: &str) -> Result<SessionOrderBy, JsValue> {
    match value {
        "manual" => Ok(SessionOrderBy::Manual),
        "updated" => Ok(SessionOrderBy::Updated),
        value => {
            Err(js_sys::TypeError::new(&format!("unknown Workspace order mode {value:?}")).into())
        }
    }
}

fn js_strings(values: impl IntoIterator<Item = String>) -> Array {
    values
        .into_iter()
        .map(|value| JsValue::from_str(&value))
        .collect()
}

fn js_session_ids(values: &[SessionId]) -> Array {
    values
        .iter()
        .map(|value| JsValue::from_str(value.as_str()))
        .collect()
}

fn has_own(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    let object = required(&js_sys::global(), "Object", "globalThis")?;
    function(&object, "hasOwn", "Object")?
        .call2(&object, value, &JsValue::from_str(key))?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new("Object.hasOwn() must return a boolean").into())
}

fn timestamp_index(list: &RuntimeSessionListState) -> IndexMap<SessionId, i64> {
    list.by_id
        .iter()
        .map(|(id, summary)| (id.clone(), summary.updated_at))
        .collect()
}

fn ungrouped_session_ids(
    list: &RuntimeSessionListState,
    workspaces: &[Rc<ClientWorkspaceView>],
) -> Vec<SessionId> {
    let accounted = workspaces
        .iter()
        .flat_map(|workspace| workspace.session_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    list.ids
        .iter()
        .filter(|id| list.by_id.contains_key(*id) && !accounted.contains(*id))
        .cloned()
        .collect()
}

fn ordered_workspaces(
    workspaces: &[Rc<ClientWorkspaceView>],
    accounts: &IndexMap<String, Vec<String>>,
) -> Vec<Rc<ClientWorkspaceView>> {
    workspaces
        .iter()
        .map(|workspace| {
            let mut workspace = (**workspace).clone();
            workspace.session_ids = reconciled_session_order(
                &workspace.session_ids,
                accounts
                    .get(workspace.workspace_id.as_str())
                    .map(Vec::as_slice),
            );
            Rc::new(workspace)
        })
        .collect()
}

pub(crate) fn warn_rejection(result: &JsValue, prefix: &'static str) {
    let promise = Promise::resolve(result);
    let failure = Closure::wrap(Box::new(move |reason: JsValue| {
        if let Ok(console) = Reflect::get(&js_sys::global(), &JsValue::from_str("console"))
            && let Ok(warn) = function(&console, "warn", "console")
        {
            let _ = warn.call2(&console, &JsValue::from_str(prefix), &reason);
        }
    }) as Box<dyn FnMut(JsValue)>);
    let _ = promise.catch(&failure);
    drop(failure.into_js_value());
}

fn native_drag_acceptance(react: &JsValue, active: bool) -> Result<(), JsValue> {
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        if !active {
            return JsValue::UNDEFINED;
        }
        let document = match Reflect::get(&js_sys::global(), &JsValue::from_str("document")) {
            Ok(document) if !document.is_null() && !document.is_undefined() => document,
            _ => return JsValue::UNDEFINED,
        };
        let accept_drag = Closure::wrap(Box::new(move |event: JsValue| {
            let _ = call(&event, "preventDefault", &[]);
            if let Ok(transfer) = Reflect::get(&event, &JsValue::from_str("dataTransfer"))
                && !transfer.is_null()
                && !transfer.is_undefined()
            {
                let _ = Reflect::set(
                    &transfer,
                    &JsValue::from_str("dropEffect"),
                    &JsValue::from_str("move"),
                );
            }
        }) as Box<dyn FnMut(JsValue)>);
        let accept_drop = Closure::wrap(Box::new(move |event: JsValue| {
            let _ = call(&event, "preventDefault", &[]);
        }) as Box<dyn FnMut(JsValue)>);
        let drag_value = accept_drag.into_js_value();
        let drop_value = accept_drop.into_js_value();
        if call(
            &document,
            "addEventListener",
            &[JsValue::from_str("dragover"), drag_value.clone()],
        )
        .is_err()
            || call(
                &document,
                "addEventListener",
                &[JsValue::from_str("drop"), drop_value.clone()],
            )
            .is_err()
        {
            return JsValue::UNDEFINED;
        }
        Closure::wrap(Box::new(move || {
            let _ = call(
                &document,
                "removeEventListener",
                &[JsValue::from_str("dragover"), drag_value.clone()],
            );
            let _ = call(
                &document,
                "removeEventListener",
                &[JsValue::from_str("drop"), drop_value.clone()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(
        react,
        effect.into_js_value(),
        &Array::of1(&JsValue::from_bool(active)),
    )
}

fn drag_half(event: &JsValue) -> Result<DropHalf, JsValue> {
    let target = required(event, "currentTarget", "Workspace drag event")?;
    let rect = call(&target, "getBoundingClientRect", &[])?;
    let client_y = number_property(event, "clientY", "Workspace drag event")?;
    let top = number_property(&rect, "top", "Workspace group rectangle")?;
    let height = number_property(&rect, "height", "Workspace group rectangle")?;
    Ok(if client_y < top + height / 2.0 {
        DropHalf::Before
    } else {
        DropHalf::After
    })
}

fn half_name(half: DropHalf) -> &'static str {
    match half {
        DropHalf::Before => "before",
        DropHalf::After => "after",
    }
}

fn parsed_half(value: &JsValue) -> Result<DropHalf, JsValue> {
    match value.as_string().as_deref() {
        Some("before") => Ok(DropHalf::Before),
        Some("after") => Ok(DropHalf::After),
        _ => Err(js_sys::TypeError::new("drag half must be before or after").into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_session_drag(
    active: &JsValue,
    over: &JsValue,
    committed: &JsValue,
    set_drag: &Function,
    groups: &[GroupNode],
    account_orders: &IndexMap<String, Vec<SessionId>>,
    set_session_order: &Function,
    order_by: SessionOrderBy,
    insert_session_before: Option<&Function>,
) -> Result<(), JsValue> {
    if current(committed)?.as_bool() == Some(true) {
        return Ok(());
    }
    set_current(committed, &JsValue::TRUE)?;
    set_drag.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
    let account_key = string_property(active, "accountKey", "Session drag")?;
    let source = SessionId::new(string_property(active, "sessionId", "Session drag")?);
    let target = SessionId::new(string_property(over, "id", "Session drag marker")?);
    let half = parsed_half(&required(over, "half", "Session drag marker")?)?;
    let Some(group) = groups.iter().find(|group| group.key == account_key) else {
        return Ok(());
    };
    let visible = group
        .sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<Vec<_>>();
    let Some(account) = account_orders.get(&account_key) else {
        return Ok(());
    };
    let Some(drop) = resolve_session_drop(&visible, account, &source, &target, half) else {
        return Ok(());
    };
    set_session_order.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(&account_key),
        &js_session_ids(&drop.order),
    )?;
    if order_by == SessionOrderBy::Updated || account_key == UNGROUPED_KEY {
        return Ok(());
    }
    let Some(insert_session_before) = insert_session_before else {
        return Ok(());
    };
    let result = insert_session_before.call3(
        &JsValue::UNDEFINED,
        &JsValue::from_str(&account_key),
        &JsValue::from_str(source.as_str()),
        &drop
            .before
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    warn_rejection(&result, "session reorder rejected:");
    Ok(())
}

fn commit_workspace_drag(
    active: &JsValue,
    over: &JsValue,
    committed: &JsValue,
    set_drag: &Function,
    workspace_ids: &[WorkspaceId],
    insert_workspace_before: &Function,
) -> Result<(), JsValue> {
    if current(committed)?.as_bool() == Some(true) {
        return Ok(());
    }
    set_current(committed, &JsValue::TRUE)?;
    set_drag.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
    let source = WorkspaceId::new(string_property(active, "workspaceId", "Workspace drag")?);
    let target = WorkspaceId::new(string_property(over, "id", "Workspace drag marker")?);
    let half = parsed_half(&required(over, "half", "Workspace drag marker")?)?;
    let Some(drop) = resolve_workspace_drop(workspace_ids, &source, &target, half) else {
        return Ok(());
    };
    let result = insert_workspace_before.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(source.as_str()),
        &drop
            .before
            .as_ref()
            .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
    )?;
    warn_rejection(&result, "workspace reorder rejected:");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sync_group_accounts_effect(
    modules: &BrowserModules,
    snapshot_js: &JsValue,
    list: Rc<RuntimeSessionListState>,
    workspaces_js: &JsValue,
    workspaces: Vec<Rc<ClientWorkspaceView>>,
    order_by_value: &str,
    accounts_js: &JsValue,
    accounts: IndexMap<String, Vec<String>>,
    timestamps_js: &JsValue,
    timestamp_accounts_value: IndexMap<String, IndexMap<String, i64>>,
    previous_order_by: &JsValue,
    sync: Function,
) -> Result<(), JsValue> {
    let order_by = order_by(order_by_value)?;
    let previous = previous_order_by.clone();
    let order_value = order_by_value.to_owned();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if list.phase != SessionListPhase::Ready {
            return Ok(());
        }
        let prior = current(&previous)?.as_string().unwrap_or_default();
        let switched = prior != "updated" && order_value == "updated";
        set_current(&previous, &JsValue::from_str(&order_value))?;
        let ungrouped = ungrouped_session_ids(&list, &workspaces);
        let mut source_accounts = workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.workspace_id.as_str().to_owned(),
                    workspace
                        .session_ids
                        .iter()
                        .filter(|id| list.by_id.contains_key(*id))
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        source_accounts.push((UNGROUPED_KEY.to_owned(), ungrouped));
        let timestamp_index = timestamp_index(&list);
        for (key, session_ids) in source_accounts {
            let previous_order = accounts.get(&key).map(Vec::as_slice);
            let previous_timestamps = timestamp_accounts_value
                .get(&key)
                .cloned()
                .unwrap_or_default();
            let next = next_session_order_account(
                &session_ids,
                previous_order,
                &previous_timestamps,
                &timestamp_index,
                order_by,
                order_by == SessionOrderBy::Updated && (previous_order.is_none() || switched),
            );
            if next.changed {
                sync.call3(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(&key),
                    &js_session_ids(&next.order),
                    &to_js(&next.updated_at, "Session timestamp baseline")?,
                )?;
            }
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of5(
            snapshot_js,
            &JsValue::from_str(order_by_value),
            accounts_js,
            timestamps_js,
            workspaces_js,
        ),
    )
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)] // Source-compatible tree owns grouping, two drag axes, overflow, and rows.
fn render_session_tree(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let snapshot_js = function(props, "useSessions", "SessionTree props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let list = Rc::new(session_list(&snapshot_js)?);
    let workspaces_js = required(props, "workspaces", "SessionTree props")?;
    let source_workspaces = workspaces(&workspaces_js)?;
    let archived_js = required(props, "archivedSessionIds", "SessionTree props")?;
    let archived = session_ids(&archived_js, "archived Session ids")?;
    let group_expansion = required(props, "groupExpansion", "SessionTree props")?;
    let accounts_js = required(props, "sessionOrderByAccount", "SessionTree props")?;
    let accounts = string_lists(&accounts_js, "Session order accounts")?;
    let timestamps_js = required(props, "sessionUpdatedAtByAccount", "SessionTree props")?;
    let timestamp_accounts_value =
        timestamp_accounts(&timestamps_js, "Session timestamp accounts")?;
    let order_by_value = string_property(props, "orderBy", "SessionTree props")?;
    let order_by_mode = order_by(&order_by_value)?;
    let (expanded_all, set_expanded_all) = use_state(&modules.react, &Array::new().into())?;
    let expanded_all = parse_json::<Vec<String>>(&expanded_all, "expanded Session groups")?;
    let (drag, set_drag) = use_state(&modules.react, &JsValue::NULL)?;
    let session_committed = use_ref(&modules.react, &JsValue::FALSE)?;
    let (workspace_drag, set_workspace_drag) = use_state(&modules.react, &JsValue::NULL)?;
    let workspace_committed = use_ref(&modules.react, &JsValue::FALSE)?;
    let previous_order_by = use_ref(&modules.react, &JsValue::from_str(&order_by_value))?;
    native_drag_acceptance(&modules.react, !drag.is_null() || !workspace_drag.is_null())?;

    let current_id = list.current.as_ref().map(|id| id.as_str().to_owned());
    let current_group = current_id.as_ref().map(|current| {
        source_workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .session_ids
                    .iter()
                    .any(|session| session.as_str() == current)
            })
            .map_or_else(
                || UNGROUPED_KEY.to_owned(),
                |workspace| workspace.workspace_id.as_str().to_owned(),
            )
    });
    let set_group_expanded = function(props, "setGroupExpanded", "SessionTree props")?;
    let expansion_effect_group = current_group.clone();
    let expansion_effect_current = current_id.clone();
    let expansion_effect_state = group_expansion.clone();
    let expansion_effect_setter = set_group_expanded.clone();
    let expansion_effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let (Some(_), Some(group)) = (&expansion_effect_current, &expansion_effect_group) else {
            return Ok(());
        };
        if !has_own(&expansion_effect_state, group)? {
            expansion_effect_setter.call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str(group),
                &JsValue::TRUE,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        expansion_effect.into_js_value(),
        &Array::of3(
            &current_id
                .as_ref()
                .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id)),
            &current_group
                .as_ref()
                .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id)),
            &group_expansion,
        ),
    )?;

    sync_group_accounts_effect(
        modules,
        &snapshot_js,
        list.clone(),
        &workspaces_js,
        source_workspaces.clone(),
        &order_by_value,
        &accounts_js,
        accounts.clone(),
        &timestamps_js,
        timestamp_accounts_value,
        &previous_order_by,
        function(props, "syncSessionOrderAccount", "SessionTree props")?,
    )?;

    let group_expansion_object = group_expansion.clone().dyn_into::<js_sys::Object>()?;
    let expanded_groups = js_sys::Object::entries(&group_expansion_object)
        .iter()
        .filter_map(|entry| {
            let entry = Array::from(&entry);
            (entry.get(1).as_bool() == Some(true))
                .then(|| entry.get(0).as_string())
                .flatten()
        })
        .collect::<Vec<_>>();
    let ungrouped_ids = ungrouped_session_ids(&list, &source_workspaces);
    let ordered_source = ordered_workspaces(&source_workspaces, &accounts);
    let ordered_ungrouped = reconciled_session_order(
        &ungrouped_ids,
        accounts.get(UNGROUPED_KEY).map(Vec::as_slice),
    );
    let groups = derive_groups(
        &list,
        &ordered_source,
        &archived,
        &TreeView {
            expanded_groups,
            ungrouped_order: accounts.get(UNGROUPED_KEY).cloned(),
        },
    );
    let account_orders = ordered_source
        .iter()
        .map(|workspace| {
            (
                workspace.workspace_id.as_str().to_owned(),
                workspace.session_ids.clone(),
            )
        })
        .chain(std::iter::once((
            UNGROUPED_KEY.to_owned(),
            ordered_ungrouped,
        )))
        .collect::<IndexMap<_, _>>();
    let workspace_ids = source_workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.clone())
        .collect::<Vec<_>>();
    let now = call(
        &required(&js_sys::global(), "Date", "globalThis")?,
        "now",
        &[],
    )?;
    let first_workspace = groups.first().and_then(|group| group.workspace_id.as_ref());
    let workspace_drop_at_start = first_workspace.is_some_and(|first| {
        if workspace_drag.is_null() {
            return false;
        }
        Reflect::get(&workspace_drag, &JsValue::from_str("over"))
            .ok()
            .filter(|over| !over.is_null() && !over.is_undefined())
            .and_then(|over| {
                Some((
                    Reflect::get(&over, &JsValue::from_str("id"))
                        .ok()?
                        .as_string()?,
                    Reflect::get(&over, &JsValue::from_str("half"))
                        .ok()?
                        .as_string()?,
                ))
            })
            .is_some_and(|(id, half)| id == first.as_str() && half == "before")
    });

    let mut group_children = Vec::new();
    if workspace_drop_at_start {
        group_children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("listTopDropIndicator", true)])),
                ),
                ("aria-hidden", JsValue::TRUE),
            ])?),
            &[],
        )?);
    }
    let mut sections = Vec::new();
    for group in &groups {
        let workspace_id = group.workspace_id.clone();
        let marker = workspace_id.as_ref().and_then(|workspace_id| {
            if workspace_drag.is_null() {
                return None;
            }
            let over = Reflect::get(&workspace_drag, &JsValue::from_str("over")).ok()?;
            if over.is_null() || over.is_undefined() {
                return None;
            }
            (Reflect::get(&over, &JsValue::from_str("id"))
                .ok()?
                .as_string()
                .as_deref()
                == Some(workspace_id.as_str()))
            .then(|| {
                Reflect::get(&over, &JsValue::from_str("half"))
                    .ok()?
                    .as_string()
            })
            .flatten()
        });
        let project_drag = if let Some(workspace_id) = &workspace_id {
            let start_setter = set_workspace_drag.clone();
            let start_committed = workspace_committed.clone();
            let start_id = workspace_id.as_str().to_owned();
            let start = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                set_current(&start_committed, &JsValue::FALSE)?;
                start_setter.call1(
                    &JsValue::UNDEFINED,
                    &object(&[
                        ("workspaceId", JsValue::from_str(&start_id)),
                        ("over", JsValue::NULL),
                    ])?
                    .into(),
                )?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let end_drag = workspace_drag.clone();
            let end_setter = set_workspace_drag.clone();
            let end_committed = workspace_committed.clone();
            let end_ids = workspace_ids.clone();
            let end_insert = function(props, "insertWorkspaceBefore", "SessionTree props")?;
            let end = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                if end_drag.is_null() {
                    end_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
                } else {
                    let over = Reflect::get(&end_drag, &JsValue::from_str("over"))?;
                    if !over.is_null() && !over.is_undefined() {
                        commit_workspace_drag(
                            &end_drag,
                            &over,
                            &end_committed,
                            &end_setter,
                            &end_ids,
                            &end_insert,
                        )?;
                    } else {
                        end_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
                    }
                }
                set_current(&end_committed, &JsValue::FALSE)
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            Some(object(&[
                ("start", start.into_js_value()),
                ("end", end.into_js_value()),
            ])?)
        } else {
            None
        };

        let toggle_setter = set_group_expanded.clone();
        let toggle_expanded_all = set_expanded_all.clone();
        let toggle_expanded_all_values = expanded_all.clone();
        let toggle_key = group.key.clone();
        let toggle_expanded = group.expanded;
        let on_toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if toggle_expanded {
                let retained = toggle_expanded_all_values
                    .iter()
                    .filter(|key| *key != &toggle_key)
                    .cloned()
                    .collect::<Vec<_>>();
                toggle_expanded_all.call1(&JsValue::UNDEFINED, &js_strings(retained))?;
            }
            toggle_setter.call2(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&toggle_key),
                &JsValue::from_bool(!toggle_expanded),
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let create_setter = set_group_expanded.clone();
        let create_start = function(props, "startSession", "SessionTree props")?;
        let create_key = group.key.clone();
        let create_workspace = workspace_id.clone();
        let on_create = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if let Some(workspace_id) = &create_workspace {
                create_setter.call2(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(&create_key),
                    &JsValue::TRUE,
                )?;
                create_start.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(workspace_id.as_str()),
                )?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let actions = workspace_id
            .as_ref()
            .map(|workspace_id| {
                let rename = function(props, "onRenameRequest", "SessionTree props")?;
                let rename_id = workspace_id.as_str().to_owned();
                let rename_title = group.label.clone();
                let rename_action = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                    rename.call2(
                        &JsValue::UNDEFINED,
                        &JsValue::from_str(&rename_id),
                        &JsValue::from_str(&rename_title),
                    )?;
                    Ok(())
                })
                    as Box<dyn FnMut() -> Result<(), JsValue>>);
                let delete = function(props, "onDeleteRequest", "SessionTree props")?;
                let delete_id = workspace_id.as_str().to_owned();
                let delete_title = group.label.clone();
                let delete_action = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                    delete.call2(
                        &JsValue::UNDEFINED,
                        &JsValue::from_str(&delete_id),
                        &JsValue::from_str(&delete_title),
                    )?;
                    Ok(())
                })
                    as Box<dyn FnMut() -> Result<(), JsValue>>);
                object(&[
                    ("rename", rename_action.into_js_value()),
                    ("delete", delete_action.into_js_value()),
                ])
            })
            .transpose()?;
        let mut project_props = vec![
            ("group", to_js(group, "Workspace group")?),
            ("t", required(props, "t", "SessionTree props")?),
            ("onToggle", on_toggle.into_js_value()),
            ("onCreate", on_create.into_js_value()),
        ];
        if let Some(drag) = project_drag {
            project_props.push(("drag", drag.into()));
        }
        if let Some(actions) = actions {
            project_props.push(("actions", actions.into()));
        }
        let mut section_children = vec![element(
            &modules.react,
            &project_row_item_component()?,
            Some(&object(&project_props)?),
            &[],
        )?];
        let visible_count = if expanded_all.iter().any(|key| key == &group.key) {
            group.sessions.len()
        } else {
            group.sessions.len().min(COLLAPSED_SESSION_LIMIT)
        };
        for node in group.sessions.iter().take(visible_count) {
            let same_drag = !drag.is_null()
                && string_property(&drag, "accountKey", "Session drag")? == group.key;
            let marker = if same_drag {
                let over = Reflect::get(&drag, &JsValue::from_str("over"))?;
                if !over.is_null()
                    && string_property(&over, "id", "Session drag marker")? == node.id.as_str()
                {
                    required(&over, "half", "Session drag marker")?
                } else {
                    JsValue::NULL
                }
            } else {
                JsValue::NULL
            };
            let start_setter = set_drag.clone();
            let start_committed = session_committed.clone();
            let start_account = group.key.clone();
            let start_id = node.id.as_str().to_owned();
            let start = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                set_current(&start_committed, &JsValue::FALSE)?;
                start_setter.call1(
                    &JsValue::UNDEFINED,
                    &object(&[
                        ("accountKey", JsValue::from_str(&start_account)),
                        ("sessionId", JsValue::from_str(&start_id)),
                        ("over", JsValue::NULL),
                    ])?
                    .into(),
                )?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let hover_setter = set_drag.clone();
            let hover_drag = drag.clone();
            let hover_id = node.id.as_str().to_owned();
            let hover = Closure::wrap(Box::new(move |half: String| -> Result<(), JsValue> {
                if hover_drag.is_null() {
                    return Ok(());
                }
                hover_setter.call1(
                    &JsValue::UNDEFINED,
                    &object(&[
                        (
                            "accountKey",
                            required(&hover_drag, "accountKey", "Session drag")?,
                        ),
                        (
                            "sessionId",
                            required(&hover_drag, "sessionId", "Session drag")?,
                        ),
                        (
                            "over",
                            object(&[
                                ("id", JsValue::from_str(&hover_id)),
                                ("half", JsValue::from_str(&half)),
                            ])?
                            .into(),
                        ),
                    ])?
                    .into(),
                )?;
                Ok(())
            })
                as Box<dyn FnMut(String) -> Result<(), JsValue>>);
            let drop_drag = drag.clone();
            let drop_id = node.id.as_str().to_owned();
            let drop_committed = session_committed.clone();
            let drop_setter = set_drag.clone();
            let drop_groups = groups.clone();
            let drop_accounts = account_orders.clone();
            let drop_action = function(props, "setSessionOrder", "SessionTree props")?;
            let drop_insert = function(props, "insertSessionBefore", "SessionTree props")?;
            let drop = Closure::wrap(Box::new(move |half: String| -> Result<(), JsValue> {
                if drop_drag.is_null() {
                    return Ok(());
                }
                let over = object(&[
                    ("id", JsValue::from_str(&drop_id)),
                    ("half", JsValue::from_str(&half)),
                ])?;
                commit_session_drag(
                    &drop_drag,
                    &over,
                    &drop_committed,
                    &drop_setter,
                    &drop_groups,
                    &drop_accounts,
                    &drop_action,
                    order_by_mode,
                    Some(&drop_insert),
                )
            })
                as Box<dyn FnMut(String) -> Result<(), JsValue>>);
            let end_drag = drag.clone();
            let end_committed = session_committed.clone();
            let end_setter = set_drag.clone();
            let end_groups = groups.clone();
            let end_accounts = account_orders.clone();
            let end_action = function(props, "setSessionOrder", "SessionTree props")?;
            let end_insert = function(props, "insertSessionBefore", "SessionTree props")?;
            let end = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                if !end_drag.is_null() {
                    let over = Reflect::get(&end_drag, &JsValue::from_str("over"))?;
                    if !over.is_null() && !over.is_undefined() {
                        commit_session_drag(
                            &end_drag,
                            &over,
                            &end_committed,
                            &end_setter,
                            &end_groups,
                            &end_accounts,
                            &end_action,
                            order_by_mode,
                            Some(&end_insert),
                        )?;
                    } else {
                        end_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
                    }
                }
                set_current(&end_committed, &JsValue::FALSE)
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            let row_props = object(&[
                ("node", to_js(node, "Session row")?),
                (
                    "currentId",
                    current_id
                        .as_ref()
                        .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id)),
                ),
                ("now", now.clone()),
                ("onOpen", required(props, "open", "SessionTree props")?),
                (
                    "onRename",
                    required(props, "onSessionRename", "SessionTree props")?,
                ),
                (
                    "onFork",
                    required(props, "forkSession", "SessionTree props")?,
                ),
                (
                    "onArchive",
                    required(props, "onSessionArchive", "SessionTree props")?,
                ),
                (
                    "drag",
                    object(&[
                        ("start", start.into_js_value()),
                        ("active", JsValue::from_bool(same_drag)),
                        ("marker", marker),
                        ("hover", hover.into_js_value()),
                        ("drop", drop.into_js_value()),
                        ("end", end.into_js_value()),
                    ])?
                    .into(),
                ),
                ("t", required(props, "t", "SessionTree props")?),
            ])?;
            section_children.push(element(
                &modules.react,
                &session_node_item_component()?,
                Some(&row_props),
                &[],
            )?);
        }
        if group.sessions.len() > COLLAPSED_SESSION_LIMIT {
            let overflow_setter = set_expanded_all.clone();
            let overflow_key = group.key.clone();
            let overflow_expanded = expanded_all.iter().any(|key| key == &group.key);
            let overflow_current = expanded_all.clone();
            let overflow = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                let next = if overflow_expanded {
                    overflow_current
                        .iter()
                        .filter(|key| *key != &overflow_key)
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    let mut next = overflow_current.clone();
                    next.push(overflow_key.clone());
                    next
                };
                overflow_setter.call1(&JsValue::UNDEFINED, &js_strings(next))?;
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>);
            section_children.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str(&class(&[("sessionOverflowButton", true)])),
                    ),
                    ("aria-expanded", JsValue::from_bool(overflow_expanded)),
                    ("onClick", overflow.into_js_value()),
                ])?),
                &[JsValue::from_str(&if overflow_expanded {
                    translated_string(
                        &function(props, "t", "SessionTree props")?,
                        "sessions.collapse",
                        None,
                    )?
                } else {
                    translated_string(
                        &function(props, "t", "SessionTree props")?,
                        "sessions.expand",
                        Some(&object(&[(
                            "n",
                            JsValue::from_f64(
                                (group.sessions.len() - COLLAPSED_SESSION_LIMIT) as f64,
                            ),
                        )])?),
                    )?
                })],
            )?);
        }

        let wrapper_drag = workspace_drag.clone();
        let wrapper_workspace = workspace_id.clone();
        let wrapper_setter = set_workspace_drag.clone();
        let on_drag_over = if !workspace_drag.is_null() && workspace_id.is_some() {
            Some(
                Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                    call(&event, "preventDefault", &[])?;
                    let transfer = required(&event, "dataTransfer", "Workspace drag event")?;
                    Reflect::set(
                        &transfer,
                        &JsValue::from_str("dropEffect"),
                        &JsValue::from_str("move"),
                    )?;
                    let Some(workspace_id) = &wrapper_workspace else {
                        return Ok(());
                    };
                    wrapper_setter.call1(
                        &JsValue::UNDEFINED,
                        &object(&[
                            (
                                "workspaceId",
                                required(&wrapper_drag, "workspaceId", "Workspace drag")?,
                            ),
                            (
                                "over",
                                object(&[
                                    ("id", JsValue::from_str(workspace_id.as_str())),
                                    ("half", JsValue::from_str(half_name(drag_half(&event)?))),
                                ])?
                                .into(),
                            ),
                        ])?
                        .into(),
                    )?;
                    Ok(())
                })
                    as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
                .into_js_value(),
            )
        } else {
            None
        };
        let drop_drag = workspace_drag.clone();
        let drop_workspace = workspace_id.clone();
        let drop_committed = workspace_committed.clone();
        let drop_setter = set_workspace_drag.clone();
        let drop_ids = workspace_ids.clone();
        let drop_insert = function(props, "insertWorkspaceBefore", "SessionTree props")?;
        let on_drop = if !workspace_drag.is_null() && workspace_id.is_some() {
            Some(
                Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                    call(&event, "preventDefault", &[])?;
                    let Some(workspace_id) = &drop_workspace else {
                        return Ok(());
                    };
                    let over = object(&[
                        ("id", JsValue::from_str(workspace_id.as_str())),
                        ("half", JsValue::from_str(half_name(drag_half(&event)?))),
                    ])?;
                    commit_workspace_drag(
                        &drop_drag,
                        &over,
                        &drop_committed,
                        &drop_setter,
                        &drop_ids,
                        &drop_insert,
                    )
                })
                    as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
                .into_js_value(),
            )
        } else {
            None
        };
        sections.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("key", JsValue::from_str(&group.key)),
                (
                    "className",
                    JsValue::from_str(&class(&[
                        ("groupSection", true),
                        ("workspaceDropBefore", marker.as_deref() == Some("before")),
                        ("workspaceDropAfter", marker.as_deref() == Some("after")),
                    ])),
                ),
                ("onDragOver", on_drag_over.unwrap_or(JsValue::UNDEFINED)),
                ("onDrop", on_drop.unwrap_or(JsValue::UNDEFINED)),
            ])?),
            &section_children,
        )?);
    }
    if groups.is_empty() {
        sections.push(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("empty", true)])),
            )])?),
            &[translated(
                &function(props, "t", "SessionTree props")?,
                "empty.none",
                None,
            )?],
        )?);
    }
    group_children.push(tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class(&[
                    ("list", true),
                    ("listTopDropActive", workspace_drop_at_start),
                ])),
            ),
            ("role", JsValue::from_str("tree")),
            (
                "aria-label",
                translated(
                    &function(props, "t", "SessionTree props")?,
                    "section.sessions",
                    None,
                )?,
            ),
        ])?),
        &sections,
    )?);
    group_children.push(tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("fade", true)])),
        )])?),
        &[],
    )?);
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("treeBody", true), ("wide", true)])),
        )])?),
        &group_children,
    )
}

#[allow(clippy::too_many_arguments)]
fn sync_flat_account_effect(
    modules: &BrowserModules,
    snapshot_js: &JsValue,
    list: Rc<RuntimeSessionListState>,
    session_ids: Vec<SessionId>,
    order_by_value: &str,
    accounts_js: &JsValue,
    accounts: IndexMap<String, Vec<String>>,
    timestamps_js: &JsValue,
    timestamps: IndexMap<String, IndexMap<String, i64>>,
    previous_order_by: &JsValue,
    sync: Function,
) -> Result<(), JsValue> {
    let mode = order_by(order_by_value)?;
    let previous = previous_order_by.clone();
    let order_value = order_by_value.to_owned();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if list.phase != SessionListPhase::Ready {
            return Ok(());
        }
        let prior = current(&previous)?.as_string().unwrap_or_default();
        let switched = prior != "updated" && order_value == "updated";
        set_current(&previous, &JsValue::from_str(&order_value))?;
        let previous_order = accounts.get(FLAT_SESSION_ORDER_KEY).map(Vec::as_slice);
        let previous_timestamps = timestamps
            .get(FLAT_SESSION_ORDER_KEY)
            .cloned()
            .unwrap_or_default();
        let next = next_session_order_account(
            &session_ids,
            previous_order,
            &previous_timestamps,
            &timestamp_index(&list),
            mode,
            mode == SessionOrderBy::Updated && (previous_order.is_none() || switched),
        );
        if next.changed {
            sync.call3(
                &JsValue::UNDEFINED,
                &JsValue::from_str(FLAT_SESSION_ORDER_KEY),
                &js_session_ids(&next.order),
                &to_js(&next.updated_at, "flat Session timestamp baseline")?,
            )?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of4(
            snapshot_js,
            &JsValue::from_str(order_by_value),
            accounts_js,
            timestamps_js,
        ),
    )
}

#[allow(clippy::too_many_lines)] // Flat list owns its local order account and one drag lifecycle.
fn render_flat_list(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let snapshot_js = function(props, "useSessions", "FlatList props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let list = Rc::new(session_list(&snapshot_js)?);
    let archived = session_ids(
        &required(props, "archivedSessionIds", "FlatList props")?,
        "archived Session ids",
    )?;
    let base_rows = derive_flat(&list, &archived);
    let session_ids_value = base_rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let accounts_js = required(props, "sessionOrderByAccount", "FlatList props")?;
    let accounts = string_lists(&accounts_js, "Session order accounts")?;
    let timestamps_js = required(props, "sessionUpdatedAtByAccount", "FlatList props")?;
    let timestamps = timestamp_accounts(&timestamps_js, "Session timestamp accounts")?;
    let order_by_value = string_property(props, "orderBy", "FlatList props")?;
    let previous_order_by = use_ref(&modules.react, &JsValue::from_str(&order_by_value))?;
    sync_flat_account_effect(
        modules,
        &snapshot_js,
        list.clone(),
        session_ids_value.clone(),
        &order_by_value,
        &accounts_js,
        accounts.clone(),
        &timestamps_js,
        timestamps,
        &previous_order_by,
        function(props, "syncSessionOrderAccount", "FlatList props")?,
    )?;
    let by_id = base_rows
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<IndexMap<_, _>>();
    let rows = reconciled_session_order(
        &session_ids_value,
        accounts.get(FLAT_SESSION_ORDER_KEY).map(Vec::as_slice),
    )
    .into_iter()
    .filter_map(|id| by_id.get(&id).cloned())
    .collect::<Vec<_>>();
    let (drag, set_drag) = use_state(&modules.react, &JsValue::NULL)?;
    let committed = use_ref(&modules.react, &JsValue::FALSE)?;
    native_drag_acceptance(&modules.react, !drag.is_null())?;
    let now = call(
        &required(&js_sys::global(), "Date", "globalThis")?,
        "now",
        &[],
    )?;
    let row_ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
    let account_orders = IndexMap::from([(FLAT_SESSION_ORDER_KEY.to_owned(), row_ids.clone())]);
    let synthetic_group = GroupNode {
        key: FLAT_SESSION_ORDER_KEY.to_owned(),
        workspace_id: None,
        cwd: None,
        created_at: None,
        label: String::new(),
        session_count: rows.len(),
        expanded: true,
        contains_current: false,
        sessions: rows.clone(),
    };
    let groups = vec![synthetic_group];
    let set_session_order = function(props, "setSessionOrder", "FlatList props")?;
    let mut children = Vec::new();
    if rows.is_empty() {
        children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("empty", true)])),
            )])?),
            &[translated(
                &function(props, "t", "FlatList props")?,
                "empty.none",
                None,
            )?],
        )?);
    }
    for node in &rows {
        let active = !drag.is_null();
        let marker = if active {
            let over = Reflect::get(&drag, &JsValue::from_str("over"))?;
            if !over.is_null()
                && string_property(&over, "id", "Session drag marker")? == node.id.as_str()
            {
                required(&over, "half", "Session drag marker")?
            } else {
                JsValue::NULL
            }
        } else {
            JsValue::NULL
        };
        let start_setter = set_drag.clone();
        let start_committed = committed.clone();
        let start_id = node.id.as_str().to_owned();
        let start = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            set_current(&start_committed, &JsValue::FALSE)?;
            start_setter.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("accountKey", JsValue::from_str(FLAT_SESSION_ORDER_KEY)),
                    ("sessionId", JsValue::from_str(&start_id)),
                    ("over", JsValue::NULL),
                ])?
                .into(),
            )?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let hover_drag = drag.clone();
        let hover_setter = set_drag.clone();
        let hover_id = node.id.as_str().to_owned();
        let hover = Closure::wrap(Box::new(move |half: String| -> Result<(), JsValue> {
            if hover_drag.is_null() {
                return Ok(());
            }
            hover_setter.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("accountKey", JsValue::from_str(FLAT_SESSION_ORDER_KEY)),
                    (
                        "sessionId",
                        required(&hover_drag, "sessionId", "Session drag")?,
                    ),
                    (
                        "over",
                        object(&[
                            ("id", JsValue::from_str(&hover_id)),
                            ("half", JsValue::from_str(&half)),
                        ])?
                        .into(),
                    ),
                ])?
                .into(),
            )?;
            Ok(())
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
        let drop_drag = drag.clone();
        let drop_setter = set_drag.clone();
        let drop_committed = committed.clone();
        let drop_groups = groups.clone();
        let drop_accounts = account_orders.clone();
        let drop_action = set_session_order.clone();
        let drop_id = node.id.as_str().to_owned();
        let drop = Closure::wrap(Box::new(move |half: String| -> Result<(), JsValue> {
            if drop_drag.is_null() {
                return Ok(());
            }
            commit_session_drag(
                &drop_drag,
                &object(&[
                    ("id", JsValue::from_str(&drop_id)),
                    ("half", JsValue::from_str(&half)),
                ])?
                .into(),
                &drop_committed,
                &drop_setter,
                &drop_groups,
                &drop_accounts,
                &drop_action,
                SessionOrderBy::Updated,
                None,
            )
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
        let end_drag = drag.clone();
        let end_setter = set_drag.clone();
        let end_committed = committed.clone();
        let end_groups = groups.clone();
        let end_accounts = account_orders.clone();
        let end_action = set_session_order.clone();
        let end = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            if !end_drag.is_null() {
                let over = Reflect::get(&end_drag, &JsValue::from_str("over"))?;
                if !over.is_null() && !over.is_undefined() {
                    commit_session_drag(
                        &end_drag,
                        &over,
                        &end_committed,
                        &end_setter,
                        &end_groups,
                        &end_accounts,
                        &end_action,
                        SessionOrderBy::Updated,
                        None,
                    )?;
                } else {
                    end_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
                }
            }
            set_current(&end_committed, &JsValue::FALSE)
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let row_props = object(&[
            ("node", to_js(node, "flat Session row")?),
            (
                "currentId",
                list.current
                    .as_ref()
                    .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
            ),
            ("now", now.clone()),
            ("onOpen", required(props, "open", "FlatList props")?),
            (
                "onRename",
                required(props, "onSessionRename", "FlatList props")?,
            ),
            ("onFork", required(props, "forkSession", "FlatList props")?),
            (
                "onArchive",
                required(props, "onSessionArchive", "FlatList props")?,
            ),
            ("flat", JsValue::TRUE),
            (
                "drag",
                object(&[
                    ("start", start.into_js_value()),
                    ("active", JsValue::from_bool(active)),
                    ("marker", marker),
                    ("hover", hover.into_js_value()),
                    ("drop", drop.into_js_value()),
                    ("end", end.into_js_value()),
                ])?
                .into(),
            ),
            ("t", required(props, "t", "FlatList props")?),
        ])?;
        children.push(element(
            &modules.react,
            &session_node_item_component()?,
            Some(&row_props),
            &[],
        )?);
    }
    let list_node = tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class(&[("list", true), ("flatList", true)])),
            ),
            ("role", JsValue::from_str("tree")),
            (
                "aria-label",
                translated(
                    &function(props, "t", "FlatList props")?,
                    "section.sessions",
                    None,
                )?,
            ),
        ])?),
        &children,
    )?;
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("treeBody", true), ("wide", true)])),
        )])?),
        &[
            list_node,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("fade", true)])),
                )])?),
                &[],
            )?,
        ],
    )
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn render_search_results(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let snapshot_js = function(props, "useSessions", "SearchResults props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let list = session_list(&snapshot_js)?;
    let workspaces_value = workspaces(&required(props, "workspaces", "SearchResults props")?)?;
    let archived = session_ids(
        &required(props, "archivedSessionIds", "SearchResults props")?,
        "archived Session ids",
    )?;
    let query = string_property(props, "query", "SearchResults props")?;
    let remote = required(props, "remote", "SearchResults props")?;
    let remote_query = string_property(&remote, "query", "remote search state")?;
    let (status, items, has_more) = if remote_query == query {
        (
            string_property(&remote, "status", "remote search state")?,
            search_items(&required(&remote, "items", "remote search state")?)?,
            bool_property(&remote, "hasMore", "remote search state")?,
        )
    } else {
        ("loading".to_owned(), Vec::new(), false)
    };
    let limit = number_property(props, "resultLimit", "SearchResults props")?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let limit = limit as usize;
    let results = derive_search_results(
        &list,
        &workspaces_value,
        &query,
        &archived,
        &crate::SessionSearchPage { items, has_more },
        limit,
    );
    let mut rows = Vec::new();
    for result in &results.items {
        rows.push(element(
            &modules.react,
            &search_result_item_component()?,
            Some(&object(&[
                ("key", JsValue::from_str(result.id.as_str())),
                ("result", to_js(result, "search result row")?),
                (
                    "currentId",
                    list.current
                        .as_ref()
                        .map_or(JsValue::UNDEFINED, |id| JsValue::from_str(id.as_str())),
                ),
                ("onOpen", required(props, "open", "SearchResults props")?),
                ("t", required(props, "t", "SearchResults props")?),
            ])?),
            &[],
        )?);
    }
    let search_tree = tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class(&[("searchTree", true)])),
            ),
            ("role", JsValue::from_str("tree")),
            (
                "aria-label",
                translated(
                    &function(props, "t", "SearchResults props")?,
                    "search.results.aria",
                    None,
                )?,
            ),
        ])?),
        &rows,
    )?;
    let mut list_children = vec![search_tree];
    let translate = function(props, "t", "SearchResults props")?;
    if status == "loading" {
        list_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("searchStatus", true)])),
                ),
                ("role", JsValue::from_str("status")),
            ])?),
            &[translated(&translate, "search.pending", None)?],
        )?);
    }
    if status == "error" {
        list_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("searchWarning", true)])),
                ),
                ("role", JsValue::from_str("status")),
            ])?),
            &[translated(&translate, "search.unavailable", None)?],
        )?);
    }
    if status != "loading" && results.items.is_empty() {
        list_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("empty", true)])),
            )])?),
            &[translated(&translate, "search.noMatches", None)?],
        )?);
    }
    if results.has_more {
        list_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("searchStatus", true)])),
            )])?),
            &[translated(
                &translate,
                "search.hasMore",
                Some(&object(&[("n", JsValue::from_f64(limit as f64))])?),
            )?],
        )?);
    }
    let list = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("list", true)])),
        )])?),
        &list_children,
    )?;
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("treeBody", true), ("wide", true)])),
        )])?),
        &[
            list,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("fade", true)])),
                )])?),
                &[],
            )?,
        ],
    )
}
