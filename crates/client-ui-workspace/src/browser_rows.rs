//! Compiled Workspace browser tree-row components.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{RelativeTimeUnit, relative_time};

use crate::browser::{
    BrowserModules, call, class, component, element, function, inject_style, object, required, tag,
    translated, use_state,
};

const ROWS_CSS: &str =
    include_str!("../../../packages/client/ui-workspace/src/client/rows/Rows.module.css");

thread_local! {
    static COMPONENTS: RefCell<Option<RowComponents>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct RowComponents {
    project: JsValue,
    search: JsValue,
    session: JsValue,
}

pub(crate) fn configure_rows(modules: &BrowserModules) -> Result<(), JsValue> {
    inject_style("rows/Rows", ROWS_CSS)?;
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(RowComponents {
            project: component(modules, render_project_row),
            search: component(modules, render_search_result),
            session: component(modules, render_session_row),
        });
    });
    Ok(())
}

fn configured_components() -> Result<RowComponents, JsValue> {
    COMPONENTS.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-workspace row components were not configured").into()
        })
    })
}

macro_rules! component_getter {
    ($rust:ident, $js:literal, $field:ident, $label:literal) => {
        #[doc = concat!("Returns the compiled `", $label, "` component.")]
        ///
        /// # Errors
        ///
        /// Returns before browser configuration.
        #[wasm_bindgen(js_name = $js)]
        pub fn $rust() -> Result<JsValue, JsValue> {
            Ok(configured_components()?.$field)
        }
    };
}

component_getter!(
    project_row_item_component,
    "projectRowItemComponent",
    project,
    "ProjectRowItem"
);
component_getter!(
    search_result_item_component,
    "searchResultItemComponent",
    search,
    "SearchResultItem"
);
component_getter!(
    session_node_item_component,
    "sessionNodeItemComponent",
    session,
    "SessionNodeItem"
);

fn type_error(owner: &str, key: &str, expected: &str) -> JsValue {
    js_sys::TypeError::new(&format!("{owner} {key} must be {expected}")).into()
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| type_error(owner, key, "a string"))
}

fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required(value, key, owner)?
        .as_bool()
        .ok_or_else(|| type_error(owner, key, "a boolean"))
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required(value, key, owner)?
        .as_f64()
        .ok_or_else(|| type_error(owner, key, "a number"))
}

fn translated_string(
    translate: &Function,
    key: &str,
    variables: Option<&Object>,
) -> Result<String, JsValue> {
    translated(translate, key, variables)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("Workspace translator must return a string").into())
}

fn icon(modules: &BrowserModules, name: &str, props: Option<&Object>) -> Result<JsValue, JsValue> {
    element(&modules.react, &modules.primitive(name)?, props, &[])
}

fn stop_propagation(event: &JsValue) -> Result<(), JsValue> {
    call(event, "stopPropagation", &[]).map(|_| ())
}

fn event_data_transfer(event: &JsValue) -> Result<JsValue, JsValue> {
    required(event, "dataTransfer", "drag event")
}

fn row_half(event: &JsValue) -> Result<&'static str, JsValue> {
    let target = required(event, "currentTarget", "drag event")?;
    let rect = call(&target, "getBoundingClientRect", &[])?;
    let client_y = required_number(event, "clientY", "drag event")?;
    let top = required_number(&rect, "top", "row rectangle")?;
    let height = required_number(&rect, "height", "row rectangle")?;
    Ok(if client_y < top + height / 2.0 {
        "before"
    } else {
        "after"
    })
}

fn set_drag_payload(event: &JsValue, identity: &str) -> Result<(), JsValue> {
    let transfer = event_data_transfer(event)?;
    Reflect::set(
        &transfer,
        &JsValue::from_str("effectAllowed"),
        &JsValue::from_str("move"),
    )?;
    call(
        &transfer,
        "setData",
        &[JsValue::from_str("text/plain"), JsValue::from_str(identity)],
    )?;
    Ok(())
}

fn js_string(value: &JsValue) -> Result<String, JsValue> {
    function(&js_sys::global(), "String", "globalThis")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() must return a string").into())
}

