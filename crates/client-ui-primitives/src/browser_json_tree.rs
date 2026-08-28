//! Compiled read-only JSON inspector with recursive previews, roving focus, and copy actions.

use std::{cell::RefCell, collections::BTreeSet, fmt::Write as _};

use js_sys::{Array, Function, Object, Promise, Reflect, RegExp};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
    ICON_DEFINITIONS, browser_icons::render_icon, configure_client_ui_primitive_menu,
    menu_component,
};

const JSON_TREE_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/JsonTree.module.css");
const OBJECT_PREVIEW_LIMIT: usize = 4;
const ARRAY_PREVIEW_LIMIT: usize = 5;
const PREVIEW_DEPTH_LIMIT: usize = 2;
const COPY_RESET_MS: f64 = 1_500.0;

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    menu: JsValue,
}

#[derive(Clone, Debug)]
enum PathPart {
    Index(usize),
    Field(String),
}

type JsonPath = Vec<PathPart>;

#[derive(Clone)]
struct Labels {
    copy_value: String,
    copy_json: String,
    copy_path: String,
    copy_pretty_json: String,
    copy_compact_json: String,
    copied: String,
    copy_failed: String,
    collapse_node: String,
    expand_node: String,
    copy_button_title: Option<Function>,
}

#[derive(Clone)]
struct CopyContext {
    copyable: bool,
    root_ref: JsValue,
    active_row_ref: JsValue,
    copy_menu_open_ref: JsValue,
    set_copy_target: Function,
    set_copy_state: Function,
    set_copy_menu_open: Function,
}

#[derive(Clone)]
struct NodeContext {
    modules: BrowserModules,
    labels: Labels,
    expanded: BTreeSet<String>,
    set_expanded: Function,
    tab_stop_id: Option<String>,
    set_tab_stop_id: Function,
    copy: CopyContext,
}

/// Configures React/ReactDOM, the composed `Menu`, and the `JsonTree` stylesheet.
///
/// # Errors
///
/// Returns Menu or DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveJsonTree)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_json_tree(
    react: JsValue,
    react_dom: JsValue,
) -> Result<(), JsValue> {
    configure_client_ui_primitive_menu(react.clone(), react_dom)?;
    let menu = menu_component()?;
    MODULES.with(|modules| *modules.borrow_mut() = Some(BrowserModules { react, menu }));
    inject_style()
}

