//! Compiled owning Workspace-browser surface.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    FLAT_SESSION_ORDER_KEY, UNGROUPED_KEY,
    browser::{
        BrowserModules, call, class, component, element, function, inject_style, object,
        rejection_text, required, tag, translated, use_state,
    },
    browser_lists::{ListComponents, configure_lists, warn_rejection},
    browser_model::workspaces,
    workspace_pick_flow_component,
};

const BROWSER_CSS: &str =
    include_str!("../../../packages/client/ui-workspace/src/client/WorkspaceBrowser.module.css");
const EXPAND_SLIDE_MS: f64 = 300.0;
const SEARCH_DEBOUNCE_MS: f64 = 250.0;
const SEARCH_QUERY_MAX_CODE_UNITS_F64: f64 = 500.0;

thread_local! {
    static INTERNALS: RefCell<Option<Internals>> = const { RefCell::new(None) };
    static ROOT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct Internals {
    lists: ListComponents,
    view_options: JsValue,
}

pub(crate) fn configure_workspace_browser(modules: &BrowserModules) -> Result<(), JsValue> {
    inject_style("WorkspaceBrowser", BROWSER_CSS)?;
    let internals = Internals {
        lists: configure_lists(modules),
        view_options: component(modules, render_view_options),
    };
    INTERNALS.with(|configured| *configured.borrow_mut() = Some(internals));
    ROOT.with(|configured| {
        *configured.borrow_mut() = Some(component(modules, render_workspace_browser));
    });
    Ok(())
}

fn internals() -> Result<Internals, JsValue> {
    INTERNALS.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-workspace browser internals were not configured").into()
        })
    })
}

/// Returns the compiled `WorkspaceBrowser` component.
///
/// # Errors
///
/// Returns before browser configuration.
#[wasm_bindgen(js_name = workspaceBrowserComponent)]
pub fn workspace_browser_component() -> Result<JsValue, JsValue> {
    ROOT.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-workspace browser was not configured").into()
        })
    })
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

fn icon(modules: &BrowserModules, name: &str, props: Option<&Object>) -> Result<JsValue, JsValue> {
    element(&modules.react, &modules.primitive(name)?, props, &[])
}

fn tooltip(
    modules: &BrowserModules,
    label: JsValue,
    side: Option<&str>,
    delay_ms: Option<f64>,
    disabled: Option<bool>,
    child: JsValue,
) -> Result<JsValue, JsValue> {
    let mut props = vec![("label", label)];
    if let Some(side) = side {
        props.push(("side", JsValue::from_str(side)));
    }
    if let Some(delay_ms) = delay_ms {
        props.push(("delayMs", JsValue::from_f64(delay_ms)));
    }
    if let Some(disabled) = disabled {
        props.push(("disabled", JsValue::from_bool(disabled)));
    }
    element(
        &modules.react,
        &modules.primitive("Tooltip")?,
        Some(&object(&props)?),
        &[child],
    )
}

fn boxed(value: &JsValue) -> Result<JsValue, JsValue> {
    function(&js_sys::global(), "Object", "globalThis")?.call1(&JsValue::UNDEFINED, value)
}

fn string_method(value: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let boxed = boxed(value)?;
    call(&boxed, name, args)
}

fn sanitize_search_query(value: &JsValue) -> Result<JsValue, JsValue> {
    let without_nul = string_method(
        value,
        "replaceAll",
        &[JsValue::from_str("\0"), JsValue::from_str("")],
    )?;
    let length = number_property(&boxed(&without_nul)?, "length", "search query")?;
    if length <= SEARCH_QUERY_MAX_CODE_UNITS_F64 {
        return Ok(without_nul);
    }
    let mut end = SEARCH_QUERY_MAX_CODE_UNITS_F64;
    let last = string_method(&without_nul, "charCodeAt", &[JsValue::from_f64(end - 1.0)])?
        .as_f64()
        .unwrap_or(f64::NAN);
    let next = string_method(&without_nul, "charCodeAt", &[JsValue::from_f64(end)])?
        .as_f64()
        .unwrap_or(f64::NAN);
    if (f64::from(0xD800)..=f64::from(0xDBFF)).contains(&last)
        && (f64::from(0xDC00)..=f64::from(0xDFFF)).contains(&next)
    {
        end -= 1.0;
    }
    string_method(
        &without_nul,
        "slice",
        &[JsValue::from_f64(0.0), JsValue::from_f64(end)],
    )
}

fn normalized_query(query: &JsValue) -> Result<JsValue, JsValue> {
    string_method(&sanitize_search_query(query)?, "trim", &[])
}

fn focus_ref(reference: &JsValue, prevent_scroll: bool) -> Result<(), JsValue> {
    let target = current(reference)?;
    if target.is_null() || target.is_undefined() {
        return Ok(());
    }
    if prevent_scroll {
        call(
            &target,
            "focus",
            &[object(&[("preventScroll", JsValue::TRUE)])?.into()],
        )?;
    } else {
        call(&target, "focus", &[])?;
    }
    Ok(())
}

fn set_timeout(callback: &JsValue, delay_ms: f64) -> Result<JsValue, JsValue> {
    call(
        &js_sys::global(),
        "setTimeout",
        &[callback.clone(), JsValue::from_f64(delay_ms)],
    )
}

fn clear_timeout(timer: &JsValue) {
    let _ = call(
        &js_sys::global(),
        "clearTimeout",
        std::slice::from_ref(timer),
    );
}

fn abort_controller() -> Result<JsValue, JsValue> {
    let constructor =
        required(&js_sys::global(), "AbortController", "globalThis")?.dyn_into::<Function>()?;
    Reflect::construct(&constructor, &Array::new())
}

fn attach_promise(
    result: &JsValue,
    success: Closure<dyn FnMut(JsValue)>,
    failure: Closure<dyn FnMut(JsValue)>,
) {
    let _ = Promise::resolve(result).then2(&success, &failure);
    drop(success.into_js_value());
    drop(failure.into_js_value());
}

fn target_value(event: &JsValue) -> Result<JsValue, JsValue> {
    let target = required(event, "target", "input event")?;
    required(&target, "value", "input target")
}

fn event_key(event: &JsValue) -> Result<String, JsValue> {
    string_property(event, "key", "keyboard event")
}