fn pad_two(value: &JsValue) -> Result<String, JsValue> {
    let text = JsValue::from_str(&js_string(value)?);
    let boxed =
        function(&js_sys::global(), "Object", "globalThis")?.call1(&JsValue::UNDEFINED, &text)?;
    call(
        &boxed,
        "padStart",
        &[JsValue::from_f64(2.0), JsValue::from_str("0")],
    )?
    .as_string()
    .ok_or_else(|| js_sys::TypeError::new("String.padStart() must return a string").into())
}

fn created_label(created_at: f64, translate: &Function) -> Result<String, JsValue> {
    let date: JsValue = js_sys::Date::new(&JsValue::from_f64(created_at)).into();
    let year = call(&date, "getFullYear", &[])?;
    let month = JsValue::from_f64(call(&date, "getMonth", &[])?.as_f64().unwrap_or(f64::NAN) + 1.0);
    let day = call(&date, "getDate", &[])?;
    let hour = call(&date, "getHours", &[])?;
    let minute = call(&date, "getMinutes", &[])?;
    let date_variables = object(&[("y", year), ("m", month), ("d", day)])?;
    let date_label = translated_string(translate, "date.ymd", Some(&date_variables))?;
    let time = format!("{date_label} {}:{}", pad_two(&hour)?, pad_two(&minute)?);
    translated_string(
        translate,
        "hover.created",
        Some(&object(&[("time", JsValue::from_str(&time))])?),
    )
}

fn time_label(
    updated_at: f64,
    now: f64,
    translate: &Function,
    hover: bool,
) -> Result<String, JsValue> {
    #[allow(clippy::cast_possible_truncation)]
    let relative = relative_time(updated_at as i64, now as i64);
    if relative.unit == RelativeTimeUnit::Now {
        return translated_string(translate, "time.now", None);
    }
    let unit = match relative.unit {
        RelativeTimeUnit::Now => unreachable!("the now bucket returned above"),
        RelativeTimeUnit::Minutes => "minutes",
        RelativeTimeUnit::Hours => "hours",
        RelativeTimeUnit::Days => "days",
        RelativeTimeUnit::Months => "months",
        RelativeTimeUnit::Years => "years",
    };
    #[allow(clippy::cast_precision_loss)]
    let compact = translated_string(
        translate,
        &format!("time.{unit}"),
        Some(&object(&[("n", JsValue::from_f64(relative.n as f64))])?),
    )?;
    if hover {
        translated_string(
            translate,
            "time.ago",
            Some(&object(&[("t", JsValue::from_str(&compact))])?),
        )
    } else {
        Ok(compact)
    }
}

fn display_title(node: &JsValue, translate: &Function) -> Result<String, JsValue> {
    if required_bool(node, "blank", "Session node")? {
        translated_string(translate, "session.new", None)
    } else {
        required_string(node, "title", "Session node")
    }
}

#[derive(Clone)]
struct SessionStatus {
    state: &'static str,
    label: String,
}

fn session_statuses(node: &JsValue, translate: &Function) -> Result<Vec<SessionStatus>, JsValue> {
    let count = required_number(node, "runningSubagentCount", "Session node")?;
    let subagents = if count == 0.0 {
        None
    } else {
        Some(SessionStatus {
            state: "ongoing",
            label: translated_string(
                translate,
                if count.to_bits() == 1.0_f64.to_bits() {
                    "status.subagentsRunning.one"
                } else {
                    "status.subagentsRunning.other"
                },
                Some(&object(&[("n", JsValue::from_f64(count))])?),
            )?,
        })
    };
    let pending_value = Reflect::get(node, &JsValue::from_str("pendingInteraction"))?;
    let pending = if pending_value.is_undefined() {
        None
    } else {
        let (key, state) = match pending_value.as_string().as_deref() {
            Some("approval") => ("status.waitingApproval", "warning"),
            Some("plan-review") => ("status.planReview", "warning"),
            Some("question") => ("status.waitingAnswer", "warning"),
            _ => {
                return Err(js_sys::Error::new(&format!(
                    "unknown pending interaction: {}",
                    js_string(&pending_value)?
                ))
                .into());
            }
        };
        Some(SessionStatus {
            state,
            label: translated_string(translate, key, None)?,
        })
    };
    if let Some(pending) = pending {
        let mut statuses = vec![pending];
        statuses.extend(subagents);
        return Ok(statuses);
    }
    if required_bool(node, "running", "Session node")? {
        let mut statuses = vec![SessionStatus {
            state: "ongoing",
            label: translated_string(translate, "status.running", None)?,
        }];
        statuses.extend(subagents);
        return Ok(statuses);
    }
    if let Some(subagents) = subagents {
        return Ok(vec![subagents]);
    }
    Ok(vec![SessionStatus {
        state: "done",
        label: translated_string(
            translate,
            if required_bool(node, "completed", "Session node")? {
                "status.completed"
            } else {
                "status.idle"
            },
            None,
        )?,
    }])
}