/// Returns the compiled `JsonTree` component.
///
/// # Errors
///
/// Returns before the browser modules are configured.
#[wasm_bindgen(js_name = jsonTreeComponent)]
pub fn json_tree_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_json_tree(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

#[allow(clippy::too_many_lines)]
fn render_json_tree(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let data = required_property(props, "data", "JsonTree props")?;
    let label = optional_string(props, "label")?.unwrap_or_else(|| "JSON".to_owned());
    let class_name = optional_string(props, "className")?;
    let copyable = optional_bool(props, "copyable")?.unwrap_or(true);
    let expand_top_level = optional_bool(props, "expandTopLevel")?.unwrap_or(true);
    let labels = labels(props)?;
    let root_entries = entries_of(&data)?;
    let initial_tab_stop = initial_tab_stop(&data, &root_entries, expand_top_level)?;

    let root_ref = use_ref(react, &JsValue::NULL)?;
    let active_row_ref = use_ref(react, &JsValue::UNDEFINED)?;
    let copy_button_ref = use_ref(react, &JsValue::NULL)?;
    let copy_menu_open_ref = use_ref(react, &JsValue::FALSE)?;
    let reset_timer_ref = use_ref(react, &JsValue::UNDEFINED)?;
    let expand_top_level_ref = use_ref(react, &JsValue::from_bool(expand_top_level))?;
    let (copy_target, set_copy_target) = use_state(react, &JsValue::UNDEFINED)?;
    let (copy_state, set_copy_state) = use_state(react, &JsValue::from_str("idle"))?;
    let (copy_menu_open, set_copy_menu_open) = use_state(react, &JsValue::FALSE)?;
    let (tab_stop_value, set_tab_stop_id) = use_state(
        react,
        &initial_tab_stop
            .as_ref()
            .map_or(JsValue::NULL, |value| JsValue::from_str(value)),
    )?;
    let tab_stop_id = tab_stop_value.as_string();
    let initial_expanded = if expand_top_level {
        Array::new()
    } else {
        Array::of1(&JsValue::from_str(""))
    };
    let (expanded_value, set_expanded) = use_state(react, initial_expanded.as_ref())?;
    let expanded = Array::from(&expanded_value)
        .iter()
        .filter_map(|value| value.as_string())
        .collect::<BTreeSet<_>>();

    let copy_context = CopyContext {
        copyable,
        root_ref: root_ref.clone(),
        active_row_ref: active_row_ref.clone(),
        copy_menu_open_ref: copy_menu_open_ref.clone(),
        set_copy_target: set_copy_target.clone(),
        set_copy_state: set_copy_state.clone(),
        set_copy_menu_open: set_copy_menu_open.clone(),
    };
    install_cleanup_effect(react, &reset_timer_ref, &active_row_ref)?;
    install_data_effect(
        react,
        DataEffectInputs {
            data: data.clone(),
            expand_top_level,
            expand_top_level_ref,
            initial_tab_stop: initial_tab_stop.clone(),
            active_row_ref: active_row_ref.clone(),
            copy_menu_open_ref: copy_menu_open_ref.clone(),
            set_copy_target: set_copy_target.clone(),
            set_copy_state: set_copy_state.clone(),
            set_copy_menu_open: set_copy_menu_open.clone(),
            set_tab_stop_id: set_tab_stop_id.clone(),
            set_expanded: set_expanded.clone(),
        },
    )?;
    install_reposition_effect(react, &copy_context)?;

    let node_context = NodeContext {
        modules: modules.clone(),
        labels: labels.clone(),
        expanded,
        set_expanded,
        tab_stop_id,
        set_tab_stop_id,
        copy: copy_context.clone(),
    };

    let (root_open, root_close) = bracket_of(&data);
    let content = if expand_top_level {
        let root_hover = row_hover_handler(&copy_context, Vec::new(), data.clone());
        let opening = create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(
                        "seekdeep-primitive-json-tree-row seekdeep-primitive-json-tree-topLevelBracket",
                    ),
                ),
                ("data-json-root-row", JsValue::TRUE),
                ("onMouseOver", root_hover),
            ])?),
            &[styled_text(react, "punctuation", root_open)?],
        )?;
        let mut rows = Vec::new();
        for (index, (key, value)) in root_entries.iter().enumerate() {
            let path = vec![if Array::is_array(&data) {
                PathPart::Index(index)
            } else {
                PathPart::Field(key.clone())
            }];
            rows.push(render_node(
                Some(key),
                value,
                &path,
                index + 1 == root_entries.len(),
                &node_context,
            )?);
        }
        let tree = create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("aria-label", JsValue::from_str(&label)),
                (
                    "className",
                    JsValue::from_str(
                        "seekdeep-primitive-json-tree-container seekdeep-primitive-json-tree-expandedTopLevelContainer",
                    ),
                ),
                ("role", JsValue::from_str("tree")),
            ])?),
            &rows,
        )?;
        let closing = create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[(
                "className",
                JsValue::from_str(
                    "seekdeep-primitive-json-tree-row seekdeep-primitive-json-tree-topLevelBracket",
                ),
            )])?),
            &[styled_text(react, "punctuation", root_close)?],
        )?;
        create_element(
            react,
            &JsValue::from_str("div"),
            Some(&class_props(
                "seekdeep-primitive-json-tree-expandedTopLevel",
            )?),
            &[opening, tree, closing],
        )?
    } else {
        let root = render_node(None, &data, &[], true, &node_context)?;
        create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[
                ("aria-label", JsValue::from_str(&label)),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-json-tree-container"),
                ),
                ("role", JsValue::from_str("tree")),
            ])?),
            &[root],
        )?
    };

    let mut root_children = vec![content];
    if !copy_target.is_undefined() {
        root_children.push(render_copy_control(
            modules,
            &copy_target,
            &copy_state,
            copy_menu_open.as_bool().unwrap_or(false),
            &labels,
            &copy_context,
            &copy_button_ref,
            &reset_timer_ref,
        )?);
    }

    let root_over_context = copy_context.clone();
    let root_over = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if !root_over_context.copyable
            || current(&root_over_context.copy_menu_open_ref)?.as_bool() == Some(true)
        {
            return Ok(());
        }
        let target = required_property(&event, "target", "mouse event")?;
        if !target.is_instance_of::<web_sys::Element>() {
            return Ok(());
        }
        if call_method(
            &target,
            "closest",
            &[JsValue::from_str("[data-json-copy-button]")],
        )?
        .is_null()
        {
            clear_copy_target(&root_over_context)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let leave_context = copy_context.clone();
    let mouse_leave = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if current(&leave_context.copy_menu_open_ref)?.as_bool() != Some(true) {
            clear_copy_target(&leave_context)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let scroll_context = copy_context;
    let scroll = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let row = current(&scroll_context.active_row_ref)?;
        if !row.is_undefined() {
            reposition_copy_button(&scroll_context, &row)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let mut classes = vec!["seekdeep-primitive-json-tree-root".to_owned()];
    if let Some(class_name) = class_name {
        classes.push(class_name);
    }
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", root_ref),
            ("className", JsValue::from_str(&classes.join(" "))),
            ("onMouseOver", root_over.into_js_value()),
            ("onMouseLeave", mouse_leave.into_js_value()),
            ("onScroll", scroll.into_js_value()),
        ])?),
        &root_children,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_node(
    field: Option<&str>,
    value: &JsValue,
    path: &[PathPart],
    last_element: bool,
    context: &NodeContext,
) -> Result<JsValue, JsValue> {
    let react = &context.modules.react;
    let container = is_expandable_value(value);
    let entries = if container {
        entries_of(value)?
    } else {
        Vec::new()
    };
    let expandable = !entries.is_empty();
    let node_id = path_id(path);
    let expanded = expandable && context.expanded.contains(&node_id);
    let row_hover = row_hover_handler(&context.copy, path.to_vec(), value.clone());
    let mut row_children = Vec::new();

    if !container {
        if let Some(field) = field_node(react, field, false, None)? {
            row_children.push(field);
        }
        row_children.push(primitive_value(react, value)?);
        if !last_element {
            row_children.push(styled_text(react, "punctuation", ",")?);
        }
        return row(react, &node_id, &row_children, None, row_hover);
    }

    let (open, close) = bracket_of(value);
    if !expandable {
        if let Some(field) = field_node(react, field, false, None)? {
            row_children.push(field);
        }
        row_children.push(styled_text(react, "punctuation", open)?);
        row_children.push(styled_text(react, "punctuation", close)?);
        if !last_element {
            row_children.push(styled_text(react, "punctuation", ",")?);
        }
        return row(react, &node_id, &row_children, None, row_hover);
    }

    let expander_ref = object(&[("current", JsValue::NULL)])?;
    let toggle = toggle_handler(
        context,
        node_id.clone(),
        expanded,
        expander_ref.clone().into(),
    );
    let key_context = context.clone();
    let key_id = node_id.clone();
    let key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        let key = required_string(&event, "key", "keyboard event")?;
        if key == "ArrowRight" || key == "ArrowLeft" {
            call_method(&event, "preventDefault", &[])?;
            set_expanded(&key_context, &key_id, key == "ArrowRight")?;
        } else if key == "ArrowUp" || key == "ArrowDown" {
            call_method(&event, "preventDefault", &[])?;
            let current_target = required_property(&event, "currentTarget", "keyboard event")?;
            move_focus(&current_target, if key == "ArrowUp" { -1 } else { 1 })?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let focus_setter = context.set_tab_stop_id.clone();
    let focus_id = node_id.clone();
    let focus = Closure::wrap(Box::new(move || {
        let _ = set_state(&focus_setter, &JsValue::from_str(&focus_id));
    }) as Box<dyn FnMut()>);
    let expander_class = if expanded {
        "seekdeep-primitive-json-tree-expander seekdeep-primitive-json-tree-collapseIcon"
    } else {
        "seekdeep-primitive-json-tree-expander seekdeep-primitive-json-tree-expandIcon"
    };
    row_children.push(create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("ref", expander_ref.into()),
            ("key", JsValue::from_str(&format!("expander-{node_id}"))),
            ("className", JsValue::from_str(expander_class)),
            ("data-json-expander", JsValue::TRUE),
            ("role", JsValue::from_str("button")),
            (
                "aria-label",
                JsValue::from_str(if expanded {
                    &context.labels.collapse_node
                } else {
                    &context.labels.expand_node
                }),
            ),
            ("aria-expanded", JsValue::from_bool(expanded)),
            (
                "aria-controls",
                if expanded {
                    JsValue::from_str(&contents_id(&node_id))
                } else {
                    JsValue::UNDEFINED
                },
            ),
            (
                "tabIndex",
                JsValue::from_f64(if context.tab_stop_id.as_deref() == Some(&node_id) {
                    0.0
                } else {
                    -1.0
                }),
            ),
            ("onFocus", focus.into_js_value()),
            ("onClick", toggle.clone().into()),
            ("onKeyDown", key_down.into_js_value()),
        ])?),
        &[],
    )?);
    if let Some(field) = field_node(react, field, true, Some(toggle))? {
        row_children.push(field);
    }
    row_children.push(create_element(
        react,
        &JsValue::from_str("span"),
        Some(&class_props("seekdeep-primitive-json-tree-preview")?),
        &[preview_value(react, value, 0)?],
    )?);
    if !last_element {
        row_children.push(styled_text(react, "punctuation", ",")?);
    }
    if expanded {
        let mut child_nodes = Vec::new();
        for (index, (key, child)) in entries.iter().enumerate() {
            let mut child_path = path.to_vec();
            child_path.push(if Array::is_array(value) {
                PathPart::Index(index)
            } else {
                PathPart::Field(key.clone())
            });
            child_nodes.push(render_node(
                Some(key),
                child,
                &child_path,
                index + 1 == entries.len(),
                context,
            )?);
        }
        row_children.push(create_element(
            react,
            &JsValue::from_str("ul"),
            Some(&object(&[
                ("id", JsValue::from_str(&contents_id(&node_id))),
                ("role", JsValue::from_str("group")),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-json-tree-children"),
                ),
            ])?),
            &child_nodes,
        )?);
    }
    row(react, &node_id, &row_children, Some(expanded), row_hover)
}