#[allow(clippy::too_many_lines)]
fn render_view_options(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let group_by = string_property(props, "groupBy", "ViewOptionsMenu props")?;
    let order_by = string_property(props, "orderBy", "ViewOptionsMenu props")?;
    let translate = function(props, "t", "ViewOptionsMenu props")?;
    let (open, set_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let open = open.as_bool().unwrap_or(false);
    let items = Array::new();
    for item in [
        object(&[
            ("type", JsValue::from_str("label")),
            ("id", JsValue::from_str("group-by")),
            ("text", translated(&translate, "groupBy.label", None)?),
        ])?,
        object(&[
            ("id", JsValue::from_str("workspace")),
            ("label", translated(&translate, "groupBy.workspace", None)?),
        ])?,
        object(&[
            ("id", JsValue::from_str("flat")),
            ("label", translated(&translate, "groupBy.flat", None)?),
        ])?,
        object(&[
            ("type", JsValue::from_str("separator")),
            ("id", JsValue::from_str("order-by-separator")),
        ])?,
        object(&[
            ("type", JsValue::from_str("label")),
            ("id", JsValue::from_str("order-by")),
            ("text", translated(&translate, "orderBy.label", None)?),
        ])?,
        object(&[
            ("id", JsValue::from_str("manual")),
            ("label", translated(&translate, "orderBy.manual", None)?),
        ])?,
        object(&[
            ("id", JsValue::from_str("updated")),
            ("label", translated(&translate, "orderBy.updated", None)?),
        ])?,
    ] {
        items.push(&item);
    }
    let close_setter = set_open.clone();
    let on_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let select_setter = set_open.clone();
    let group_pick = function(props, "onGroupPick", "ViewOptionsMenu props")?;
    let order_pick = function(props, "onOrderPick", "ViewOptionsMenu props")?;
    let on_select = Closure::wrap(Box::new(move |id: String| -> Result<(), JsValue> {
        match id.as_str() {
            "workspace" | "flat" => {
                group_pick.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id))?;
            }
            "manual" | "updated" => {
                order_pick.call1(&JsValue::UNDEFINED, &JsValue::from_str(&id))?;
            }
            _ => {}
        }
        select_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
    let toggle_setter = set_open;
    let on_toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        toggle_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!open))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let label = translated(&translate, "viewOptions.label", None)?;
    let button = tag(
        &modules.react,
        "button",
        Some(&object(&[
            ("type", JsValue::from_str("button")),
            (
                "className",
                JsValue::from_str(&class(&[("iconButton", true), ("wide", true)])),
            ),
            ("aria-label", label.clone()),
            ("onClick", on_toggle.into_js_value()),
        ])?),
        &[icon(modules, "IconPersonalizationOutline16", None)?],
    )?;
    let anchor = tooltip(modules, label, Some("bottom"), Some(500.0), None, button)?;
    element(
        &modules.react,
        &modules.primitive("Menu")?,
        Some(&object(&[
            ("open", JsValue::from_bool(open)),
            ("onClose", on_close.into_js_value()),
            ("items", items.into()),
            (
                "selectedIds",
                Array::of2(&JsValue::from_str(&group_by), &JsValue::from_str(&order_by)).into(),
            ),
            ("onSelect", on_select.into_js_value()),
            ("align", JsValue::from_str("end")),
            ("dense", JsValue::TRUE),
            ("portal", JsValue::TRUE),
            ("anchor", anchor),
        ])?),
        &[],
    )
}