fn status_dot(modules: &BrowserModules, status: &SessionStatus) -> Result<JsValue, JsValue> {
    element(
        &modules.react,
        &modules.primitive("StateDot")?,
        Some(&object(&[("state", JsValue::from_str(status.state))])?),
        &[],
    )
}

fn status_dots(modules: &BrowserModules, statuses: &[SessionStatus]) -> Result<JsValue, JsValue> {
    let mut children = vec![status_dot(modules, &statuses[0])?];
    for status in statuses {
        children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("visuallyHidden", true)])),
                ),
                ("key", JsValue::from_str(&status.label)),
            ])?),
            &[JsValue::from_str(&status.label)],
        )?);
    }
    element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &children,
    )
}

fn workspace_hover_content(
    modules: &BrowserModules,
    label: &str,
    cwd: JsValue,
    created_at: f64,
    translate: &Function,
) -> Result<JsValue, JsValue> {
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("hoverContent", true)])),
        )])?),
        &[
            tag(
                &modules.react,
                "div",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("hoverTitle", true)])),
                )])?),
                &[JsValue::from_str(label)],
            )?,
            tag(
                &modules.react,
                "div",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("hoverPath", true)])),
                )])?),
                &[cwd],
            )?,
            tag(
                &modules.react,
                "div",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("hoverTime", true)])),
                )])?),
                &[JsValue::from_str(&created_label(created_at, translate)?)],
            )?,
        ],
    )
}

