//! Compiled selected-Tool details panel.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{browser_reasoning::inject_style, find_tool_call_browser};

const DETAILS_CSS: &str = include_str!(
    "../../../packages/client/ui-conversation/src/client/skeleton/DetailsPanel.module.css"
);

thread_local! {
    static COMPONENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    fragment: JsValue,
    code_block: JsValue,
    shallow_equal: Function,
}

/// Configures the compiled selected-Tool details panel.
///
/// # Errors
///
/// Returns on missing React/ui-primitives/runtime faces or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiConversationDetailsPanel)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_details_panel(
    react: JsValue,
    ui_primitives: JsValue,
    shallow_equal: Function,
) -> Result<(), JsValue> {
    required_function(&react, "createElement", "React")?;
    let modules = BrowserModules {
        fragment: required_property(&react, "Fragment", "React")?,
        code_block: required_property(&ui_primitives, "CodeBlock", "ui-primitives")?,
        react,
        shallow_equal,
    };
    inject_style(
        "DetailsPanel",
        DETAILS_CSS,
        &[
            ("body", "seekdeep-conversation-details-body"),
            ("close", "seekdeep-conversation-details-close"),
            ("code", "seekdeep-conversation-details-code"),
            ("empty", "seekdeep-conversation-details-empty"),
            ("header", "seekdeep-conversation-details-header"),
            ("root", "seekdeep-conversation-details-root"),
            ("section", "seekdeep-conversation-details-section"),
            ("sectionLabel", "seekdeep-conversation-details-sectionLabel"),
            ("title", "seekdeep-conversation-details-title"),
        ],
    )?;
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_details_panel(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    COMPONENT.with(|configured| *configured.borrow_mut() = Some(component));
    Ok(())
}

/// Returns the compiled `DetailsPanel` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = detailsPanelComponent)]
pub fn details_panel_component() -> Result<JsValue, JsValue> {
    COMPONENT.with(|component| {
        component.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation DetailsPanel was not configured").into()
        })
    })
}

#[allow(clippy::too_many_lines)] // Closed panel tree and three selector calls stay auditable together.
fn render_details_panel(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let use_store = required_function(props, "useStore", "DetailsPanel props")?;
    let selection_selector = Closure::wrap(Box::new(move |snapshot: JsValue| {
        Reflect::get(&snapshot, &JsValue::from_str("selection"))
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let selection = use_store.call1(&JsValue::UNDEFINED, &selection_selector.into_js_value())?;
    let session_id = required_property(props, "sessionId", "DetailsPanel props")?;
    let cwd_session_id = session_id.clone();
    let cwd_selector = Closure::wrap(Box::new(move |list: JsValue| -> Result<JsValue, JsValue> {
        let by_id = required_property(&list, "byId", "session list")?;
        let session = Reflect::get(&by_id, &cwd_session_id)?;
        if session.is_null() || session.is_undefined() {
            Ok(JsValue::UNDEFINED)
        } else {
            Reflect::get(&session, &JsValue::from_str("cwd"))
        }
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let session_cwd = required_function(props, "useSessions", "DetailsPanel props")?
        .call1(&JsValue::UNDEFINED, &cwd_selector.into_js_value())?;
    let call_id = if selection.is_null() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(&selection, &JsValue::from_str("callId"))?
    };
    let material_call_id = call_id.as_string();
    let material_selector = Closure::wrap(Box::new(
        move |snapshot: JsValue| -> Result<JsValue, JsValue> {
            let Some(call_id) = material_call_id.as_ref() else {
                return Ok(JsValue::NULL);
            };
            material_for(snapshot, call_id)
        },
    )
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    let material = required_function(props, "useSession", "DetailsPanel props")?.call2(
        &JsValue::UNDEFINED,
        &material_selector.into_js_value(),
        modules.shallow_equal.as_ref(),
    )?;
    let translate = required_function(props, "t", "DetailsPanel props")?;
    let title = if selection.is_null() {
        translate_value(&translate, "details.title")?
    } else {
        let material_name = if material.is_null() || material.is_undefined() {
            JsValue::UNDEFINED
        } else {
            Reflect::get(&material, &JsValue::from_str("name"))?
        };
        if is_nullish(&material_name) {
            let tool_name = Reflect::get(&selection, &JsValue::from_str("toolName"))?;
            if is_nullish(&tool_name) {
                translate_value(&translate, "details.title")?
            } else {
                tool_name
            }
        } else {
            material_name
        }
    };
    let close = required_function(props, "closeDetails", "DetailsPanel props")?;
    let on_close = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        close.call0(&JsValue::UNDEFINED)?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>)
    .into_js_value();
    let header = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-details-header")?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-conversation-details-title")?),
                &[title],
            )?,
            create_element(
                &modules.react,
                &JsValue::from_str("button"),
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-conversation-details-close"),
                    ),
                    ("aria-label", translate_value(&translate, "details.close")?),
                    ("onClick", on_close),
                ])?),
                &[close_icon(modules)?],
            )?,
        ],
    )?;
    let body = if selection.is_null() || call_id.is_undefined() {
        empty(modules, translate_value(&translate, "details.empty")?)?
    } else if material.is_null() {
        empty(modules, translate_value(&translate, "details.notInWindow")?)?
    } else {
        render_material(
            modules,
            props,
            &translate,
            &material,
            &call_id,
            &session_cwd,
        )?
    };
    let body = create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-details-body")?),
        &[body],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-details-root")?),
        &[header, body],
    )
}

