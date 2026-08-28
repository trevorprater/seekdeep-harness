//! Compiled highlighted/plain code surface with language and copy chrome.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{highlight_store_faces, highlight_to_html, write_clipboard};

const CODE_BLOCK_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/markdown/CodeBlock.module.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    subscribe: Function,
    snapshot: Function,
}

/// Configures React for the compiled `CodeBlock` over the shared highlighter.
///
/// # Errors
///
/// Returns before highlighter configuration or on stylesheet injection failure.
#[wasm_bindgen(js_name = configureClientUiPrimitiveCodeBlock)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_code_block(react: JsValue) -> Result<(), JsValue> {
    let (subscribe, snapshot) = highlight_store_faces()?;
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(BrowserModules {
            react,
            subscribe,
            snapshot,
        });
    });
    inject_style()
}

/// Returns the compiled `CodeBlock` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = codeBlockComponent)]
pub fn code_block_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(Closure::wrap(
        Box::new(move |props: JsValue| render_code_block(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    )
    .into_js_value())
}

#[allow(clippy::too_many_lines)]
fn render_code_block(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let code = required_string(props, "code", "CodeBlock props")?;
    let trimmed = code.strip_suffix('\n').unwrap_or(&code).to_owned();
    let lang = optional_string(props, "lang")?;
    let class_name = optional_string(props, "className")?;
    let copy_label = optional_string(props, "copyLabel")?.unwrap_or_else(|| "复制".to_owned());
    let copied_label =
        optional_string(props, "copiedLabel")?.unwrap_or_else(|| "复制成功".to_owned());
    let loaded = required_function(react, "useSyncExternalStore", "React")?.call3(
        react,
        modules.subscribe.as_ref(),
        modules.snapshot.as_ref(),
        modules.snapshot.as_ref(),
    )?;
    let highlight_code = trimmed.clone();
    let highlight_lang = lang.clone();
    let highlight = Closure::wrap(Box::new(move || {
        highlight_to_html(highlight_code.clone(), highlight_lang.clone())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let highlight_dependencies = Array::new();
    highlight_dependencies.push(&JsValue::from_str(&trimmed));
    highlight_dependencies.push(
        &lang
            .as_deref()
            .map_or(JsValue::UNDEFINED, JsValue::from_str),
    );
    highlight_dependencies.push(&loaded);
    let html = use_memo(react, &highlight.into_js_value(), &highlight_dependencies)?;
    let root_ref = use_ref(react, &JsValue::NULL)?;
    let (copied_value, set_copied) = use_state(react, &JsValue::FALSE)?;
    let copied = copied_value.as_bool().unwrap_or(false);

    let copy_root = root_ref.clone();
    let copy_trimmed = trimmed.clone();
    let copy_setter = set_copied;
    let on_copy = Closure::wrap(Box::new(move || -> Result<(), JsValue> {
        if copied {
            return Ok(());
        }
        let root = current(&copy_root)?;
        let text = if root.is_null() {
            copy_trimmed.clone()
        } else {
            let pre = call_method(&root, "querySelector", &[JsValue::from_str("pre")])?;
            if pre.is_null() {
                copy_trimmed.clone()
            } else {
                Reflect::get(&pre, &JsValue::from_str("textContent"))?
                    .as_string()
                    .unwrap_or_else(|| copy_trimmed.clone())
            }
        };
        let pending = write_clipboard(text);
        let setter = copy_setter.clone();
        let settled = Closure::wrap(Box::new(move |accepted: JsValue| -> Result<(), JsValue> {
            if accepted.as_bool() != Some(true) {
                return Ok(());
            }
            set_state(&setter, &JsValue::TRUE)?;
            let reset = setter.clone();
            let callback = Closure::wrap(Box::new(move || {
                let _ = set_state(&reset, &JsValue::FALSE);
            }) as Box<dyn FnMut()>);
            required_function(&js_sys::global(), "setTimeout", "global")?.call2(
                &js_sys::global(),
                &callback.into_js_value(),
                &JsValue::from_f64(1_000.0),
            )?;
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        call_method(
            Promise::resolve(&pending).as_ref(),
            "then",
            &[settled.into_js_value()],
        )?;
        Ok(())
    }) as Box<dyn FnMut() -> Result<(), JsValue>>);
    let copy_dependencies = Array::new();
    copy_dependencies.push(&JsValue::from_bool(copied));
    copy_dependencies.push(&JsValue::from_str(&trimmed));
    let on_copy = use_callback(react, &on_copy.into_js_value(), &copy_dependencies)?;

    let body = if html.is_undefined() {
        let code = create_element(
            react,
            &JsValue::from_str("code"),
            None,
            &[JsValue::from_str(&trimmed)],
        )?;
        create_element(
            react,
            &JsValue::from_str("pre"),
            Some(&class_props("seekdeep-primitive-code-block-plain")?),
            &[code],
        )?
    } else {
        let inner = object(&[("__html", html)])?;
        create_element(
            react,
            &JsValue::from_str("div"),
            Some(&object(&[("dangerouslySetInnerHTML", inner.into())])?),
            &[],
        )?
    };
    let banner = create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-primitive-code-block-bannerWrap")?),
        &[create_element(
            react,
            &JsValue::from_str("div"),
            Some(&class_props("seekdeep-primitive-code-block-banner")?),
            &[
                create_element(
                    react,
                    &JsValue::from_str("div"),
                    Some(&class_props("seekdeep-primitive-code-block-infostring")?),
                    &[JsValue::from_str(lang.as_deref().unwrap_or_default())],
                )?,
                create_element(
                    react,
                    &JsValue::from_str("div"),
                    Some(&class_props("seekdeep-primitive-code-block-action")?),
                    &[create_element(
                        react,
                        &JsValue::from_str("button"),
                        Some(&object(&[
                            ("type", JsValue::from_str("button")),
                            (
                                "className",
                                JsValue::from_str("seekdeep-primitive-code-block-copyButton"),
                            ),
                            ("onClick", on_copy.into()),
                        ])?),
                        &[JsValue::from_str(if copied {
                            &copied_label
                        } else {
                            &copy_label
                        })],
                    )?],
                )?,
            ],
        )?],
    )?;
    let mut classes = vec![
        "seekdeep-primitive-code-block-block".to_owned(),
        "md-code-block".to_owned(),
    ];
    if let Some(class_name) = class_name.filter(|class_name| !class_name.is_empty()) {
        classes.push(class_name);
    }
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("ref", root_ref),
            ("className", JsValue::from_str(&classes.join(" "))),
        ])?),
        &[banner, body],
    )
}

fn inject_style() -> Result<(), JsValue> {
    let replacements = [
        ("bannerWrap", "seekdeep-primitive-code-block-bannerWrap"),
        ("copyButton", "seekdeep-primitive-code-block-copyButton"),
        ("infostring", "seekdeep-primitive-code-block-infostring"),
        ("banner", "seekdeep-primitive-code-block-banner"),
        ("action", "seekdeep-primitive-code-block-action"),
        ("plain", "seekdeep-primitive-code-block-plain"),
        ("block", "seekdeep-primitive-code-block-block"),
    ];
    inject_namespaced_style("CodeBlock", CODE_BLOCK_CSS, &replacements)
}

pub(crate) fn inject_namespaced_style(
    name: &str,
    source: &str,
    replacements: &[(&str, &str)],
) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = format!("@seekdeep-ai/seekdeep-client-ui-primitives/{name}.module.css");
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
    let mut css = source.to_owned();
    for (source, target) in replacements {
        css = css.replace(&format!(".{source}"), &format!(".{target}"));
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[
            JsValue::from_str("data-plugin-css"),
            JsValue::from_str(&tag),
        ],
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
            js_sys::Error::new("client-ui-primitives CodeBlock module was not configured").into()
        })
    })
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn use_state(react: &JsValue, initial: &JsValue) -> Result<(JsValue, Function), JsValue> {
    let pair = Array::from(&required_function(react, "useState", "React")?.call1(react, initial)?);
    Ok((pair.get(0), pair.get(1).dyn_into()?))
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn use_memo(react: &JsValue, factory: &JsValue, dependencies: &Array) -> Result<JsValue, JsValue> {
    required_function(react, "useMemo", "React")?.call2(react, factory, dependencies)
}

fn use_callback(
    react: &JsValue,
    callback: &JsValue,
    dependencies: &Array,
) -> Result<Function, JsValue> {
    required_function(react, "useCallback", "React")?
        .call2(react, callback, dependencies)?
        .dyn_into()
}

fn set_state(setter: &Function, value: &JsValue) -> Result<(), JsValue> {
    setter.call1(&JsValue::UNDEFINED, value).map(|_| ())
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
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