fn row(
    react: &JsValue,
    node_id: &str,
    children: &[JsValue],
    aria_expanded: Option<bool>,
    on_mouse_over: JsValue,
) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("key", JsValue::from_str(node_id)),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-json-tree-row"),
            ),
            ("role", JsValue::from_str("treeitem")),
            (
                "aria-expanded",
                aria_expanded.map_or(JsValue::UNDEFINED, JsValue::from_bool),
            ),
            ("onMouseOver", on_mouse_over),
        ])?),
        children,
    )
}

fn field_node(
    react: &JsValue,
    field: Option<&str>,
    expandable: bool,
    on_toggle: Option<Function>,
) -> Result<Option<JsValue>, JsValue> {
    let Some(field) = field else {
        return Ok(None);
    };
    let label = if field.is_empty() { "\"\"" } else { field };
    let class_name = if expandable {
        "seekdeep-primitive-json-tree-label seekdeep-primitive-json-tree-clickableLabel"
    } else {
        "seekdeep-primitive-json-tree-label"
    };
    Ok(Some(create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            ("className", JsValue::from_str(class_name)),
            ("onClick", on_toggle.map_or(JsValue::UNDEFINED, Into::into)),
        ])?),
        &[JsValue::from_str(&format!("{label}:"))],
    )?))
}

fn toggle_handler(
    context: &NodeContext,
    node_id: String,
    expanded: bool,
    expander_ref: JsValue,
) -> Function {
    let context = context.clone();
    Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        set_expanded(&context, &node_id, !expanded)?;
        let element = current(&expander_ref)?;
        if !element.is_null() {
            call_method(&element, "focus", &[])?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value()
    .unchecked_into()
}

fn set_expanded(context: &NodeContext, node_id: &str, value: bool) -> Result<(), JsValue> {
    let mut expanded = context.expanded.clone();
    if value {
        expanded.insert(node_id.to_owned());
    } else {
        expanded.remove(node_id);
    }
    let next = Array::new();
    for node in expanded {
        next.push(&JsValue::from_str(&node));
    }
    set_state(&context.set_expanded, next.as_ref())
}

fn move_focus(button: &JsValue, direction: i32) -> Result<(), JsValue> {
    let tree = call_method(button, "closest", &[JsValue::from_str("[role=\"tree\"]")])?;
    if tree.is_null() {
        return Ok(());
    }
    let expanders = Array::from(&call_method(
        &tree,
        "querySelectorAll",
        &[JsValue::from_str("[data-json-expander]")],
    )?);
    let length = expanders.length();
    if length == 0 {
        return Ok(());
    }
    let current = expanders
        .iter()
        .position(|candidate| Object::is(&candidate, button));
    let Some(current) = current else {
        return Ok(());
    };
    let length_i64 = i64::from(length);
    let current_i64 = i64::try_from(current).unwrap_or(0);
    let next = (current_i64 + i64::from(direction) + length_i64) % length_i64;
    let next = u32::try_from(next).unwrap_or(0);
    call_method(&expanders.get(next), "focus", &[])?;
    Ok(())
}

fn row_hover_handler(context: &CopyContext, path: JsonPath, value: JsValue) -> JsValue {
    let context = context.clone();
    Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call_method(&event, "stopPropagation", &[])?;
        let row = required_property(&event, "currentTarget", "mouse event")?;
        handle_row_hover(&context, &row, &path, &value)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>)
    .into_js_value()
}