fn material_for(snapshot: JsValue, call_id: &str) -> Result<JsValue, JsValue> {
    let found = find_tool_call_browser(snapshot, call_id.to_owned())?;
    if found.is_undefined() {
        return Ok(JsValue::NULL);
    }
    if Reflect::has(&found, &JsValue::from_str("kind"))? {
        let call = Reflect::get(&found, &JsValue::from_str("call"))?;
        let (name, args_raw) = if is_nullish(&call) {
            (JsValue::from_str(call_id), JsValue::NULL)
        } else {
            let name = Reflect::get(&call, &JsValue::from_str("name"))?;
            let args = Reflect::get(&call, &JsValue::from_str("argsRaw"))?;
            (
                if is_nullish(&name) {
                    JsValue::from_str(call_id)
                } else {
                    name
                },
                if is_nullish(&args) {
                    JsValue::NULL
                } else {
                    args
                },
            )
        };
        Ok(object(&[("name", name), ("argsRaw", args_raw), ("block", found)])?.into())
    } else {
        Ok(object(&[
            ("name", Reflect::get(&found, &JsValue::from_str("name"))?),
            (
                "argsRaw",
                Reflect::get(&found, &JsValue::from_str("argsRaw"))?,
            ),
            ("block", found),
        ])?
        .into())
    }
}

fn render_material(
    modules: &BrowserModules,
    props: &JsValue,
    translate: &Function,
    material: &JsValue,
    call_id: &JsValue,
    session_cwd: &JsValue,
) -> Result<JsValue, JsValue> {
    let args_raw = Reflect::get(material, &JsValue::from_str("argsRaw"))?;
    let input = if args_raw.is_null() {
        JsValue::FALSE
    } else {
        let raw = args_raw.as_string().ok_or_else(|| {
            js_sys::TypeError::new("details material argsRaw must be a string or null")
        })?;
        section(
            modules,
            translate_value(translate, "details.input")?,
            create_element(
                &modules.react,
                &modules.code_block,
                Some(&object(&[
                    ("code", JsValue::from_str(&pretty(&raw))),
                    ("lang", JsValue::from_str("json")),
                    ("copyLabel", translate_value(translate, "copy")?),
                    ("copiedLabel", translate_value(translate, "copied")?),
                ])?),
                &[],
            )?,
        )?
    };
    let block = required_property(material, "block", "details material")?;
    let settled = Reflect::has(&block, &JsValue::from_str("kind"))?;
    let output_label = translate_value(translate, "details.output")?;
    let fallback = if settled {
        create_element(
            &modules.react,
            &JsValue::from_str("pre"),
            Some(&object(&[
                (
                    "className",
                    JsValue::from_str("seekdeep-conversation-details-code"),
                ),
                (
                    "data-error",
                    if Reflect::get(&block, &JsValue::from_str("isError"))?
                        .as_bool()
                        .unwrap_or(false)
                    {
                        JsValue::TRUE
                    } else {
                        JsValue::UNDEFINED
                    },
                ),
            ])?),
            &[JsValue::from_str(&raw_result_text(&block)?)],
        )?
    } else {
        empty(modules, translate_value(translate, "details.running")?)?
    };
    let rendered = required_function(props, "renderSlot", "DetailsPanel props")?.apply(
        &JsValue::UNDEFINED,
        &Array::of3(
            &JsValue::from_str("conversation.details.tool"),
            object(&[("block", block), ("cwd", session_cwd.clone())])?.as_ref(),
            object(&[("fallback", fallback)])?.as_ref(),
        ),
    )?;
    let keyed = create_element(
        &modules.react,
        &modules.fragment,
        Some(&object(&[("key", call_id.clone())])?),
        &[rendered],
    )?;
    let output = section(modules, output_label, keyed)?;
    create_element(&modules.react, &modules.fragment, None, &[input, output])
}