#[allow(clippy::too_many_lines)] // The source row owns one DOM cell, its menu, drag lifecycle, and hover card.
fn render_project_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let group = required(props, "group", "ProjectRowItem props")?;
    let translate = function(props, "t", "ProjectRowItem props")?;
    let workspace_id = Reflect::get(&group, &JsValue::from_str("workspaceId"))?;
    let label = if workspace_id.is_undefined() {
        translated_string(&translate, "group.ungrouped", None)?
    } else {
        required_string(&group, "label", "Workspace group")?
    };
    let expanded = required_bool(&group, "expanded", "Workspace group")?;
    let active = expanded && required_bool(&group, "containsCurrent", "Workspace group")?;
    let key = required_string(&group, "key", "Workspace group")?;
    let (menu_open, set_menu_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let menu_open = menu_open.as_bool().unwrap_or(false);
    let actions = Reflect::get(props, &JsValue::from_str("actions"))?;
    let has_actions = !actions.is_undefined();
    let drag = Reflect::get(props, &JsValue::from_str("drag"))?;
    let has_drag = !drag.is_undefined();

    let workspace_menu_items = Array::new();
    for (id, translation, icon_name, danger) in [
        ("rename", "rename", "IconEditOutline16", false),
        ("delete", "delete.workspace", "IconTrashOutline16", true),
    ] {
        let mut fields = vec![
            ("id", JsValue::from_str(id)),
            (
                "label",
                JsValue::from_str(&translated_string(&translate, translation, None)?),
            ),
            ("icon", icon(modules, icon_name, None)?),
        ];
        if danger {
            fields.push(("danger", JsValue::TRUE));
        }
        let item: JsValue = object(&fields)?.into();
        workspace_menu_items.push(&item);
    }

    let menu = if has_actions {
        let close_setter = set_menu_open.clone();
        let close_menu = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let select_setter = set_menu_open.clone();
        let select_actions = actions.clone();
        let select_menu = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
            select_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            match id.as_str() {
                "rename" => {
                    function(&select_actions, "rename", "Workspace actions")?
                        .call0(&JsValue::UNDEFINED)?;
                }
                "delete" => {
                    function(&select_actions, "delete", "Workspace actions")?
                        .call0(&JsValue::UNDEFINED)?;
                }
                _ => {}
            }
            Ok(())
        })
            as Box<dyn FnMut(String) -> Result<(), JsValue>>);
        let anchor_setter = set_menu_open.clone();
        let toggle_menu = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            stop_propagation(&event)?;
            anchor_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!menu_open))?;
            Ok(())
        })
            as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let menu_anchor = tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class(&[("iconButton", true)])),
                ),
                (
                    "aria-label",
                    JsValue::from_str(&translated_string(
                        &translate,
                        "actions.workspace.aria",
                        Some(&object(&[("name", JsValue::from_str(&label))])?),
                    )?),
                ),
                ("onClick", toggle_menu.into_js_value()),
            ])?),
            &[icon(modules, "IconEllipsisOutline16", None)?],
        )?;
        Some(element(
            &modules.react,
            &modules.primitive("Menu")?,
            Some(&object(&[
                ("open", JsValue::from_bool(menu_open)),
                ("onClose", close_menu.into_js_value()),
                ("items", workspace_menu_items.into()),
                ("onSelect", select_menu.into_js_value()),
                ("portal", JsValue::TRUE),
                ("closeOnPointerLeave", JsValue::TRUE),
                ("anchor", menu_anchor),
            ])?),
            &[],
        )?)
    } else {
        None
    };

    let create = function(props, "onCreate", "ProjectRowItem props")?;
    let create_click = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        stop_propagation(&event)?;
        create.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let create_button = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class(&[("iconButton", true)])),
            ),
            (
                "aria-label",
                JsValue::from_str(&translated_string(
                    &translate,
                    "actions.newSession.aria",
                    Some(&object(&[("name", JsValue::from_str(&label))])?),
                )?),
            ),
            ("onClick", create_click.into_js_value()),
        ])?),
        &[icon(modules, "IconPlusOutline16", None)?],
    )?;

    let toggle = function(props, "onToggle", "ProjectRowItem props")?;
    let row_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        toggle.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let drag_start = if has_drag {
        let drag = drag.clone();
        let identity = key.clone();
        Some(
            Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                set_drag_payload(&event, &identity)?;
                function(&drag, "start", "Workspace row drag")?.call0(&JsValue::UNDEFINED)?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
            .into_js_value(),
        )
    } else {
        None
    };
    let drag_end = if has_drag {
        function(&drag, "end", "Workspace row drag")?.into()
    } else {
        JsValue::UNDEFINED
    };
    let folder = icon(
        modules,
        if expanded {
            "IconFolderOpen16"
        } else {
            "IconFolderClose16"
        },
        None,
    )?;
    let arrow = icon(
        modules,
        "IconTriangleRightFill14",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("arrow", true), ("arrowOpen", expanded)])),
        )])?),
    )?;
    let mut action_children = Vec::new();
    if let Some(menu) = menu {
        action_children.push(menu);
    }
    action_children.push(create_button);
    let own_row = tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class(&[("projectRow", true), ("menuOpen", menu_open)])),
            ),
            ("role", JsValue::from_str("treeitem")),
            ("aria-expanded", JsValue::from_bool(expanded)),
            ("onClick", row_click.into_js_value()),
            ("draggable", JsValue::from_bool(has_drag)),
            ("onDragStart", drag_start.unwrap_or(JsValue::UNDEFINED)),
            ("onDragEnd", drag_end),
        ])?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[
                        ("slot", true),
                        ("folder", true),
                        ("folderActive", active),
                    ])),
                )])?),
                &[folder],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("slot", true), ("chevron", true)])),
                )])?),
                &[arrow],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("projectText", true)])),
                )])?),
                &[tag(
                    &modules.react,
                    "span",
                    Some(&object(&[(
                        "className",
                        JsValue::from_str(&class(&[("title", true)])),
                    )])?),
                    &[JsValue::from_str(&label)],
                )?],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("rowActions", true)])),
                )])?),
                &action_children,
            )?,
        ],
    )?;

    let created_at = Reflect::get(&group, &JsValue::from_str("createdAt"))?;
    if created_at.is_undefined() {
        return Ok(own_row);
    }
    let created_at = created_at
        .as_f64()
        .ok_or_else(|| type_error("Workspace group", "createdAt", "a number"))?;
    let cwd = Reflect::get(&group, &JsValue::from_str("cwd"))?;
    let hover = workspace_hover_content(
        modules,
        &required_string(&group, "label", "Workspace group")?,
        cwd.clone(),
        created_at,
        &translate,
    )?;
    element(
        &modules.react,
        &modules.primitive("HoverCard")?,
        Some(&object(&[
            ("anchor", own_row),
            ("content", hover),
            ("disabled", JsValue::from_bool(menu_open)),
            ("copyText", cwd),
            (
                "copyLabel",
                JsValue::from_str(&translated_string(&translate, "copy", None)?),
            ),
            (
                "copiedLabel",
                JsValue::from_str(&translated_string(&translate, "hover.copied", None)?),
            ),
        ])?),
        &[],
    )
}