fn retain_accounts_effect(
    modules: &BrowserModules,
    workspace_snapshot: &JsValue,
    workspace_phase: &str,
    workspace_ids: Vec<String>,
    retain: Function,
) -> Result<(), JsValue> {
    let phase = workspace_phase.to_owned();
    let effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if phase != "ready" {
            return Ok(());
        }
        let keys = Array::new();
        keys.push(&JsValue::from_str(UNGROUPED_KEY));
        keys.push(&JsValue::from_str(FLAT_SESSION_ORDER_KEY));
        for key in &workspace_ids {
            keys.push(&JsValue::from_str(key));
        }
        retain.call1(&JsValue::UNDEFINED, &keys)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of2(workspace_snapshot, &JsValue::from_str(workspace_phase)),
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn search_focus_effects(
    modules: &BrowserModules,
    wide: bool,
    search_expanded: bool,
    search_on_expand: bool,
    set_search_on_expand: &Function,
    search_input: &JsValue,
    normalized: &JsValue,
    search_root: &JsValue,
    set_search_expanded: &Function,
) -> Result<(), JsValue> {
    let delayed_input = search_input.clone();
    let delayed_setter = set_search_on_expand.clone();
    let delayed = Closure::wrap(Box::new(move || -> JsValue {
        if !wide || !search_on_expand {
            return JsValue::UNDEFINED;
        }
        let input = delayed_input.clone();
        let setter = delayed_setter.clone();
        let callback = Closure::wrap(Box::new(move || {
            let _ = focus_ref(&input, true);
            let _ = setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
        }) as Box<dyn FnMut()>);
        let callback_value = callback.into_js_value();
        let Ok(timer) = set_timeout(&callback_value, EXPAND_SLIDE_MS) else {
            return JsValue::UNDEFINED;
        };
        Closure::wrap(Box::new(move || clear_timeout(&timer)) as Box<dyn FnMut()>).into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(
        &modules.react,
        delayed.into_js_value(),
        &Array::of2(
            &JsValue::from_bool(wide),
            &JsValue::from_bool(search_on_expand),
        ),
    )?;

    let immediate_input = search_input.clone();
    let immediate = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if wide && search_expanded && !search_on_expand {
            focus_ref(&immediate_input, true)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        immediate.into_js_value(),
        &Array::of3(
            &JsValue::from_bool(wide),
            &JsValue::from_bool(search_expanded),
            &JsValue::from_bool(search_on_expand),
        ),
    )?;

    let outside_root = search_root.clone();
    let outside_input = search_input.clone();
    let outside_normalized = normalized.clone();
    let outside_setter = set_search_expanded.clone();
    let outside = Closure::wrap(Box::new(move || -> JsValue {
        if !wide || !search_expanded {
            return JsValue::UNDEFINED;
        }
        let document = match Reflect::get(&js_sys::global(), &JsValue::from_str("document")) {
            Ok(document) if !document.is_null() && !document.is_undefined() => document,
            _ => return JsValue::UNDEFINED,
        };
        let root = outside_root.clone();
        let input = outside_input.clone();
        let normalized = outside_normalized.clone();
        let setter = outside_setter.clone();
        let listener = Closure::wrap(Box::new(move |event: JsValue| {
            let target =
                Reflect::get(&event, &JsValue::from_str("target")).unwrap_or(JsValue::UNDEFINED);
            if !target.is_object() {
                return;
            }
            let root_value = current(&root).unwrap_or(JsValue::NULL);
            let inside = if root_value.is_null() || root_value.is_undefined() {
                false
            } else {
                call(&root_value, "contains", &[target])
                    .ok()
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            };
            if inside {
                return;
            }
            let input_value = current(&input).unwrap_or(JsValue::NULL);
            if !input_value.is_null() && !input_value.is_undefined() {
                let _ = call(&input_value, "blur", &[]);
            }
            if normalized.as_string().as_deref() == Some("") {
                let _ = setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let listener_value = listener.into_js_value();
        if call(
            &document,
            "addEventListener",
            &[JsValue::from_str("click"), listener_value.clone()],
        )
        .is_err()
        {
            return JsValue::UNDEFINED;
        }
        Closure::wrap(Box::new(move || {
            let _ = call(
                &document,
                "removeEventListener",
                &[JsValue::from_str("click"), listener_value.clone()],
            );
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    use_effect(
        &modules.react,
        outside.into_js_value(),
        &Array::of3(
            normalized,
            &JsValue::from_bool(wide),
            &JsValue::from_bool(search_expanded),
        ),
    )
}

#[allow(clippy::too_many_lines)]
fn remote_search_effect(
    modules: &BrowserModules,
    normalized: &JsValue,
    search: Function,
    set_remote: Function,
) -> Result<(), JsValue> {
    let query = normalized.clone();
    let search_dependency = search.clone();
    let effect = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        let query_text = query.as_string().unwrap_or_default();
        if query_text.is_empty() {
            set_remote.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("query", JsValue::from_str("")),
                    ("status", JsValue::from_str("idle")),
                    ("items", Array::new().into()),
                    ("hasMore", JsValue::FALSE),
                ])?
                .into(),
            )?;
            return Ok(JsValue::UNDEFINED);
        }
        let controller = abort_controller()?;
        let signal = required(&controller, "signal", "AbortController")?;
        set_remote.call1(
            &JsValue::UNDEFINED,
            &object(&[
                ("query", query.clone()),
                ("status", JsValue::from_str("loading")),
                ("items", Array::new().into()),
                ("hasMore", JsValue::FALSE),
            ])?
            .into(),
        )?;
        let timer_query = query.clone();
        let timer_search = search.clone();
        let timer_signal = signal.clone();
        let success_setter = set_remote.clone();
        let failure_setter = set_remote.clone();
        let callback = Closure::wrap(Box::new(move || {
            let Ok(result) = timer_search.call2(&JsValue::UNDEFINED, &timer_query, &timer_signal)
            else {
                if Reflect::get(&timer_signal, &JsValue::from_str("aborted"))
                    .ok()
                    .and_then(|value| value.as_bool())
                    != Some(true)
                {
                    let _ = failure_setter.call1(
                        &JsValue::UNDEFINED,
                        &object(&[
                            ("query", timer_query.clone()),
                            ("status", JsValue::from_str("error")),
                            ("items", Array::new().into()),
                            ("hasMore", JsValue::FALSE),
                        ])
                        .map(JsValue::from)
                        .unwrap_or(JsValue::UNDEFINED),
                    );
                }
                return;
            };
            let success_query = timer_query.clone();
            let success_signal = timer_signal.clone();
            let success_setter = success_setter.clone();
            let success = Closure::wrap(Box::new(move |result: JsValue| {
                if Reflect::get(&success_signal, &JsValue::from_str("aborted"))
                    .ok()
                    .and_then(|value| value.as_bool())
                    == Some(true)
                {
                    return;
                }
                let items = Reflect::get(&result, &JsValue::from_str("items"))
                    .unwrap_or_else(|_| Array::new().into());
                let has_more =
                    Reflect::get(&result, &JsValue::from_str("hasMore")).unwrap_or(JsValue::FALSE);
                if let Ok(state) = object(&[
                    ("query", success_query.clone()),
                    ("status", JsValue::from_str("ready")),
                    ("items", items),
                    ("hasMore", has_more),
                ]) {
                    let _ = success_setter.call1(&JsValue::UNDEFINED, &state);
                }
            }) as Box<dyn FnMut(JsValue)>);
            let failure_query = timer_query.clone();
            let failure_signal = timer_signal.clone();
            let failure_setter = failure_setter.clone();
            let failure = Closure::wrap(Box::new(move |_reason: JsValue| {
                if Reflect::get(&failure_signal, &JsValue::from_str("aborted"))
                    .ok()
                    .and_then(|value| value.as_bool())
                    == Some(true)
                {
                    return;
                }
                if let Ok(state) = object(&[
                    ("query", failure_query.clone()),
                    ("status", JsValue::from_str("error")),
                    ("items", Array::new().into()),
                    ("hasMore", JsValue::FALSE),
                ]) {
                    let _ = failure_setter.call1(&JsValue::UNDEFINED, &state);
                }
            }) as Box<dyn FnMut(JsValue)>);
            attach_promise(&result, success, failure);
        }) as Box<dyn FnMut()>);
        let callback_value = callback.into_js_value();
        let timer = set_timeout(&callback_value, SEARCH_DEBOUNCE_MS)?;
        let cleanup_controller = controller.clone();
        Ok(Closure::wrap(Box::new(move || {
            clear_timeout(&timer);
            let _ = call(&cleanup_controller, "abort", &[]);
        }) as Box<dyn FnMut()>)
        .into_js_value())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    use_effect(
        &modules.react,
        effect.into_js_value(),
        &Array::of2(normalized, search_dependency.as_ref()),
    )
}

#[allow(clippy::too_many_arguments)]
fn dialog_input(
    modules: &BrowserModules,
    class_name: &str,
    value: &JsValue,
    label: JsValue,
    disabled: bool,
    composing: &JsValue,
    set_value: Function,
    clear_error: Function,
    confirm: Function,
) -> Result<JsValue, JsValue> {
    let focus = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        call(&required(&event, "target", "focus event")?, "select", &[])?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let change_setter = set_value;
    let change_clear = clear_error;
    let change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        change_setter.call1(&JsValue::UNDEFINED, &target_value(&event)?)?;
        change_clear.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    let composing_start = composing.clone();
    let start = Closure::wrap(Box::new(move || {
        let _ = set_current(&composing_start, &JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let composing_end = composing.clone();
    let end = Closure::wrap(Box::new(move || {
        let _ = set_current(&composing_end, &JsValue::FALSE);
    }) as Box<dyn FnMut()>);
    let key_composing = composing.clone();
    let key_confirm = confirm;
    let key_down = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
        if event_key(&event)? == "Enter" && current(&key_composing)?.as_bool() != Some(true) {
            call(&event, "preventDefault", &[])?;
            key_confirm.call0(&JsValue::UNDEFINED)?;
        }
        Ok(())
    }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
    tag(
        &modules.react,
        "input",
        Some(&object(&[
            ("className", JsValue::from_str(class_name)),
            ("value", value.clone()),
            ("aria-label", label),
            ("autoFocus", JsValue::TRUE),
            ("disabled", JsValue::from_bool(disabled)),
            ("onFocus", focus.into_js_value()),
            ("onChange", change.into_js_value()),
            ("onCompositionStart", start.into_js_value()),
            ("onCompositionEnd", end.into_js_value()),
            ("onKeyDown", key_down.into_js_value()),
        ])?),
        &[],
    )
}

fn footer(modules: &BrowserModules, buttons: &[JsValue]) -> Result<JsValue, JsValue> {
    element(
        &modules.react,
        &required(&modules.react, "Fragment", "React")?,
        None,
        buttons,
    )
}

fn button(
    modules: &BrowserModules,
    variant: &str,
    class_name: Option<&str>,
    disabled: bool,
    on_click: JsValue,
    label: JsValue,
) -> Result<JsValue, JsValue> {
    let mut props = vec![
        ("variant", JsValue::from_str(variant)),
        ("disabled", JsValue::from_bool(disabled)),
        ("onClick", on_click),
    ];
    if let Some(class_name) = class_name {
        props.push(("className", JsValue::from_str(class_name)));
    }
    element(
        &modules.react,
        &modules.primitive("Button")?,
        Some(&object(&props)?),
        &[label],
    )
}

#[allow(clippy::too_many_lines)] // The source owner coordinates chrome, search, flow, lists, and three durable-action dialogs.
fn render_workspace_browser(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let wide = bool_property(props, "wide", "WorkspaceBrowser props")?;
    let translate = function(props, "t", "WorkspaceBrowser props")?;
    let workspace_snapshot = function(props, "useWorkspaces", "WorkspaceBrowser props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let workspace_items = required(&workspace_snapshot, "items", "Workspace snapshot")?;
    let source_workspaces = workspaces(&workspace_items)?;
    let workspace_phase = string_property(&workspace_snapshot, "phase", "Workspace snapshot")?;
    let archived = required(
        &workspace_snapshot,
        "archivedSessionIds",
        "Workspace snapshot",
    )?;
    let directory_available = function(props, "useDirectoryFlow", "WorkspaceBrowser props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?
        .as_bool()
        .unwrap_or(false);
    let store = function(props, "useStore", "WorkspaceBrowser props")?
        .call1(&JsValue::UNDEFINED, &identity_selector())?;
    let group_by = string_property(&store, "groupBy", "Workspace view store")?;
    let order_by = string_property(&store, "orderBy", "Workspace view store")?;
    let group_expansion = required(&store, "groupExpansion", "Workspace view store")?;
    let session_orders = required(&store, "sessionOrderByAccount", "Workspace view store")?;
    let session_timestamps = required(&store, "sessionUpdatedAtByAccount", "Workspace view store")?;
    let actions = required(props, "actions", "WorkspaceBrowser props")?;
    retain_accounts_effect(
        modules,
        &workspace_snapshot,
        &workspace_phase,
        source_workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.as_str().to_owned())
            .collect(),
        function(&actions, "retainAccountKeys", "Workspace view actions")?,
    )?;

    let (query, set_query) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (search_expanded, set_search_expanded) = use_state(&modules.react, &JsValue::FALSE)?;
    let search_expanded = search_expanded.as_bool().unwrap_or(false);
    let normalized = normalized_query(&query)?;
    let (remote_search, set_remote_search) = use_state(
        &modules.react,
        &object(&[
            ("query", JsValue::from_str("")),
            ("status", JsValue::from_str("idle")),
            ("items", Array::new().into()),
            ("hasMore", JsValue::FALSE),
        ])?
        .into(),
    )?;
    let search_root = use_ref(&modules.react, &JsValue::NULL)?;
    let search_input = use_ref(&modules.react, &JsValue::NULL)?;
    let (picker_open, set_picker_open) = use_state(&modules.react, &JsValue::FALSE)?;
    let picker_open = picker_open.as_bool().unwrap_or(false);
    let plus_ref = use_ref(&modules.react, &JsValue::NULL)?;
    let composing = use_ref(&modules.react, &JsValue::FALSE)?;
    let (search_on_expand, set_search_on_expand) = use_state(&modules.react, &JsValue::FALSE)?;
    let search_on_expand = search_on_expand.as_bool().unwrap_or(false);
    search_focus_effects(
        modules,
        wide,
        search_expanded,
        search_on_expand,
        &set_search_on_expand,
        &search_input,
        &normalized,
        &search_root,
        &set_search_expanded,
    )?;
    remote_search_effect(
        modules,
        &normalized,
        function(props, "searchSessions", "WorkspaceBrowser props")?,
        set_remote_search,
    )?;

    let (rename_target, set_rename_target) = use_state(&modules.react, &JsValue::NULL)?;
    let (rename_draft, set_rename_draft) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (renaming, set_renaming) = use_state(&modules.react, &JsValue::FALSE)?;
    let renaming = renaming.as_bool().unwrap_or(false);
    let (rename_error, set_rename_error) = use_state(&modules.react, &JsValue::NULL)?;
    let rename_trimmed = string_method(&rename_draft, "trim", &[])?;
    let rename_trimmed_text = rename_trimmed.as_string().unwrap_or_default();
    let rename_current = if rename_target.is_null() {
        None
    } else {
        Some(string_property(
            &rename_target,
            "currentTitle",
            "Workspace rename target",
        )?)
    };
    let rename_duplicate = rename_current.as_ref().is_some_and(|current| {
        !rename_trimmed_text.is_empty()
            && rename_trimmed_text != *current
            && source_workspaces
                .iter()
                .any(|workspace| workspace.title == rename_trimmed_text)
    });
    let rename_blocked = renaming
        || rename_trimmed_text.is_empty()
        || rename_target.is_null()
        || rename_current.as_deref() == Some(rename_trimmed_text.as_str())
        || rename_duplicate;

    let (session_target, set_session_target) = use_state(&modules.react, &JsValue::NULL)?;
    let (session_draft, set_session_draft) = use_state(&modules.react, &JsValue::from_str(""))?;
    let (session_renaming, set_session_renaming) = use_state(&modules.react, &JsValue::FALSE)?;
    let session_renaming = session_renaming.as_bool().unwrap_or(false);
    let (session_error, set_session_error) = use_state(&modules.react, &JsValue::NULL)?;
    let session_trimmed = string_method(&session_draft, "trim", &[])?;
    let session_trimmed_text = session_trimmed.as_string().unwrap_or_default();
    let session_blocked =
        session_renaming || session_trimmed_text.is_empty() || session_target.is_null();

    let (delete_target, set_delete_target) = use_state(&modules.react, &JsValue::NULL)?;
    let (deleting, set_deleting) = use_state(&modules.react, &JsValue::FALSE)?;
    let deleting = deleting.as_bool().unwrap_or(false);
    let (delete_committed, set_delete_committed) = use_state(&modules.react, &JsValue::NULL)?;
    let (delete_error, set_delete_error) = use_state(&modules.react, &JsValue::NULL)?;
    let delete_effect_target = delete_committed.clone();
    let delete_effect_workspaces = source_workspaces.clone();
    let delete_effect_deleting = set_deleting.clone();
    let delete_effect_committed = set_delete_committed.clone();
    let delete_effect_target_setter = set_delete_target.clone();
    let delete_effect = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        let Some(id) = delete_effect_target.as_string() else {
            return Ok(());
        };
        if delete_effect_workspaces
            .iter()
            .any(|workspace| workspace.workspace_id.as_str() == id)
        {
            return Ok(());
        }
        delete_effect_deleting.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        delete_effect_committed.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        delete_effect_target_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    use_effect(
        &modules.react,
        delete_effect.into_js_value(),
        &Array::of2(&delete_committed, &workspace_items),
    )?;

    let close_rename_target = set_rename_target.clone();
    let close_rename_error = set_rename_error.clone();
    let close_rename = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !renaming {
            close_rename_target.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            close_rename_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let confirm_rename_action = function(props, "renameWorkspace", "WorkspaceBrowser props")?;
    let confirm_rename_target = rename_target.clone();
    let confirm_rename_draft = rename_trimmed.clone();
    let confirm_renaming = set_renaming.clone();
    let confirm_error = set_rename_error.clone();
    let confirm_target_setter = set_rename_target.clone();
    let confirm_rename = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if rename_blocked {
            return Ok(());
        }
        confirm_renaming.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        confirm_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        let workspace_id = required(
            &confirm_rename_target,
            "workspaceId",
            "Workspace rename target",
        )?;
        let result = confirm_rename_action.call2(
            &JsValue::UNDEFINED,
            &workspace_id,
            &confirm_rename_draft,
        )?;
        let success_renaming = confirm_renaming.clone();
        let success_target = confirm_target_setter.clone();
        let success = Closure::wrap(Box::new(move |_value: JsValue| {
            let _ = success_renaming.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = success_target.call1(&JsValue::UNDEFINED, &JsValue::NULL);
        }) as Box<dyn FnMut(JsValue)>);
        let failure_renaming = confirm_renaming.clone();
        let failure_error = confirm_error.clone();
        let failure = Closure::wrap(Box::new(move |reason: JsValue| {
            let _ = failure_renaming.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = failure_error.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&rejection_text(&reason)),
            );
        }) as Box<dyn FnMut(JsValue)>);
        attach_promise(&result, success, failure);
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();

    let close_session_target = set_session_target.clone();
    let close_session_error = set_session_error.clone();
    let close_session = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !session_renaming {
            close_session_target.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            close_session_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let confirm_session_action = function(props, "renameSession", "WorkspaceBrowser props")?;
    let confirm_session_target = session_target.clone();
    let confirm_session_draft = session_trimmed.clone();
    let confirm_session_renaming = set_session_renaming.clone();
    let confirm_session_error = set_session_error.clone();
    let confirm_session_target_setter = set_session_target.clone();
    let confirm_session = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if session_blocked {
            return Ok(());
        }
        confirm_session_renaming.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        confirm_session_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        let session_id = required(
            &confirm_session_target,
            "sessionId",
            "Session rename target",
        )?;
        let result = confirm_session_action.call2(
            &JsValue::UNDEFINED,
            &session_id,
            &confirm_session_draft,
        )?;
        let success_renaming = confirm_session_renaming.clone();
        let success_target = confirm_session_target_setter.clone();
        let success = Closure::wrap(Box::new(move |_value: JsValue| {
            let _ = success_renaming.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = success_target.call1(&JsValue::UNDEFINED, &JsValue::NULL);
        }) as Box<dyn FnMut(JsValue)>);
        let failure_renaming = confirm_session_renaming.clone();
        let failure_error = confirm_session_error.clone();
        let failure = Closure::wrap(Box::new(move |reason: JsValue| {
            let _ = failure_renaming.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = failure_error.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&rejection_text(&reason)),
            );
        }) as Box<dyn FnMut(JsValue)>);
        attach_promise(&result, success, failure);
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();

    let close_delete_target = set_delete_target.clone();
    let close_delete_error = set_delete_error.clone();
    let close_delete = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if !deleting {
            close_delete_target.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            close_delete_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        }
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let confirm_delete_action = function(props, "deleteWorkspace", "WorkspaceBrowser props")?;
    let confirm_delete_target = delete_target.clone();
    let confirm_deleting = set_deleting.clone();
    let confirm_committed = set_delete_committed.clone();
    let confirm_error = set_delete_error.clone();
    let confirm_delete = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if deleting || confirm_delete_target.is_null() {
            return Ok(());
        }
        confirm_deleting.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
        confirm_committed.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        confirm_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
        let workspace_id = required(
            &confirm_delete_target,
            "workspaceId",
            "Workspace delete target",
        )?;
        let result = confirm_delete_action.call1(&JsValue::UNDEFINED, &workspace_id)?;
        let success_committed = confirm_committed.clone();
        let success_id = workspace_id.clone();
        let success = Closure::wrap(Box::new(move |_value: JsValue| {
            let _ = success_committed.call1(&JsValue::UNDEFINED, &success_id);
        }) as Box<dyn FnMut(JsValue)>);
        let failure_deleting = confirm_deleting.clone();
        let failure_error = confirm_error.clone();
        let failure = Closure::wrap(Box::new(move |reason: JsValue| {
            let _ = failure_deleting.call1(&JsValue::UNDEFINED, &JsValue::FALSE);
            let _ = failure_error.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str(&rejection_text(&reason)),
            );
        }) as Box<dyn FnMut(JsValue)>);
        attach_promise(&result, success, failure);
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();

    let session_rename_target_setter = set_session_target.clone();
    let session_rename_draft_setter = set_session_draft.clone();
    let session_rename_error_setter = set_session_error.clone();
    let on_session_rename = Closure::wrap(Box::new(
        move |session_id: String, current_title: String| -> Result<(), JsValue> {
            session_rename_target_setter.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("sessionId", JsValue::from_str(&session_id)),
                    ("currentTitle", JsValue::from_str(&current_title)),
                ])?
                .into(),
            )?;
            session_rename_draft_setter
                .call1(&JsValue::UNDEFINED, &JsValue::from_str(&current_title))?;
            session_rename_error_setter.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            Ok(())
        },
    )
        as Box<dyn FnMut(String, String) -> Result<(), JsValue>>)
    .into_js_value();
    let archive_action = function(props, "archiveSession", "WorkspaceBrowser props")?;
    let on_archive = Closure::wrap(Box::new(move |session_id: String| -> Result<(), JsValue> {
        let result = archive_action.call1(&JsValue::UNDEFINED, &JsValue::from_str(&session_id))?;
        warn_rejection(&result, "session archive rejected:");
        Ok(())
    }) as Box<dyn FnMut(String) -> Result<(), JsValue>>)
    .into_js_value();
    let workspace_rename_target = set_rename_target.clone();
    let workspace_rename_draft = set_rename_draft.clone();
    let workspace_rename_error = set_rename_error.clone();
    let on_workspace_rename = Closure::wrap(Box::new(
        move |workspace_id: String, current_title: String| -> Result<(), JsValue> {
            workspace_rename_target.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("workspaceId", JsValue::from_str(&workspace_id)),
                    ("currentTitle", JsValue::from_str(&current_title)),
                ])?
                .into(),
            )?;
            workspace_rename_draft
                .call1(&JsValue::UNDEFINED, &JsValue::from_str(&current_title))?;
            workspace_rename_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            Ok(())
        },
    )
        as Box<dyn FnMut(String, String) -> Result<(), JsValue>>)
    .into_js_value();
    let workspace_delete_target = set_delete_target.clone();
    let workspace_delete_error = set_delete_error.clone();
    let on_workspace_delete = Closure::wrap(Box::new(
        move |workspace_id: String, title: String| -> Result<(), JsValue> {
            workspace_delete_target.call1(
                &JsValue::UNDEFINED,
                &object(&[
                    ("workspaceId", JsValue::from_str(&workspace_id)),
                    ("title", JsValue::from_str(&title)),
                ])?
                .into(),
            )?;
            workspace_delete_error.call1(&JsValue::UNDEFINED, &JsValue::NULL)?;
            Ok(())
        },
    )
        as Box<dyn FnMut(String, String) -> Result<(), JsValue>>)
    .into_js_value();

    let picker_setter = set_picker_open.clone();
    let picker_toggle = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        picker_setter.call1(&JsValue::UNDEFINED, &JsValue::from_bool(!picker_open))?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let mut header_actions = Vec::new();
    if wide {
        let group_action = function(&actions, "setGroupBy", "Workspace view actions")?;
        let on_group = Closure::wrap(Box::new(move |mode: String| -> Result<(), JsValue> {
            group_action.call1(&JsValue::UNDEFINED, &JsValue::from_str(&mode))?;
            Ok(())
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
        let order_action = function(&actions, "setOrderBy", "Workspace view actions")?;
        let on_order = Closure::wrap(Box::new(move |mode: String| -> Result<(), JsValue> {
            order_action.call1(&JsValue::UNDEFINED, &JsValue::from_str(&mode))?;
            Ok(())
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
        header_actions.push(element(
            &modules.react,
            &internals()?.view_options,
            Some(&object(&[
                ("groupBy", JsValue::from_str(&group_by)),
                ("orderBy", JsValue::from_str(&order_by)),
                ("onGroupPick", on_group.into_js_value()),
                ("onOrderPick", on_order.into_js_value()),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?);
    }
    if directory_available {
        let add_button = tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("ref", plus_ref.clone()),
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class(&[("iconButton", true)])),
                ),
                ("aria-label", translated(&translate, "workspace.add", None)?),
                ("onClick", picker_toggle.into_js_value()),
            ])?),
            &[icon(
                modules,
                "IconProjectAddOutline16",
                Some(&object(&[(
                    "size",
                    JsValue::from_f64(if wide { 16.0 } else { 18.0 }),
                )])?),
            )?],
        )?;
        header_actions.push(tooltip(
            modules,
            translated(&translate, "workspace.add", None)?,
            Some("bottom"),
            Some(500.0),
            None,
            add_button,
        )?);
    }

    let picker_close_setter = set_picker_open.clone();
    let picker_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        picker_close_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let picker_pick_setter = set_picker_open.clone();
    let picker_start = function(props, "startSession", "WorkspaceBrowser props")?;
    let picker_pick = Closure::wrap(
        Box::new(move |workspace_id: String| -> Result<(), JsValue> {
            picker_pick_setter.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            picker_start.call1(&JsValue::UNDEFINED, &JsValue::from_str(&workspace_id))?;
            Ok(())
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>,
    );
    let render_slot = function(props, "renderSlot", "WorkspaceBrowser props")?;
    let render_flow = Closure::wrap(Box::new(move |owner: JsValue| {
        render_slot.call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("sidebar.workspaces.directoryFlow"),
            &owner,
        )
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let picker = element(
        &modules.react,
        &workspace_pick_flow_component()?,
        Some(&object(&[
            ("t", translate.clone().into()),
            ("open", JsValue::from_bool(picker_open)),
            ("anchorRef", plus_ref.clone()),
            (
                "useWorkspaces",
                required(props, "useWorkspaces", "WorkspaceBrowser props")?,
            ),
            (
                "createWorkspace",
                required(props, "createWorkspace", "WorkspaceBrowser props")?,
            ),
            (
                "useDirectoryFlow",
                required(props, "useDirectoryFlow", "WorkspaceBrowser props")?,
            ),
            ("renderDirectoryFlow", render_flow.into_js_value()),
            ("addOnly", JsValue::TRUE),
            ("side", JsValue::from_str("right")),
            ("onPick", picker_pick.into_js_value()),
            ("onClose", picker_close.into_js_value()),
        ])?),
        &[],
    )?;

    let mut header_children = Vec::new();
    if wide {
        header_children.push(tag(
            &modules.react,
            "span",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[
                    ("sectionLabel", true),
                    ("wide", true),
                    ("sectionLabelHidden", search_expanded),
                ])),
            )])?),
            &[translated(
                &translate,
                if group_by == "flat" {
                    "section.sessions"
                } else {
                    "section.workspaces"
                },
                None,
            )?],
        )?);
        let search_set_picker = set_picker_open.clone();
        let search_set_expanded = set_search_expanded.clone();
        let search_focus = search_input.clone();
        let search_click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            search_set_picker.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            search_set_expanded.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            focus_ref(&search_focus, false)
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let search_button_picker = set_picker_open.clone();
        let search_button_expanded = set_search_expanded.clone();
        let search_button = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            search_button_picker.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            search_button_expanded.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let search_button_node = tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class(&[("searchButton", true)])),
                ),
                (
                    "aria-label",
                    translated(&translate, "search.sessions.aria", None)?,
                ),
                ("aria-expanded", JsValue::from_bool(search_expanded)),
                ("onClick", search_button.into_js_value()),
            ])?),
            &[icon(
                modules,
                "IconSearchOutline16",
                Some(&object(&[(
                    "size",
                    JsValue::from_f64(if search_expanded { 11.0 } else { 14.0 }),
                )])?),
            )?],
        )?;
        let search_tooltip = tooltip(
            modules,
            translated(&translate, "search", None)?,
            Some("bottom"),
            Some(500.0),
            Some(search_expanded),
            search_button_node,
        )?;
        let query_setter = set_query.clone();
        let on_change = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            query_setter.call1(
                &JsValue::UNDEFINED,
                &sanitize_search_query(&target_value(&event)?)?,
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let escape_query = set_query.clone();
        let escape_expanded = set_search_expanded.clone();
        let on_key = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
            if event_key(&event)? == "Escape" {
                escape_query.call1(&JsValue::UNDEFINED, &JsValue::from_str(""))?;
                escape_expanded.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
            }
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        let input = tag(
            &modules.react,
            "input",
            Some(&object(&[
                ("ref", search_input.clone()),
                (
                    "className",
                    JsValue::from_str(&class(&[("searchInput", true)])),
                ),
                ("type", JsValue::from_str("text")),
                (
                    "placeholder",
                    translated(&translate, "search.placeholder", None)?,
                ),
                (
                    "maxLength",
                    JsValue::from_f64(SEARCH_QUERY_MAX_CODE_UNITS_F64),
                ),
                ("value", query.clone()),
                (
                    "tabIndex",
                    JsValue::from_f64(if search_expanded { 0.0 } else { -1.0 }),
                ),
                ("onChange", on_change.into_js_value()),
                ("onKeyDown", on_key.into_js_value()),
            ])?),
            &[],
        )?;
        let mut search_children = vec![search_tooltip, input];
        if search_expanded {
            let clear_query = set_query.clone();
            let clear_expanded = set_search_expanded.clone();
            let clear = Closure::wrap(Box::new(move |event: JsValue| -> Result<(), JsValue> {
                call(&event, "stopPropagation", &[])?;
                clear_query.call1(&JsValue::UNDEFINED, &JsValue::from_str(""))?;
                clear_expanded.call1(&JsValue::UNDEFINED, &JsValue::FALSE)?;
                Ok(())
            })
                as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
            search_children.push(tag(
                &modules.react,
                "button",
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str(&class(&[("clearButton", true)])),
                    ),
                    ("aria-label", translated(&translate, "search.clear", None)?),
                    ("onClick", clear.into_js_value()),
                ])?),
                &[icon(modules, "IconCloseFill14", None)?],
            )?);
        }
        let search = tag(
            &modules.react,
            "div",
            Some(&object(&[
                ("ref", search_root.clone()),
                (
                    "className",
                    JsValue::from_str(&class(&[
                        ("search", true),
                        ("searchExpanded", search_expanded),
                    ])),
                ),
                ("onClick", search_click.into_js_value()),
            ])?),
            &search_children,
        )?;
        header_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[
                    ("searchSlot", true),
                    ("searchSlotExpanded", search_expanded),
                ])),
            )])?),
            &[search],
        )?);
    }
    header_children.push(tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[
                ("headerActions", true),
                ("headerActionsHidden", wide && search_expanded),
            ])),
        )])?),
        &header_actions,
    )?);
    header_children.push(picker);
    let header = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("sectionHeader", true)])),
        )])?),
        &header_children,
    )?;

    let rail_search = if wide {
        None
    } else {
        let rail_expanded = set_search_expanded.clone();
        let rail_on_expand = set_search_on_expand.clone();
        let expand = function(props, "expandSidebar", "WorkspaceBrowser props")?;
        let click = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
            rail_expanded.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            rail_on_expand.call1(&JsValue::UNDEFINED, &JsValue::TRUE)?;
            expand.call0(&JsValue::UNDEFINED)?;
            Ok(())
        }) as Box<dyn FnMut() -> Result<(), JsValue>>);
        let rail_button = tag(
            &modules.react,
            "button",
            Some(&object(&[
                ("type", JsValue::from_str("button")),
                (
                    "className",
                    JsValue::from_str(&class(&[("searchButton", true)])),
                ),
                (
                    "aria-label",
                    translated(&translate, "search.sessions.aria", None)?,
                ),
                ("onClick", click.into_js_value()),
            ])?),
            &[icon(
                modules,
                "IconSearchOutline16",
                Some(&object(&[("size", JsValue::from_f64(18.0))])?),
            )?],
        )?;
        Some(tag(
            &modules.react,
            "div",
            Some(&object(&[(
                "className",
                JsValue::from_str(&class(&[("search", true)])),
            )])?),
            &[tooltip(
                modules,
                translated(&translate, "search", None)?,
                None,
                None,
                None,
                rail_button,
            )?],
        )?)
    };

    let normalized_text = normalized.as_string().unwrap_or_default();
    let lists = internals()?.lists;
    let list_content = if !wide {
        None
    } else if !normalized_text.is_empty() {
        Some(element(
            &modules.react,
            &lists.search,
            Some(&object(&[
                (
                    "useSessions",
                    required(props, "useSessions", "WorkspaceBrowser props")?,
                ),
                ("open", required(props, "open", "WorkspaceBrowser props")?),
                ("workspaces", workspace_items.clone()),
                ("archivedSessionIds", archived.clone()),
                ("query", normalized.clone()),
                ("remote", remote_search.clone()),
                (
                    "resultLimit",
                    required(props, "searchResultLimit", "WorkspaceBrowser props")?,
                ),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?)
    } else if group_by == "flat" {
        Some(element(
            &modules.react,
            &lists.flat,
            Some(&object(&[
                (
                    "useSessions",
                    required(props, "useSessions", "WorkspaceBrowser props")?,
                ),
                ("open", required(props, "open", "WorkspaceBrowser props")?),
                (
                    "forkSession",
                    required(props, "forkSession", "WorkspaceBrowser props")?,
                ),
                ("onSessionRename", on_session_rename.clone()),
                ("onSessionArchive", on_archive.clone()),
                ("archivedSessionIds", archived.clone()),
                ("orderBy", JsValue::from_str(&order_by)),
                ("sessionOrderByAccount", session_orders.clone()),
                ("sessionUpdatedAtByAccount", session_timestamps.clone()),
                (
                    "syncSessionOrderAccount",
                    required(
                        &actions,
                        "syncSessionOrderAccount",
                        "Workspace view actions",
                    )?,
                ),
                (
                    "setSessionOrder",
                    required(&actions, "setSessionOrder", "Workspace view actions")?,
                ),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?)
    } else {
        Some(element(
            &modules.react,
            &lists.tree,
            Some(&object(&[
                (
                    "useSessions",
                    required(props, "useSessions", "WorkspaceBrowser props")?,
                ),
                (
                    "startSession",
                    required(props, "startSession", "WorkspaceBrowser props")?,
                ),
                ("open", required(props, "open", "WorkspaceBrowser props")?),
                (
                    "forkSession",
                    required(props, "forkSession", "WorkspaceBrowser props")?,
                ),
                ("workspaces", workspace_items.clone()),
                ("groupExpansion", group_expansion.clone()),
                (
                    "setGroupExpanded",
                    required(&actions, "setGroupExpanded", "Workspace view actions")?,
                ),
                ("sessionOrderByAccount", session_orders.clone()),
                ("sessionUpdatedAtByAccount", session_timestamps.clone()),
                (
                    "syncSessionOrderAccount",
                    required(
                        &actions,
                        "syncSessionOrderAccount",
                        "Workspace view actions",
                    )?,
                ),
                (
                    "setSessionOrder",
                    required(&actions, "setSessionOrder", "Workspace view actions")?,
                ),
                ("archivedSessionIds", archived.clone()),
                ("onRenameRequest", on_workspace_rename),
                ("onDeleteRequest", on_workspace_delete),
                ("onSessionRename", on_session_rename),
                ("onSessionArchive", on_archive),
                (
                    "insertWorkspaceBefore",
                    required(props, "insertWorkspaceBefore", "WorkspaceBrowser props")?,
                ),
                (
                    "insertSessionBefore",
                    required(props, "insertSessionBefore", "WorkspaceBrowser props")?,
                ),
                ("orderBy", JsValue::from_str(&order_by)),
                ("t", translate.clone().into()),
            ])?),
            &[],
        )?)
    };
    let list_area = tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("listArea", true)])),
        )])?),
        &list_content.into_iter().collect::<Vec<_>>(),
    )?;

    let rename_confirm = confirm_rename.clone().dyn_into::<Function>()?;
    let rename_input = dialog_input(
        modules,
        &class(&[("renameInput", true)]),
        &rename_draft,
        translated(&translate, "field.workspaceName", None)?,
        renaming,
        &composing,
        set_rename_draft,
        set_rename_error.clone(),
        rename_confirm,
    )?;
    let mut rename_children = vec![rename_input];
    if rename_duplicate {
        rename_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("renameError", true)])),
                ),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[translated(
                &translate,
                "conflict.named",
                Some(&object(&[(
                    "name",
                    JsValue::from_str(&rename_trimmed_text),
                )])?),
            )?],
        )?);
    }
    if !rename_error.is_null() {
        rename_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("renameError", true)])),
                ),
                ("role", JsValue::from_str("alert")),
            ])?),
            std::slice::from_ref(&rename_error),
        )?);
    }
    let rename_footer = footer(
        modules,
        &[
            button(
                modules,
                "outline",
                None,
                renaming,
                close_rename.clone(),
                translated(&translate, "cancel", None)?,
            )?,
            button(
                modules,
                "primary",
                None,
                rename_blocked,
                confirm_rename.clone(),
                translated(&translate, "rename", None)?,
            )?,
        ],
    )?;
    let rename_modal = element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(!rename_target.is_null())),
            ("onClose", close_rename),
            ("closeLabel", translated(&translate, "close", None)?),
            (
                "title",
                translated(&translate, "rename.workspace.title", None)?,
            ),
            ("footer", rename_footer),
        ])?),
        &rename_children,
    )?;

    let session_confirm = confirm_session.clone().dyn_into::<Function>()?;
    let session_input = dialog_input(
        modules,
        &class(&[("renameInput", true)]),
        &session_draft,
        translated(&translate, "field.sessionName", None)?,
        session_renaming,
        &composing,
        set_session_draft,
        set_session_error.clone(),
        session_confirm,
    )?;
    let mut session_children = vec![session_input];
    if !session_error.is_null() {
        session_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("renameError", true)])),
                ),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[session_error],
        )?);
    }
    let session_footer = footer(
        modules,
        &[
            button(
                modules,
                "outline",
                None,
                session_renaming,
                close_session.clone(),
                translated(&translate, "cancel", None)?,
            )?,
            button(
                modules,
                "primary",
                None,
                session_blocked,
                confirm_session.clone(),
                translated(&translate, "rename", None)?,
            )?,
        ],
    )?;
    let session_modal = element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&[
            ("open", JsValue::from_bool(!session_target.is_null())),
            ("onClose", close_session),
            ("closeLabel", translated(&translate, "close", None)?),
            (
                "title",
                translated(&translate, "rename.session.title", None)?,
            ),
            ("footer", session_footer),
        ])?),
        &session_children,
    )?;

    let mut delete_footer_buttons = vec![button(
        modules,
        "outline",
        None,
        deleting,
        close_delete.clone(),
        translated(&translate, "cancel", None)?,
    )?];
    delete_footer_buttons.push(button(
        modules,
        "outline",
        Some(&class(&[("deleteAction", true)])),
        deleting,
        confirm_delete.clone(),
        translated(&translate, "delete.workspace", None)?,
    )?);
    let delete_footer = footer(modules, &delete_footer_buttons)?;
    let mut delete_children = Vec::new();
    if deleting {
        delete_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("deleteStatus", true)])),
                ),
                ("role", JsValue::from_str("status")),
            ])?),
            &[translated(&translate, "delete.pending", None)?],
        )?);
    }
    if !delete_error.is_null() {
        delete_children.push(tag(
            &modules.react,
            "div",
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str(&class(&[("renameError", true)])),
                ),
                ("role", JsValue::from_str("alert")),
            ])?),
            &[delete_error],
        )?);
    }
    let mut delete_props = vec![
        ("open", JsValue::from_bool(!delete_target.is_null())),
        ("onClose", close_delete),
        ("closeLabel", translated(&translate, "close", None)?),
        ("title", translated(&translate, "delete.workspace", None)?),
        ("footer", delete_footer),
    ];
    if !delete_target.is_null() {
        delete_props.push((
            "description",
            translated(
                &translate,
                "delete.desc",
                Some(&object(&[(
                    "name",
                    required(&delete_target, "title", "Workspace delete target")?,
                )])?),
            )?,
        ));
    }
    let delete_modal = element(
        &modules.react,
        &modules.primitive("Modal")?,
        Some(&object(&delete_props)?),
        &delete_children,
    )?;

    let mut root_children = vec![header];
    if let Some(rail_search) = rail_search {
        root_children.push(rail_search);
    }
    root_children.extend([list_area, rename_modal, session_modal, delete_modal]);
    tag(
        &modules.react,
        "div",
        Some(&object(&[(
            "className",
            JsValue::from_str(&class(&[("root", true), ("rail", !wide)])),
        )])?),
        &root_children,
    )
}
