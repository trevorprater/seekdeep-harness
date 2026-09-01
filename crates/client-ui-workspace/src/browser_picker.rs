//! Compiled Workspace picker and directory-flow core.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::browser::{
    BrowserModules, call, component, css, element, function, inject_style, object, optional,
    rejection_text, required, tag, use_state,
};

const PICKER_CSS: &str =
    include_str!("../../../packages/client/ui-workspace/src/client/WorkspacePicker.module.css");
const ADD_WORKSPACE: &str = "::add-workspace";

thread_local! {
    static COMPONENTS: RefCell<Option<Components>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct Components {
    picker: JsValue,
    flow: JsValue,
}

/// Configures page-owned React, primitives, and picker styles.
///
/// # Errors
///
/// Returns before mutation when a required browser dependency is unavailable.
#[wasm_bindgen(js_name = configureClientUiWorkspace)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_workspace(react: JsValue, primitives: JsValue) -> Result<(), JsValue> {
    for method in ["createElement", "useCallback", "useEffect", "useState"] {
        function(&react, method, "React")?;
    }
    required(&react, "Fragment", "React")?;
    for primitive in [
        "Button",
        "HoverCard",
        "IconArchiveOutline20",
        "IconBranchOutline16",
        "IconEditOutline16",
        "IconEllipsisOutline16",
        "IconFolderClose16",
        "IconFolderOpen16",
        "IconPlusOutline16",
        "IconTrashOutline16",
        "IconTriangleRightFill14",
        "Menu",
        "Modal",
        "StateDot",
    ] {
        required(&primitives, primitive, "UI primitives")?;
    }
    inject_style("WorkspacePicker", PICKER_CSS)?;
    let modules = BrowserModules { react, primitives };
    crate::browser_rows::configure_rows(&modules)?;
    COMPONENTS.with(|configured| {
        *configured.borrow_mut() = Some(Components {
            picker: component(&modules, render_picker),
            flow: component(&modules, render_pick_flow),
        });
    });
    Ok(())
}

fn configured_components() -> Result<Components, JsValue> {
    COMPONENTS.with(|configured| {
        configured
            .borrow()
            .clone()
            .ok_or_else(|| js_sys::Error::new("client-ui-workspace was not configured").into())
    })
}

/// Returns the compiled empty-state Workspace picker.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = workspacePickerComponent)]
pub fn workspace_picker_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.picker)
}

/// Returns the reusable compiled picker-flow core.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = workspacePickFlowComponent)]
pub fn workspace_pick_flow_component() -> Result<JsValue, JsValue> {
    Ok(configured_components()?.flow)
}

fn use_effect(react: &JsValue, effect: JsValue, deps: &Array) -> Result<(), JsValue> {
    let result = function(react, "useEffect", "React")?
        .call2(react, &effect, deps)
        .map(|_| ());
    drop(effect);
    result
}

fn use_callback(react: &JsValue, callback: JsValue, deps: &Array) -> Result<Function, JsValue> {
    let result = function(react, "useCallback", "React")?
        .call2(react, &callback, deps)?
        .dyn_into::<Function>();
    drop(callback);
    result
}

fn identity_selector() -> JsValue {
    Closure::wrap(Box::new(move |value: JsValue| value) as Box<dyn FnMut(JsValue) -> JsValue>)
        .into_js_value()
}