fn handle_row_hover(
    context: &CopyContext,
    row: &JsValue,
    path: &JsonPath,
    value: &JsValue,
) -> Result<(), JsValue> {
    if !context.copyable || current(&context.copy_menu_open_ref)?.as_bool() == Some(true) {
        return Ok(());
    }
    if Object::is(&current(&context.active_row_ref)?, row) {
        return Ok(());
    }
    set_active_row(&context.active_row_ref, Some(row))?;
    set_state(&context.set_copy_state, &JsValue::from_str("idle"))?;
    set_current(&context.copy_menu_open_ref, &JsValue::FALSE)?;
    set_state(&context.set_copy_menu_open, &JsValue::FALSE)?;
    position_copy_button(context, row, path, value)
}

fn position_copy_button(
    context: &CopyContext,
    row: &JsValue,
    path: &JsonPath,
    value: &JsValue,
) -> Result<(), JsValue> {
    let position = copy_position(&context.root_ref, row)?;
    let target = Object::new();
    Reflect::set(&target, &JsValue::from_str("path"), &path_to_js(path))?;
    Reflect::set(&target, &JsValue::from_str("value"), value)?;
    for key in ["left", "side", "top"] {
        Reflect::set(
            &target,
            &JsValue::from_str(key),
            &required_property(&position, key, "copy position")?,
        )?;
    }
    set_state(&context.set_copy_target, target.as_ref())
}