fn render_search_result(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let result = required(props, "result", "SearchResultItem props")?;
    let translate = function(props, "t", "SearchResultItem props")?;
    let id = required_string(&result, "id", "Search result")?;
    let current = Reflect::get(props, &JsValue::from_str("currentId"))?;
    let selected = current.as_string().as_deref() == Some(id.as_str());
    let statuses = session_statuses(&result, &translate)?;
    let show_status =
        statuses[0].state != "done" || required_bool(&result, "completed", "Search result")?;
    let open = function(props, "onOpen", "SearchResultItem props")?;
    let open_id = id.clone();
    let on_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        open.call1(&JsValue::UNDEFINED, &JsValue::from_str(&open_id))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let status = if show_status {
        status_dots(modules, &statuses)?
    } else {
        JsValue::UNDEFINED
    };
    let heading = tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("searchResultHeading", true)])),
        )])?),
        &[
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("slot", true)])),
                )])?),
                &[status],
            )?,
            tag(
                &modules.react,
                "span",
                Some(&object(&[(
                    "className",
                    JsValue::from_str(&class(&[("searchResultTitle", true)])),
                )])?),
                &[required(&result, "title", "Search result")?],
            )?,
        ],
    )?;
    let mut meta = vec![tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("searchResultWorkspace", true)])),
        )])?),
        &[required(&result, "workspace", "Search result")?],
    )?];
    let snippet = Reflect::get(&result, &JsValue::from_str("snippet"))?;
    if !snippet.is_undefined() {
        meta.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("searchResultSnippet", true)])),
            )])?),
            &[snippet],
        )?);
    }
    let meta = tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("searchResultMeta", true)])),
        )])?),
        &meta,
    )?;
    tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class(&[("searchResultRow", true), ("selected", selected)])),
            ),
            ("role", JsValue::from_str("treeitem")),
            ("aria-selected", JsValue::from_bool(selected)),
            ("onClick", on_click.into_js_value()),
        ])?),
        &[heading, meta],
    )
}

fn session_hover_content(
    modules: &BrowserModules,
    node: &JsValue,
    title: &str,
    now: f64,
    translate: &Function,
    statuses: &[SessionStatus],
) -> Result<JsValue, JsValue> {
    let blank = required_bool(node, "blank", "Session node")?;
    let mut children = vec![tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("hoverTitle", true)])),
        )])?),
        &[JsValue::from_str(title)],
    )?];
    if !blank {
        children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("hoverTime", true)])),
            )])?),
            &[JsValue::from_str(&time_label(
                required_number(node, "updatedAt", "Session node")?,
                now,
                translate,
                true,
            )?)],
        )?);
    }
    for status in statuses {
        children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("hoverStatus", true)])),
                ),
                ("key", JsValue::from_str(&status.label)),
            ])?),
            &[
                status_dot(modules, status)?,
                tag(
                    &modules.react,
                    "span",
                    None,
                    &[JsValue::from_str(&status.label)],
                )?,
            ],
        )?);
    }
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("hoverContent", true)])),
        )])?),
        &children,
    )
}