fn pretty(raw: &str) -> String {
    let Ok(parsed) = js_sys::JSON::parse(raw) else {
        return raw.to_owned();
    };
    js_sys::JSON::stringify_with_replacer_and_space(
        &parsed,
        &JsValue::NULL,
        &JsValue::from_f64(2.0),
    )
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_else(|| raw.to_owned())
}

fn raw_result_text(block: &JsValue) -> Result<String, JsValue> {
    if !Reflect::has(block, &JsValue::from_str("kind"))? {
        return Ok(String::new());
    }
    let content =
        required_property(block, "content", "settled Tool result")?.dyn_into::<Array>()?;
    let mut parts = Vec::new();
    for index in 0..content.length() {
        let item = content.get(index);
        if Reflect::get(&item, &JsValue::from_str("type"))?
            .as_string()
            .as_deref()
            == Some("text")
        {
            parts.push(
                required_property(&item, "text", "Tool text result")?
                    .as_string()
                    .ok_or_else(|| js_sys::TypeError::new("Tool result text must be a string"))?,
            );
        } else {
            parts.push(json_pretty(&item)?);
        }
    }
    let error = Reflect::get(block, &JsValue::from_str("error"))?;
    if parts.is_empty() && !error.is_undefined() {
        parts.push(format!(
            "{}: {}",
            javascript_string(&Reflect::get(&error, &JsValue::from_str("name"))?)?,
            javascript_string(&Reflect::get(&error, &JsValue::from_str("code"))?)?
        ));
    }
    Ok(parts.join("\n"))
}

fn json_pretty(value: &JsValue) -> Result<String, JsValue> {
    Ok(js_sys::JSON::stringify_with_replacer_and_space(
        value,
        &JsValue::NULL,
        &JsValue::from_f64(2.0),
    )?
    .as_string()
    .unwrap_or_default())
}

fn javascript_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String() returned a non-string").into())
}

fn close_icon(modules: &BrowserModules) -> Result<JsValue, JsValue> {
    let path = create_element(
        &modules.react,
        &JsValue::from_str("path"),
        Some(&object(&[
            ("d", JsValue::from_str("M4 4l8 8M12 4l-8 8")),
            ("stroke", JsValue::from_str("currentColor")),
            ("strokeWidth", JsValue::from_f64(1.5)),
            ("strokeLinecap", JsValue::from_str("round")),
        ])?),
        &[],
    )?;
    create_element(
        &modules.react,
        &JsValue::from_str("svg"),
        Some(&object(&[
            ("viewBox", JsValue::from_str("0 0 16 16")),
            ("width", JsValue::from_f64(14.0)),
            ("height", JsValue::from_f64(14.0)),
            ("aria-hidden", JsValue::TRUE),
        ])?),
        &[path],
    )
}

fn section(modules: &BrowserModules, label: JsValue, body: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("section"),
        Some(&class_props("seekdeep-conversation-details-section")?),
        &[
            create_element(
                &modules.react,
                &JsValue::from_str("div"),
                Some(&class_props("seekdeep-conversation-details-sectionLabel")?),
                &[label],
            )?,
            body,
        ],
    )
}

fn empty(modules: &BrowserModules, text: JsValue) -> Result<JsValue, JsValue> {
    create_element(
        &modules.react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-conversation-details-empty")?),
        &[text],
    )
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn translate_value(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

fn is_nullish(value: &JsValue) -> bool {
    value.is_null() || value.is_undefined()
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required_property(value, key, owner)?.dyn_into()
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if is_nullish(&property) {
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