#[allow(clippy::too_many_lines)] // Menu, directory-flow conversation, adoption, and error Modal are one source core.
fn render_pick_flow(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let snapshot = function(props, "useWorkspaces", "WorkspacePickFlow props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let workspaces = Array::from(&required(&snapshot, "items", "Workspace list snapshot")?);
    let anchor_ref = optional(props, "anchorRef")?;
    let rect_ref = anchor_ref.clone();
    let get_anchor = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let Some(reference) = &rect_ref else {
            return Ok(JsValue::NULL);
        };
        let current = Reflect::get(reference, &JsValue::from_str("current"))?;
        if current.is_null() || current.is_undefined() {
            Ok(JsValue::NULL)
        } else {
            call(&current, "getBoundingClientRect", &[])
        }
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let get_anchor = use_callback(
        &modules.react,
        get_anchor.into_js_value(),
        &Array::of1(&anchor_ref.unwrap_or(JsValue::UNDEFINED)),
    )?;
    let (error_open, set_error_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let (modal_error, set_modal_error) = use_state(&modules.react, &JsValue::NULL)?;
    let (flow_open, set_flow_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let (picking, set_picking) = use_state(&modules.react, &JsValue::FALSE)?;
    let error_open = error_open.as_bool().unwrap_or(false);
    let flow_open = flow_open.as_bool().unwrap_or(false);
    let picking = picking.as_bool().unwrap_or(false);
    let flow_busy = flow_open || picking;
    let flow_available = function(props, "useDirectoryFlow", "WorkspacePickFlow props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?
        .as_bool()
        .unwrap_or(false);
    let withdraw_flow = set_flow_open.clone();
    let withdraw = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if flow_open && !flow_available {
            withdraw_flow.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        withdraw.into_js_value(),
        &Array::of2(
            &JsValue::from_bool(flow_open),
            &JsValue::from_bool(flow_available),
        ),
    )?;
    let translate = function(props, "t", "WorkspacePickFlow props")?;
    let add_entries = Array::new();
    if flow_available {
        let icon = element(
            &modules.react,
            &modules.primitive("IconPlusOutline16")?,
            Some(&object(&[("size", JsValue::from_f64(16.0))])?),
            &[],
        )?;
        let entry: JsValue = object(&[
            ("id", JsValue::from_str(ADD_WORKSPACE)),
            (
                "label",
                translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("menu.addWorkspace"))?,
            ),
            ("icon", icon),
            ("disabled", JsValue::from_bool(flow_busy)),
        ])?
        .into();
        add_entries.push(&entry);
    }
    let add_only = optional(props, "addOnly")?.and_then(|value| value.as_bool()) == Some(true);
    let pin_add = !add_only && workspaces.length() > 0;
    let items = if pin_add {
        let items = Array::new();
        for workspace in workspaces.iter() {
            let icon = element(
                &modules.react,
                &modules.primitive("IconFolderClose16")?,
                Some(&object(&[("size", JsValue::from_f64(16.0))])?),
                &[],
            )?;
            let entry: JsValue = object(&[
                ("id", required(&workspace, "workspaceId", "Workspace view")?),
                ("label", required(&workspace, "title", "Workspace view")?),
                ("icon", icon),
                ("disabled", JsValue::from_bool(flow_busy)),
            ])?
            .into();
            items.push(&entry);
        }
        items
    } else {
        add_entries.clone()
    };
    let menu_empty = items.length() == 0;
    let close = function(props, "onClose", "WorkspacePickFlow props")?;
    let open_flow_close = close.clone();
    let open_flow_error = set_error_open.clone();
    let open_flow_message = set_modal_error.clone();
    let open_flow_state = set_flow_open.clone();
    let open_flow = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        open_flow_close.call0(&JsValue::UNDEFINED)?;
        open_flow_error.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        open_flow_message.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        open_flow_state.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let open_flow = use_callback(
        &modules.react,
        open_flow.into_js_value(),
        &Array::of1(&required(props, "onClose", "WorkspacePickFlow props")?),
    )?;
    let phase = required(&snapshot, "phase", "Workspace list snapshot")?
        .as_string()
        .unwrap_or_default();
    let settled = add_only || phase == "ready";
    let add_only_entry = !pin_add && settled && add_entries.length() == 1;
    let auto_flow = open_flow.clone();
    let requested_open = required(props, "open", "WorkspacePickFlow props")?
        .as_bool()
        .unwrap_or(false);
    let auto = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if requested_open && add_only_entry && !flow_busy {
            auto_flow.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        auto.into_js_value(),
        &Array::of4(
            &JsValue::from_bool(requested_open),
            &JsValue::from_bool(add_only_entry),
            &JsValue::from_bool(flow_busy),
            open_flow.as_ref(),
        ),
    )?;

    let create = function(props, "createWorkspace", "WorkspacePickFlow props")?;
    let picked = function(props, "onPick", "WorkspacePickFlow props")?;
    let picked_flow = set_flow_open.clone();
    let failed_flow = set_flow_open.clone();
    let failed_message = set_modal_error.clone();
    let failed_open = set_error_open.clone();
    let busy_setter = set_picking.clone();
    let on_picked = Closure::wrap(Box::new(move |path: String| -> Result<(), JsValue> {
        busy_setter.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        let request = object(&[("path", JsValue::from_str(&path))])?;
        let promise = Promise::resolve(&create.call1(&JsValue::UNDEFINED, &request)?);
        let success_flow = picked_flow.clone();
        let success_pick = picked.clone();
        let success = Closure::wrap(Box::new(move |workspace: JsValue| {
            let result = (|| -> Result<(), JsValue> {
                success_flow.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
                success_pick.call1(
                    &JsValue::UNDEFINED,
                    &required(&workspace, "workspaceId", "Workspace view")?,
                )?;
                Ok(())
            })();
            if let Err(error) = result {
                wasm_bindgen::throw_val(error);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let error_flow = failed_flow.clone();
        let error_message = failed_message.clone();
        let error_open = failed_open.clone();
        let failure = Closure::wrap(Box::new(move |reason: JsValue| {
            let result = (|| -> Result<(), JsValue> {
                error_message.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(&rejection_text(&reason)),
                )?;
                error_flow.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
                error_open.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
                Ok(())
            })();
            if let Err(error) = result {
                wasm_bindgen::throw_val(error);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let settled_busy = set_picking.clone();
        let settled = Closure::wrap(Box::new(move || {
            let _ = settled_busy.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        }) as Box<dyn FnMut()>);
        let chained = promise.then2(&success, &failure);
        let _ = chained.finally(&settled);
        drop(success.into_js_value());
        drop(failure.into_js_value());
        drop(settled.into_js_value());
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let cancel_flow = set_flow_open.clone();
    let on_cancel = Closure::wrap(Box::new(move || {
        let _ = cancel_flow.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let owner_error_flow = set_flow_open.clone();
    let owner_error_message = set_modal_error.clone();
    let owner_error_open = set_error_open.clone();
    let on_error = Closure::wrap(Box::new(move |message: String| -> Result<(), JsValue> {
        owner_error_flow.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        owner_error_message.call1(&JsValue::UNDEFINED, &JsValue::from_str(&message))?;
        owner_error_open.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let flow_owner = object(&[
        ("open", JsValue::from_bool(flow_open)),
        ("busy", JsValue::from_bool(picking)),
        ("onPicked", on_picked.into_js_value()),
        ("onCancel", on_cancel.into_js_value()),
        ("onError", on_error.into_js_value()),
    ])?;
    let flow = function(props, "renderDirectoryFlow", "WorkspacePickFlow props")?
        .call1(&JsValue::UNDEFINED, &flow_owner)?;
    let handle_flow = open_flow.clone();
    let handle_pick = function(props, "onPick", "WorkspacePickFlow props")?;
    let handle_select = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        if id == ADD_WORKSPACE {
            handle_flow.call0(&JsValue::UNDEFINED)?;
        } else {
            handle_pick.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id))?;
        }
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let menu_open = requested_open && !add_only_entry && !menu_empty;
    let mut menu_props = vec![
        ("open", JsValue::from_bool(menu_open)),
        ("anchor", JsValue::NULL),
        ("items", items.into()),
        (
            "selectedId",
            optional(props, "selectedId")?.unwrap_or(JsValue::UNDEFINED),
        ),
        ("onSelect", handle_select.into_js_value()),
        ("onClose", close.into()),
        (
            "side",
            optional(props, "side")?.unwrap_or(JsValue::from_str("bottom")),
        ),
        ("portal", JsValue::TRUE),
        ("getAnchorRect", get_anchor.into()),
    ];
    if pin_add {
        menu_props.push(("footer", add_entries.into()));
    }
    let menu = element(
        &modules.react,
        &modules.primitive("Menu")?,
        Some(&object(&menu_props)?),
        &[],
    )?;
    let mut children = vec![menu];
    if menu_open && phase == "pending" {
        children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("menuStatus"))),
                ("role", JsValue::from_str("status")),
            ])?),
            &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("picker.loading"))?],
        )?);
    }
    children.push(flow);
    let close_error = set_error_open.clone();
    let clear_error = set_modal_error.clone();
    let close_modal = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_error.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        clear_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let retry_flow = open_flow;
    let retry = Closure::wrap(Box::new(move || {
        let _ = retry_flow.call0(&JsValue::UNDEFINED);
    }) as Box<dyn FnMut()>);
    let cancel_button = element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("outline")),
            ("className", JsValue::from_str(&css("modalAction"))),
            ("onClick", close_modal.as_ref().clone()),
        ])?),
        &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("cancel"))?],
    )?;
    let retry_button = element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&[
            ("variant", JsValue::from_str("primary")),
            ("className", JsValue::from_str(&css("modalAction"))),
            ("disabled", JsValue::from_bool(!flow_available)),
            ("onClick", retry.into_js_value()),
        ])?),
        &[translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("folderError.retry"))?],
    )?;
    let footer = element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &[cancel_button, retry_button],
    )?;
    let modal = element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(error_open)),
            ("onClose", close_modal.into_js_value()),
            (
                "closeLabel",
                translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("close"))?,
            ),
            (
                "title",
                translate.call1(&JsValue::UNDEFINED, &JsValue::from_str("folderError.title"))?,
            ),
            ("footer", footer),
        ])?),
        &[tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("className", JsValue::from_str(&css("modalError"))),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[modal_error],
        )?],
    )?;
    children.push(modal);
    element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        &children,
    )
}

fn render_picker(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let render_slot = function(props, "renderSlot", "WorkspacePicker props")?;
    let render_flow = Closure::wrap(Box::new(move |owner: JsValue| {
        render_slot.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("conversation.hero.workspace.directoryFlow"),
            &owner,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let flow_props = Object::assign(&Object::new(), &props.clone().dyn_into::<Object>()?);
    Reflect::set(
        &flow_props,
        &JsValue::from_str("renderDirectoryFlow"),
        &render_flow.into_js_value(),
    )?;
    element(
        &modules.react,
        &configured_components()?.flow,
        Some(&flow_props),
        &[],
    )
}