#[allow(clippy::too_many_lines)] // The source row owns one DOM cell, its menu, drag lifecycle, and hover card.
fn render_session_row(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let node = required(props, "node", "SessionNodeItem props")?;
    let translate = function(props, "t", "SessionNodeItem props")?;
    let id = required_string(&node, "id", "Session node")?;
    let title = display_title(&node, &translate)?;
    let row_title = required_string(&node, "title", "Session node")?;
    let blank = required_bool(&node, "blank", "Session node")?;
    let current = Reflect::get(props, &JsValue::from_str("currentId"))?;
    let selected = current.as_string().as_deref() == Some(id.as_str());
    let statuses = session_statuses(&node, &translate)?;
    let show_status =
        statuses[0].state != "done" || required_bool(&node, "completed", "Session node")?;
    let flat = Reflect::get(props, &JsValue::from_str("flat"))?
        .as_bool()
        .unwrap_or(false);
    let now = required_number(props, "now", "SessionNodeItem props")?;
    let (menu_open, set_menu_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let menu_open = menu_open.as_bool().unwrap_or(false);
    let drag = Reflect::get(props, &JsValue::from_str("drag"))?;
    let has_drag = !drag.is_undefined();
    let drag_active =
        has_drag && Reflect::get(&drag, &JsValue::from_str("active"))?.as_bool() == Some(true);
    let marker = if has_drag {
        Reflect::get(&drag, &JsValue::from_str("marker"))?.as_string()
    } else {
        None
    };

    let session_menu_items = Array::new();
    for (id, translation, icon_name, size) in [
        ("rename", "rename", "IconEditOutline16", None),
        ("fork", "menu.fork", "IconBranchOutline16", None),
        (
            "archive",
            "menu.archiveSession",
            "IconArchiveOutline20",
            Some(16.0),
        ),
    ] {
        let icon_props = size
            .map(|size| object(&[("size", JsValue::from_f64(size))]))
            .transpose()?;
        let item: JsValue = object(&[
            ("id", JsValue::from_str(id)),
            (
                "label",
                JsValue::from_str(&translated_string(&translate, translation, None)?),
            ),
            ("icon", icon(modules, icon_name, icon_props.as_ref())?),
        ])?
        .into();
        session_menu_items.push(&item);
    }

    let close_setter = set_menu_open.clone();
    let close_menu = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let select_setter = set_menu_open.clone();
    let rename = function(props, "onRename", "SessionNodeItem props")?;
    let fork = function(props, "onFork", "SessionNodeItem props")?;
    let archive = function(props, "onArchive", "SessionNodeItem props")?;
    let select_id = id.clone();
    let select_title = row_title.clone();
    let select_menu = Closure::wrap(Box::new(move |action: String| -> Result<(), JsValue> {
        select_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        match action.as_str() {
            "rename" => {
                rename.call2(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(&select_id),
                    &JsValue::from_str(&select_title),
                )?;
            }
            "fork" => {
                fork.call1(&JsValue::UNDEFINED, &JsValue::from_str(&select_id))?;
            }
            "archive" => {
                archive.call1(&JsValue::UNDEFINED, &JsValue::from_str(&select_id))?;
            }
            _ => {}
        }
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let anchor_setter = set_menu_open.clone();
    let toggle_menu = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        stop_propagation(&event)?;
        anchor_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!menu_open))?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let menu_anchor = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class(&[("iconButton", true)])),
            ),
            (
                "aria-label",
                JsValue::from_str(&translated_string(
                    &translate,
                    "actions.session.aria",
                    Some(&object(&[("name", JsValue::from_str(&title))])?),
                )?),
            ),
            ("onClick", toggle_menu.into_js_value()),
        ])?),
        &[icon(modules, "IconEllipsisOutline16", None)?],
    )?;
    let menu = element(
        &modules.react,
        &modules.primitive("Menu")?,
        Some(&object(&[
            ("open", JsValue::from_bool(menu_open)),
            ("onClose", close_menu.into_js_value()),
            ("items", session_menu_items.into()),
            ("onSelect", select_menu.into_js_value()),
            ("portal", JsValue::TRUE),
            ("closeOnPointerLeave", JsValue::TRUE),
            ("anchor", menu_anchor),
        ])?),
        &[],
    )?;

    let open = function(props, "onOpen", "SessionNodeItem props")?;
    let open_id = id.clone();
    let row_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        open.call1(&JsValue::UNDEFINED, &JsValue::from_str(&open_id))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let drag_start = if has_drag {
        let drag = drag.clone();
        let identity = id.clone();
        Some(
            Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                set_drag_payload(&event, &identity)?;
                function(&drag, "start", "Session row drag")?.call0(&JsValue::UNDEFINED)?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
            .into_js_value(),
        )
    } else {
        None
    };
    let drag_end = if has_drag {
        function(&drag, "end", "Session row drag")?.into()
    } else {
        JsValue::UNDEFINED
    };
    let drag_over = if has_drag {
        let drag = drag.clone();
        Some(
            Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                if !required_bool(&drag, "active", "Session row drag")? {
                    return Ok(());
                }
                call(&event, "preventDefault", &[])?;
                Reflect::set(
                    &event_data_transfer(&event)?,
                    &JsValue::from_str("dropEffect"),
                    &JsValue::from_str("move"),
                )?;
                function(&drag, "hover", "Session row drag")?
                    .call1(&JsValue::UNDEFINED, &JsValue::from_str(row_half(&event)?))?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
            .into_js_value(),
        )
    } else {
        None
    };
    let drop = if has_drag {
        let drag = drag.clone();
        Some(
            Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                if !required_bool(&drag, "active", "Session row drag")? {
                    return Ok(());
                }
                call(&event, "preventDefault", &[])?;
                function(&drag, "drop", "Session row drag")?
                    .call1(&JsValue::UNDEFINED, &JsValue::from_str(row_half(&event)?))?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
            .into_js_value(),
        )
    } else {
        None
    };

    let mut row_children = Vec::new();
    if !flat || show_status {
        let status = if show_status {
            status_dots(modules, &statuses)?
        } else {
            JsValue::UNDEFINED
        };
        row_children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("slot", true)])),
            )])?),
            &[status],
        )?);
    }
    row_children.push(tag(
        &modules.react,
        "span",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("title", true)])),
        )])?),
        &[JsValue::from_str(&title)],
    )?);
    if !blank {
        row_children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("time", true)])),
            )])?),
            &[JsValue::from_str(&time_label(
                required_number(&node, "updatedAt", "Session node")?,
                now,
                &translate,
                false,
            )?)],
        )?);
        row_children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("rowActions", true)])),
            )])?),
            &[menu],
        )?);
    }
    let own_row = tag(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&class(&[
                    ("sessionRow", true),
                    ("selected", selected),
                    ("menuOpen", menu_open),
                    ("flatSessionRowWithoutStatus", flat && !show_status),
                    ("dropBefore", marker.as_deref() == Some("before")),
                    ("dropAfter", marker.as_deref() == Some("after")),
                ])),
            ),
            ("role", JsValue::from_str("treeitem")),
            ("aria-selected", JsValue::from_bool(selected)),
            ("onClick", row_click.into_js_value()),
            ("draggable", JsValue::from_bool(has_drag)),
            ("onDragStart", drag_start.unwrap_or(JsValue::UNDEFINED)),
            ("onDragEnd", drag_end),
            ("onDragOver", drag_over.unwrap_or(JsValue::UNDEFINED)),
            ("onDrop", drop.unwrap_or(JsValue::UNDEFINED)),
        ])?),
        &row_children,
    )?;
    let hover = session_hover_content(modules, &node, &title, now, &translate, &statuses)?;
    element(
        &modules.react,
        &modules.primitive("HoverCard")?,
        Some(&object(&[
            ("anchor", own_row),
            ("content", hover),
            ("disabled", JsValue::from_bool(menu_open || drag_active)),
            (
                "copyText",
                if blank {
                    JsValue::UNDEFINED
                } else {
                    JsValue::from_str(&row_title)
                },
            ),
            (
                "copyLabel",
                JsValue::from_str(&translated_string(&translate, "copy", None)?),
            ),
            (
                "copiedLabel",
                JsValue::from_str(&translated_string(&translate, "hover.copied", None)?),
            ),
        ])?),
        &[],
    )
}