fn reposition_copy_button(context: &CopyContext, row: &JsValue) -> Result<(), JsValue> {
    let position = copy_position(&context.root_ref, row)?;
    let updater = Closure::wrap(
        Box::new(move |current: JsValue| -> Result<JsValue, JsValue> {
            if current.is_undefined() {
                return Ok(current);
            }
            let next = Object::assign(&Object::new(), &Object::from(current));
            for key in ["left", "side", "top"] {
                Reflect::set(
                    &next,
                    &JsValue::from_str(key),
                    &required_property(&position, key, "copy position")?,
                )?;
            }
            Ok(next.into())
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    context
        .set_copy_target
        .call1(&JsValue::UNDEFINED, &updater.into_js_value())?;
    Ok(())
}

fn copy_position(root_ref: &JsValue, row: &JsValue) -> Result<Object, JsValue> {
    let root = current(root_ref)?;
    if root.is_null() {
        return Err(js_sys::Error::new("JsonTree root is not mounted").into());
    }
    let root_rect = call_method(&root, "getBoundingClientRect", &[])?;
    let row_rect = call_method(row, "getBoundingClientRect", &[])?;
    let root_left = required_number(&root_rect, "left", "JsonTree root DOMRect")?;
    let root_top = required_number(&root_rect, "top", "JsonTree root DOMRect")?;
    let row_top = required_number(&row_rect, "top", "JsonTree row DOMRect")?;
    let client_width = required_number(&root, "clientWidth", "JsonTree root")?;
    let client_height = required_number(&root, "clientHeight", "JsonTree root")?;
    object(&[
        ("left", JsValue::from_f64(root_left + client_width - 26.0)),
        (
            "side",
            JsValue::from_str(if row_top - root_top > client_height / 2.0 {
                "top"
            } else {
                "bottom"
            }),
        ),
        ("top", JsValue::from_f64(row_top)),
    ])
}

fn clear_copy_target(context: &CopyContext) -> Result<(), JsValue> {
    set_active_row(&context.active_row_ref, None)?;
    set_state(&context.set_copy_target, &JsValue::UNDEFINED)?;
    set_state(&context.set_copy_state, &JsValue::from_str("idle"))?;
    set_current(&context.copy_menu_open_ref, &JsValue::FALSE)?;
    set_state(&context.set_copy_menu_open, &JsValue::FALSE)
}

fn set_active_row(reference: &JsValue, row: Option<&JsValue>) -> Result<(), JsValue> {
    let current_row = current(reference)?;
    if !current_row.is_undefined() {
        call_method(
            &current_row,
            "removeAttribute",
            &[JsValue::from_str("data-json-copy-active")],
        )?;
    }
    let next = row.cloned().unwrap_or(JsValue::UNDEFINED);
    set_current(reference, &next)?;
    if let Some(row) = row {
        call_method(
            row,
            "setAttribute",
            &[
                JsValue::from_str("data-json-copy-active"),
                JsValue::from_str(""),
            ],
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_copy_control(
    modules: &BrowserModules,
    copy_target: &JsValue,
    copy_state: &JsValue,
    copy_menu_open: bool,
    labels: &Labels,
    context: &CopyContext,
    copy_button_ref: &JsValue,
    reset_timer_ref: &JsValue,
) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let value = Reflect::get(copy_target, &JsValue::from_str("value"))?;
    let target_is_object =
        value.js_typeof().as_string().as_deref() == Some("object") && !value.is_null();
    let state = copy_state.as_string().unwrap_or_else(|| "idle".to_owned());
    let default_mode = if target_is_object {
        "prettyJson"
    } else {
        "value"
    };
    let title = if state == "copied" {
        labels.copied.clone()
    } else if state == "failed" {
        labels.copy_failed.clone()
    } else if target_is_object {
        labels.copy_pretty_json.clone()
    } else {
        labels.copy_value.clone()
    };
    let button_title = labels
        .copy_button_title
        .as_ref()
        .map(|formatter| {
            formatter
                .call1(&JsValue::UNDEFINED, &JsValue::from_str(&title))
                .and_then(|value| {
                    value.as_string().ok_or_else(|| {
                        js_sys::TypeError::new("copyButtonTitle must return a string").into()
                    })
                })
        })
        .transpose()?
        .unwrap_or_else(|| format!("{title}; right-click for copy options"));
    let click_target = copy_target.clone();
    let click_mode = default_mode.to_owned();
    let click_context = context.clone();
    let click_timer = reset_timer_ref.clone();
    let click = Closure::wrap(Box::new(move || {
        begin_copy(&click_target, &click_mode, &click_context, &click_timer);
    }) as Box<dyn FnMut()>);
    let menu_ref = context.copy_menu_open_ref.clone();
    let menu_setter = context.set_copy_menu_open.clone();
    let context_menu = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call_method(&event, "preventDefault", &[])?;
        call_method(&event, "stopPropagation", &[])?;
        set_current(&menu_ref, &JsValue::TRUE)?;
        set_state(&menu_setter, &JsValue::TRUE)
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let icon = if state == "copied" {
        icon(react, "IconCheckOutline16", 12.0)?
    } else {
        icon(react, "IconCopyOutline16", 12.0)?
    };
    let button = create_element(
        react,
        &JsValue::from_str("button"),
        Some(&object(&[
            ("ref", copy_button_ref.clone()),
            ("key", JsValue::from_str("json-copy-button")),
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-json-tree-copyButton"),
            ),
            ("data-json-copy-button", JsValue::TRUE),
            ("data-state", JsValue::from_str(&state)),
            ("aria-label", JsValue::from_str(&title)),
            ("title", JsValue::from_str(&button_title)),
            ("onClick", click.into_js_value()),
            ("onContextMenu", context_menu.into_js_value()),
        ])?),
        &[icon],
    )?;
    let menu_items = if target_is_object {
        copy_menu_items(&[
            ("prettyJson", &labels.copy_pretty_json),
            ("json", &labels.copy_compact_json),
            ("path", &labels.copy_path),
        ])?
    } else {
        copy_menu_items(&[
            ("value", &labels.copy_value),
            ("json", &labels.copy_json),
            ("path", &labels.copy_path),
        ])?
    };
    let select_target = copy_target.clone();
    let select_context = context.clone();
    let select_timer = reset_timer_ref.clone();
    let select_ref = context.copy_menu_open_ref.clone();
    let select_setter = context.set_copy_menu_open.clone();
    let on_select = Closure::wrap(Box::new(move |mode: String| -> Result<(), JsValue> {
        begin_copy(&select_target, &mode, &select_context, &select_timer);
        set_current(&select_ref, &JsValue::FALSE)?;
        set_state(&select_setter, &JsValue::FALSE)
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let close_context = context.clone();
    let on_close = Closure::wrap(Box::new(move || clear_copy_target(&close_context))
        as Box<dyn FnMut() -> Result<(), JsValue>>);
    let anchor_ref = copy_button_ref.clone();
    let get_anchor_rect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let button = current(&anchor_ref)?;
        call_method(&button, "getBoundingClientRect", &[])
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let menu_props = object(&[
        ("open", JsValue::from_bool(copy_menu_open)),
        ("compact", JsValue::TRUE),
        ("portal", JsValue::TRUE),
        ("align", JsValue::from_str("end")),
        (
            "side",
            required_property(copy_target, "side", "copy target")?,
        ),
        ("anchor", button),
        ("items", menu_items.into()),
        ("onSelect", on_select.into_js_value()),
        ("onClose", on_close.into_js_value()),
        ("getAnchorRect", get_anchor_rect.into_js_value()),
    ])?;
    let menu = create_element(react, &modules.menu, Some(&menu_props), &[])?;
    create_element(
        react,
        &JsValue::from_str("span"),
        Some(&object(&[
            (
                "className",
                JsValue::from_str("seekdeep-primitive-json-tree-copyAnchor"),
            ),
            (
                "style",
                object(&[
                    (
                        "left",
                        required_property(copy_target, "left", "copy target")?,
                    ),
                    ("top", required_property(copy_target, "top", "copy target")?),
                ])?
                .into(),
            ),
        ])?),
        &[menu],
    )
}

fn copy_menu_items(items: &[(&str, &str)]) -> Result<Array, JsValue> {
    let output = Array::new();
    for (id, label) in items {
        output.push(
            &object(&[
                ("id", JsValue::from_str(id)),
                ("label", JsValue::from_str(label)),
            ])?
            .into(),
        );
    }
    Ok(output)
}

fn begin_copy(target: &JsValue, mode: &str, context: &CopyContext, reset_timer_ref: &JsValue) {
    let text = copy_text(target, mode);
    let Ok(text) = text else {
        settle_copy(false, &context.set_copy_state, reset_timer_ref);
        return;
    };
    let pending = (|| -> Result<JsValue, JsValue> {
        let navigator = required_property(&js_sys::global(), "navigator", "global")?;
        let clipboard = required_property(&navigator, "clipboard", "navigator")?;
        required_function(&clipboard, "writeText", "clipboard")?
            .call1(&clipboard, &JsValue::from_str(&text))
    })();
    let Ok(pending) = pending else {
        settle_copy(false, &context.set_copy_state, reset_timer_ref);
        return;
    };
    let setter = context.set_copy_state.clone();
    let timer = reset_timer_ref.clone();
    spawn_local(async move {
        let accepted = JsFuture::from(Promise::resolve(&pending)).await.is_ok();
        settle_copy(accepted, &setter, &timer);
    });
}

fn settle_copy(accepted: bool, setter: &Function, reset_timer_ref: &JsValue) {
    let _ = set_state(
        setter,
        &JsValue::from_str(if accepted { "copied" } else { "failed" }),
    );
    let _ = clear_optional_timeout(reset_timer_ref);
    let reset = setter.clone();
    let callback = Closure::wrap(Box::new(move || {
        let _ = set_state(&reset, &JsValue::from_str("idle"));
    }) as Box<dyn FnMut()>);
    if let Ok(handle) = set_timeout(&callback.into_js_value(), COPY_RESET_MS) {
        let _ = set_current(reset_timer_ref, &handle);
    }
}

fn copy_text(target: &JsValue, mode: &str) -> Result<String, JsValue> {
    let value = Reflect::get(target, &JsValue::from_str("value"))?;
    if mode == "path" {
        return formatted_path(&Array::from(&required_property(
            target,
            "path",
            "copy target",
        )?));
    }
    if mode == "prettyJson" {
        return json_stringify_pretty(&value);
    }
    if mode == "json" {
        return json_stringify(&value);
    }
    match value.js_typeof().as_string().as_deref() {
        Some("string") => Ok(value.as_string().unwrap_or_default()),
        Some("undefined") => Ok("undefined".to_owned()),
        Some("bigint") => js_to_string(&value),
        Some("symbol") => Ok(symbol_description(&value)?.unwrap_or_else(|| "Symbol".to_owned())),
        Some("function") => Ok(function_name(&value)?.unwrap_or_else(|| "Function".to_owned())),
        _ => json_stringify(&value),
    }
}

fn formatted_path(path: &Array) -> Result<String, JsValue> {
    let identifier = RegExp::new(r"^[A-Za-z_$][\w$]*$", "");
    let mut result = "$".to_owned();
    for part in path.iter() {
        if let Some(index) = part.as_f64() {
            write!(&mut result, "[{index:.0}]").expect("writing to String cannot fail");
        } else {
            let field = part.as_string().ok_or_else(|| {
                js_sys::TypeError::new("JSON path part must be a string or number")
            })?;
            if identifier.test(&field) {
                result.push('.');
                result.push_str(&field);
            } else {
                result.push('[');
                result.push_str(&json_stringify(&JsValue::from_str(&field))?);
                result.push(']');
            }
        }
    }
    Ok(result)
}

fn preview_value(react: &JsValue, value: &JsValue, depth: usize) -> Result<JsValue, JsValue> {
    if !is_expandable_value(value) {
        return preview_primitive(react, value);
    }
    let array = Array::is_array(value);
    let entries = entries_of(value)?;
    let limit = if array {
        ARRAY_PREVIEW_LIMIT
    } else {
        OBJECT_PREVIEW_LIMIT
    };
    let (open, close) = bracket_of(value);
    let mut children = vec![styled_text(react, "punctuation", open)?];
    if depth >= PREVIEW_DEPTH_LIMIT {
        children.push(styled_text(react, "previewEllipsis", "…")?);
    } else {
        for (index, (key, child)) in entries.iter().take(limit).enumerate() {
            let mut part = Vec::new();
            if index > 0 {
                part.push(styled_text(react, "punctuation", ", ")?);
            }
            if !array {
                part.push(styled_text(react, "previewProperty", key)?);
                part.push(styled_text(react, "punctuation", ": ")?);
            }
            part.push(preview_value(react, child, depth + 1)?);
            children.push(fragment(react, &part)?);
        }
        if entries.len() > limit {
            children.push(styled_text(react, "previewEllipsis", ", …")?);
        }
    }
    children.push(styled_text(react, "punctuation", close)?);
    fragment(react, &children)
}

fn preview_primitive(react: &JsValue, value: &JsValue) -> Result<JsValue, JsValue> {
    if value.is_null() {
        return styled_text(react, "keywordValue", "null");
    }
    let kind = value.js_typeof().as_string().unwrap_or_default();
    match kind.as_str() {
        "string" => styled_text(react, "stringValue", &json_stringify(value)?),
        "number" => styled_text(react, "numberValue", &js_to_string(value)?),
        "boolean" => styled_text(react, "keywordValue", &js_to_string(value)?),
        "bigint" => styled_text(react, "otherValue", &js_to_string(value)?),
        "undefined" => styled_text(react, "otherValue", "undefined"),
        "symbol" => styled_text(
            react,
            "otherValue",
            &symbol_description(value)?.unwrap_or_else(|| "Symbol".to_owned()),
        ),
        "function" => styled_text(
            react,
            "otherValue",
            &function_name(value)?.unwrap_or_else(|| "Function".to_owned()),
        ),
        _ => Ok(JsValue::NULL),
    }
}

fn primitive_value(react: &JsValue, value: &JsValue) -> Result<JsValue, JsValue> {
    if value.is_null() {
        return styled_text(react, "keywordValue", "null");
    }
    if value.is_instance_of::<js_sys::Date>() {
        let date = value
            .clone()
            .unchecked_into::<js_sys::Date>()
            .to_iso_string()
            .as_string()
            .ok_or_else(|| js_sys::TypeError::new("Date.toISOString() returned a non-string"))?;
        return styled_text(react, "otherValue", &date);
    }
    let kind = value.js_typeof().as_string().unwrap_or_default();
    match kind.as_str() {
        "string" => styled_text(react, "stringValue", &json_stringify(value)?),
        "boolean" => styled_text(react, "keywordValue", &js_to_string(value)?),
        "number" => styled_text(react, "numberValue", &js_to_string(value)?),
        "bigint" => styled_text(react, "numberValue", &format!("{}n", js_to_string(value)?)),
        "function" => styled_text(react, "otherValue", "function() { }"),
        "undefined" => styled_text(react, "otherValue", "undefined"),
        _ => styled_text(react, "otherValue", &js_to_string(value)?),
    }
}

fn styled_text(react: &JsValue, class_name: &str, text: &str) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &JsValue::from_str("span"),
        Some(&class_props(&format!(
            "seekdeep-primitive-json-tree-{class_name}"
        ))?),
        &[JsValue::from_str(text)],
    )
}

fn fragment(react: &JsValue, children: &[JsValue]) -> Result<JsValue, JsValue> {
    create_element(
        react,
        &required_property(react, "Fragment", "React")?,
        None,
        children,
    )
}

fn icon(react: &JsValue, name: &str, size: f64) -> Result<JsValue, JsValue> {
    let definition = ICON_DEFINITIONS
        .iter()
        .find(|definition| definition.name == name)
        .ok_or_else(|| js_sys::Error::new(&format!("{name} is missing from the icon catalog")))?;
    render_icon(
        react,
        *definition,
        &object(&[("size", JsValue::from_f64(size))])?.into(),
    )
}

#[derive(Clone)]
struct DataEffectInputs {
    data: JsValue,
    expand_top_level: bool,
    expand_top_level_ref: JsValue,
    initial_tab_stop: Option<String>,
    active_row_ref: JsValue,
    copy_menu_open_ref: JsValue,
    set_copy_target: Function,
    set_copy_state: Function,
    set_copy_menu_open: Function,
    set_tab_stop_id: Function,
    set_expanded: Function,
}

fn install_cleanup_effect(
    react: &JsValue,
    reset_timer_ref: &JsValue,
    active_row_ref: &JsValue,
) -> Result<(), JsValue> {
    let timer = reset_timer_ref.clone();
    let active = active_row_ref.clone();
    let effect = Closure::wrap(Box::new(move || -> JsValue {
        let timer = timer.clone();
        let active = active.clone();
        Closure::wrap(Box::new(move || {
            let _ = clear_optional_timeout(&timer);
            if let Ok(row) = current(&active)
                && !row.is_undefined()
            {
                let _ = call_method(
                    &row,
                    "removeAttribute",
                    &[JsValue::from_str("data-json-copy-active")],
                );
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(react, &effect.into_js_value(), &Array::new())
}

fn install_data_effect(react: &JsValue, inputs: DataEffectInputs) -> Result<(), JsValue> {
    let dependencies = Array::new();
    dependencies.push(&inputs.data);
    dependencies.push(&JsValue::from_bool(inputs.expand_top_level));
    dependencies.push(
        &inputs
            .initial_tab_stop
            .as_ref()
            .map_or_else(|| JsValue::NULL, |value| JsValue::from_str(value)),
    );
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let prior_expand_top_level = current(&inputs.expand_top_level_ref)?.as_bool();
        if prior_expand_top_level != Some(inputs.expand_top_level) {
            set_current(
                &inputs.expand_top_level_ref,
                &JsValue::from_bool(inputs.expand_top_level),
            )?;
            let expanded = if inputs.expand_top_level {
                Array::new()
            } else {
                Array::of1(&JsValue::from_str(""))
            };
            set_state(&inputs.set_expanded, expanded.as_ref())?;
        }
        set_active_row(&inputs.active_row_ref, None)?;
        set_current(&inputs.copy_menu_open_ref, &JsValue::FALSE)?;
        set_state(&inputs.set_copy_target, &JsValue::UNDEFINED)?;
        set_state(&inputs.set_copy_state, &JsValue::from_str("idle"))?;
        set_state(&inputs.set_copy_menu_open, &JsValue::FALSE)?;
        set_state(
            &inputs.set_tab_stop_id,
            &inputs
                .initial_tab_stop
                .as_ref()
                .map_or(JsValue::NULL, |value| JsValue::from_str(value)),
        )
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(react, &effect.into_js_value(), &dependencies)
}

fn install_reposition_effect(react: &JsValue, context: &CopyContext) -> Result<(), JsValue> {
    let context = context.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let reposition_context = context.clone();
        let reposition = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            let row = current(&reposition_context.active_row_ref)?;
            if !row.is_undefined() {
                reposition_copy_button(&reposition_context, &row)?;
            }
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>)
        .into_js_value();
        let window = required_property(&js_sys::global(), "window", "global")?;
        call_method(
            &window,
            "addEventListener",
            &[
                JsValue::from_str("scroll"),
                reposition.clone(),
                JsValue::TRUE,
            ],
        )?;
        call_method(
            &window,
            "addEventListener",
            &[JsValue::from_str("resize"), reposition.clone()],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            let _ = call_method(
                &window,
                "removeEventListener",
                &[
                    JsValue::from_str("scroll"),
                    reposition.clone(),
                    JsValue::TRUE,
                ],
            );
            let _ = call_method(
                &window,
                "removeEventListener",
                &[JsValue::from_str("resize"), reposition.clone()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(react, &effect.into_js_value(), &Array::new())
}

fn labels(props: &JsValue) -> Result<Labels, JsValue> {
    let source = Reflect::get(props, &JsValue::from_str("labels"))?;
    let source = (!source.is_null() && !source.is_undefined()).then_some(source);
    Ok(Labels {
        copy_value: label_string(source.as_ref(), "copyValue", "Copy value")?,
        copy_json: label_string(source.as_ref(), "copyJson", "Copy JSON")?,
        copy_path: label_string(source.as_ref(), "copyPath", "Copy property path")?,
        copy_pretty_json: label_string(source.as_ref(), "copyPrettyJson", "Copy pretty JSON")?,
        copy_compact_json: label_string(source.as_ref(), "copyCompactJson", "Copy compact JSON")?,
        copied: label_string(source.as_ref(), "copied", "Copied")?,
        copy_failed: label_string(source.as_ref(), "copyFailed", "Copy failed")?,
        collapse_node: label_string(source.as_ref(), "collapseNode", "Collapse JSON node")?,
        expand_node: label_string(source.as_ref(), "expandNode", "Expand JSON node")?,
        copy_button_title: source
            .as_ref()
            .map(|source| optional_function(source, "copyButtonTitle"))
            .transpose()?
            .flatten(),
    })
}

fn label_string(source: Option<&JsValue>, key: &str, fallback: &str) -> Result<String, JsValue> {
    source
        .map(|source| optional_string(source, key))
        .transpose()?
        .flatten()
        .map_or_else(|| Ok(fallback.to_owned()), Ok)
}

fn entries_of(value: &JsValue) -> Result<Vec<(String, JsValue)>, JsValue> {
    if Array::is_array(value) {
        let array = Array::from(value);
        return Ok((0..array.length())
            .map(|index| (index.to_string(), array.get(index)))
            .collect());
    }
    let object = Object::from(value.clone());
    Object::keys(&object)
        .iter()
        .filter_map(|key| key.as_string())
        .map(|key| -> Result<_, JsValue> {
            let value = Reflect::get(&object, &JsValue::from_str(&key))?;
            Ok((key, value))
        })
        .collect()
}

fn initial_tab_stop(
    data: &JsValue,
    entries: &[(String, JsValue)],
    expand_top_level: bool,
) -> Result<Option<String>, JsValue> {
    if !expand_top_level {
        return Ok((is_expandable_value(data) && !entries.is_empty()).then(String::new));
    }
    for (index, (key, value)) in entries.iter().enumerate() {
        if is_expandable_value(value) && !entries_of(value)?.is_empty() {
            return Ok(Some(path_id(&[if Array::is_array(data) {
                PathPart::Index(index)
            } else {
                PathPart::Field(key.clone())
            }])));
        }
    }
    Ok(None)
}

fn is_expandable_value(value: &JsValue) -> bool {
    value.js_typeof().as_string().as_deref() == Some("object")
        && !value.is_null()
        && !value.is_instance_of::<js_sys::Date>()
}

fn bracket_of(value: &JsValue) -> (&'static str, &'static str) {
    if Array::is_array(value) {
        ("[", "]")
    } else {
        ("{", "}")
    }
}

fn path_id(path: &[PathPart]) -> String {
    path.iter()
        .map(|part| match part {
            PathPart::Index(index) => format!("n{index}"),
            PathPart::Field(field) => format!("s{}:{field}", field.encode_utf16().count()),
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[allow(clippy::cast_precision_loss)] // JavaScript Array indices are bounded to u32.
fn path_to_js(path: &[PathPart]) -> JsValue {
    let output = Array::new();
    for part in path {
        match part {
            PathPart::Index(index) => output.push(&JsValue::from_f64(*index as f64)),
            PathPart::Field(field) => output.push(&JsValue::from_str(field)),
        };
    }
    output.into()
}

fn contents_id(node_id: &str) -> String {
    if node_id.is_empty() {
        "json-tree-root".to_owned()
    } else {
        format!("json-tree-{node_id}")
    }
}

fn symbol_description(value: &JsValue) -> Result<Option<String>, JsValue> {
    let boxed = required_function(&js_sys::global(), "Object", "global")?
        .call1(&JsValue::UNDEFINED, value)?;
    let description = Reflect::get(&boxed, &JsValue::from_str("description"))?;
    Ok(description.as_string())
}

fn function_name(value: &JsValue) -> Result<Option<String>, JsValue> {
    let name = Reflect::get(value, &JsValue::from_str("name"))?;
    Ok(name.as_string().filter(|name| !name.is_empty()))
}

fn js_to_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() returned a non-string").into())
}

fn json_stringify(value: &JsValue) -> Result<String, JsValue> {
    Ok(js_sys::JSON::stringify(value)?
        .as_string()
        .unwrap_or_else(|| "undefined".to_owned()))
}

fn json_stringify_pretty(value: &JsValue) -> Result<String, JsValue> {
    Ok(js_sys::JSON::stringify_with_replacer_and_space(
        value,
        &JsValue::NULL,
        &JsValue::from_f64(2.0),
    )?
    .as_string()
    .unwrap_or_else(|| "undefined".to_owned()))
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = "@seekdeep-ai/seekdeep-client-ui-primitives/JsonTree.module.css";
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("style[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let replacements = [
        (
            "expandedTopLevelContainer",
            "seekdeep-primitive-json-tree-expandedTopLevelContainer",
        ),
        (
            "expandedTopLevel",
            "seekdeep-primitive-json-tree-expandedTopLevel",
        ),
        (
            "topLevelBracket",
            "seekdeep-primitive-json-tree-topLevelBracket",
        ),
        (
            "clickableLabel",
            "seekdeep-primitive-json-tree-clickableLabel",
        ),
        (
            "previewProperty",
            "seekdeep-primitive-json-tree-previewProperty",
        ),
        (
            "previewEllipsis",
            "seekdeep-primitive-json-tree-previewEllipsis",
        ),
        (
            "collapsedContent",
            "seekdeep-primitive-json-tree-collapsedContent",
        ),
        ("keywordValue", "seekdeep-primitive-json-tree-keywordValue"),
        ("stringValue", "seekdeep-primitive-json-tree-stringValue"),
        ("numberValue", "seekdeep-primitive-json-tree-numberValue"),
        ("otherValue", "seekdeep-primitive-json-tree-otherValue"),
        ("collapseIcon", "seekdeep-primitive-json-tree-collapseIcon"),
        ("expandIcon", "seekdeep-primitive-json-tree-expandIcon"),
        ("copyAnchor", "seekdeep-primitive-json-tree-copyAnchor"),
        ("copyButton", "seekdeep-primitive-json-tree-copyButton"),
        ("punctuation", "seekdeep-primitive-json-tree-punctuation"),
        ("container", "seekdeep-primitive-json-tree-container"),
        ("children", "seekdeep-primitive-json-tree-children"),
        ("expander", "seekdeep-primitive-json-tree-expander"),
        ("preview", "seekdeep-primitive-json-tree-preview"),
        ("label", "seekdeep-primitive-json-tree-label"),
        ("root", "seekdeep-primitive-json-tree-root"),
        ("row", "seekdeep-primitive-json-tree-row"),
    ];
    let mut css = JSON_TREE_CSS.replace(
        ":global(body[data-ds-dark-theme])",
        "body[data-ds-dark-theme]",
    );
    for (source, target) in replacements {
        css = css.replace(&format!(".{source}"), &format!(".{target}"));
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin-css"), JsValue::from_str(tag)],
    )?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin"),
            JsValue::from_str("@seekdeep-ai/seekdeep-client-ui-primitives"),
        ],
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&css),
    )?;
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives JsonTree module was not configured").into()
        })
    })
}

fn clear_optional_timeout(reference: &JsValue) -> Result<(), JsValue> {
    let handle = current(reference)?;
    if handle.is_null() || handle.is_undefined() {
        return Ok(());
    }
    required_function(&js_sys::global(), "clearTimeout", "global")?
        .call1(&js_sys::global(), &handle)?;
    set_current(reference, &JsValue::UNDEFINED)
}

fn set_timeout(callback: &JsValue, delay: f64) -> Result<JsValue, JsValue> {
    required_function(&js_sys::global(), "setTimeout", "global")?.call2(
        &js_sys::global(),
        callback,
        &JsValue::from_f64(delay),
    )
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_effect(react: &JsValue, effect: &JsValue, dependencies: &Array) -> Result<(), JsValue> {
    required_function(react, "useEffect", "React")?
        .call2(react, effect, dependencies)
        .map(|_| ())
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn optional_bool(value: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_bool()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a boolean")).into())
    }
}

fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property.dyn_into().map(Some)
    }
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Ok(None)
    } else {
        property
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn required_number(value: &JsValue, key: &str, owner: &str) -> Result<f64, JsValue> {
    required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")).into())
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn create_element(
    react: &JsValue,
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
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
