//! Compiled structured web-search and web-fetch cards.

use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

const WEB_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/WebBlock.module.css");

thread_local! {
    static MODULES: RefCell<Option<BrowserModules>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct BrowserModules {
    react: JsValue,
    markdown: JsValue,
}

/// Configures React, the compiled Markdown component, and web-card styles.
///
/// # Errors
///
/// Returns DOM stylesheet-injection failures.
#[wasm_bindgen(js_name = configureClientUiPrimitiveWeb)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_web(react: JsValue, markdown: JsValue) -> Result<(), JsValue> {
    MODULES.with(|slot| *slot.borrow_mut() = Some(BrowserModules { react, markdown }));
    inject_style()
}

/// Returns the compiled `WebBlock` component.
///
/// # Errors
///
/// Returns missing module configuration.
#[wasm_bindgen(js_name = webBlockComponent)]
pub fn web_block_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    Ok(
        Closure::wrap(Box::new(move |props: JsValue| render_web(&modules, &props))
            as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>)
        .into_js_value(),
    )
}

fn render_web(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let kind = required_string(props, "kind", "WebBlock props")?;
    if kind == "search" {
        render_search(modules, props)
    } else if kind == "fetch" {
        render_fetch(modules, props)
    } else {
        Err(js_sys::TypeError::new("WebBlock kind must be search or fetch").into())
    }
}

fn render_search(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let answer = optional_string(props, "answer")?;
    let sources = Array::from(&required_property(props, "sources", "Web search props")?);
    let truncated = property_truthy(props, "truncated")?;
    let class_name = optional_string(props, "className")?;
    let mut children = Vec::new();
    if let Some(answer) = answer.as_ref().filter(|answer| !answer.is_empty()) {
        let markdown = create_node(
            &modules.react,
            &modules.markdown,
            Some(&object(&[("text", JsValue::from_str(answer))])?),
            &[],
        )?;
        children.push(create_element(
            &modules.react,
            "div",
            Some(&class_props(&class_name_for("answer"))?),
            &[markdown],
        )?);
    }
    let empty = answer.as_deref().is_none_or(str::is_empty) && sources.length() == 0;
    if empty {
        children.push(create_element(
            &modules.react,
            "div",
            Some(&class_props(&class_name_for("empty"))?),
            &[JsValue::from_str("未找到结果")],
        )?);
    } else {
        let mut source_nodes = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            source_nodes.push(render_source(&modules.react, &source, index + 1)?);
        }
        children.push(create_element(
            &modules.react,
            "ol",
            Some(&class_props(&class_name_for("sources"))?),
            &source_nodes,
        )?);
    }
    if truncated {
        children.push(create_element(
            &modules.react,
            "div",
            Some(&class_props(&class_name_for("truncated"))?),
            &[JsValue::from_str("来源列表已截断")],
        )?);
    }
    create_element(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [Some(class_name_for("block")), class_name]
                        .into_iter()
                        .flatten(),
                )),
            ),
            ("data-web", JsValue::from_str("search")),
        ])?),
        &children,
    )
}

fn render_source(react: &JsValue, source: &JsValue, index: usize) -> Result<JsValue, JsValue> {
    let url = required_string(source, "url", "WebSource")?;
    let title = optional_string(source, "title")?;
    let snippet = optional_string(source, "snippet")?;
    let published = optional_string(source, "publishedAt")?;
    let link = safe_link(
        react,
        &url,
        &link_label(&url, title.as_deref()),
        "sourceLink",
    )?;
    let mut children = vec![link];
    if let Some(snippet) = snippet.filter(|value| !value.is_empty()) {
        children.push(create_element(
            react,
            "div",
            Some(&class_props(&class_name_for("snippet"))?),
            &[JsValue::from_str(&snippet)],
        )?);
    }
    if let Some(published) = published.filter(|value| !value.is_empty()) {
        children.push(create_element(
            react,
            "div",
            Some(&class_props(&class_name_for("published"))?),
            &[JsValue::from_str(&published)],
        )?);
    }
    create_element(
        react,
        "li",
        Some(&object(&[
            ("key", JsValue::from_f64(index_to_f64(index - 1))),
            ("className", JsValue::from_str(&class_name_for("source"))),
            ("value", JsValue::from_f64(index_to_f64(index))),
        ])?),
        &children,
    )
}

fn render_fetch(modules: &BrowserModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let url = required_string(props, "url", "Web fetch props")?;
    let status = required_integer(props, "statusCode", "Web fetch props")?;
    let truncated = property_truthy(props, "truncated")?;
    let class_name = optional_string(props, "className")?;
    let link = safe_link(&modules.react, &url, &url, "fetchUrl")?;
    let mut meta = vec![create_element(
        &modules.react,
        "span",
        Some(&class_props(&class_name_for("status"))?),
        &[JsValue::from_str(&format!("HTTP {status}"))],
    )?];
    if truncated {
        meta.push(create_element(
            &modules.react,
            "span",
            Some(&class_props(&class_name_for("truncated"))?),
            &[JsValue::from_str("内容已截断")],
        )?);
    }
    let meta = create_element(
        &modules.react,
        "div",
        Some(&class_props(&class_name_for("fetchMeta"))?),
        &meta,
    )?;
    create_element(
        &modules.react,
        "div",
        Some(&object(&[
            (
                "className",
                JsValue::from_str(&join_classes(
                    [
                        Some(class_name_for("block")),
                        Some(class_name_for("fetch")),
                        class_name,
                    ]
                    .into_iter()
                    .flatten(),
                )),
            ),
            ("data-web", JsValue::from_str("fetch")),
        ])?),
        &[link, meta],
    )
}

fn safe_link(react: &JsValue, url: &str, label: &str, class: &str) -> Result<JsValue, JsValue> {
    let class_name = class_name_for(class);
    if safe_href(url)?.is_some() {
        create_element(
            react,
            "a",
            Some(&object(&[
                ("className", JsValue::from_str(&class_name)),
                ("href", JsValue::from_str(url)),
                ("target", JsValue::from_str("_blank")),
                ("rel", JsValue::from_str("noopener noreferrer")),
            ])?),
            &[JsValue::from_str(label)],
        )
    } else {
        create_element(
            react,
            "span",
            Some(&class_props(&class_name)?),
            &[JsValue::from_str(label)],
        )
    }
}

fn safe_href(url: &str) -> Result<Option<JsValue>, JsValue> {
    let parsed = parse_url(url)?;
    let Some(parsed) = parsed else {
        return Ok(None);
    };
    let protocol = Reflect::get(&parsed, &JsValue::from_str("protocol"))?.as_string();
    Ok(matches!(protocol.as_deref(), Some("http:" | "https:")).then_some(parsed))
}

fn link_label(url: &str, title: Option<&str>) -> String {
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        return title.to_owned();
    }
    parse_url(url)
        .ok()
        .flatten()
        .and_then(|parsed| Reflect::get(&parsed, &JsValue::from_str("hostname")).ok())
        .and_then(|hostname| hostname.as_string())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| url.to_owned())
}

fn parse_url(url: &str) -> Result<Option<JsValue>, JsValue> {
    let constructor =
        Reflect::get(&js_sys::global(), &JsValue::from_str("URL"))?.dyn_into::<Function>()?;
    let arguments = Array::of1(&JsValue::from_str(url));
    match Reflect::construct(&constructor, &arguments) {
        Ok(value) => Ok(Some(value)),
        Err(_) => Ok(None),
    }
}

fn configured_modules() -> Result<BrowserModules, JsValue> {
    MODULES.with(|slot| {
        slot.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives web module was not configured").into()
        })
    })
}

fn inject_style() -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = "@seekdeep-ai/seekdeep-client-ui-primitives/WebBlock.module.css";
    let mut css = WEB_CSS.to_owned();
    for local in [
        "sourceLink",
        "fetchMeta",
        "fetchUrl",
        "published",
        "truncated",
        "sources",
        "snippet",
        "answer",
        "source",
        "status",
        "empty",
        "fetch",
        "block",
    ] {
        css = css.replace(&format!(".{local}"), &format!(".{}", class_name_for(local)));
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    call_method(
        &style,
        "setAttribute",
        &[JsValue::from_str("data-plugin-css"), JsValue::from_str(tag)],
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

fn class_name_for(local: &str) -> String {
    format!("seekdeep-primitive-webblock-{local}")
}

fn join_classes(values: impl IntoIterator<Item = String>) -> String {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

fn optional_string(value: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_string()
            .map(Some)
            .ok_or_else(|| js_sys::TypeError::new(&format!("{key} must be a string")).into())
    }
}

fn property_truthy(value: &JsValue, key: &str) -> Result<bool, JsValue> {
    Ok(Reflect::get(value, &JsValue::from_str(key))?.is_truthy())
}

fn required_integer(value: &JsValue, key: &str, owner: &str) -> Result<i64, JsValue> {
    let value = required_property(value, key, owner)?
        .as_f64()
        .filter(|value| value.is_finite() && value.fract() == 0.0)
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be an integer")))?;
    format!("{value:.0}")
        .parse()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} {key} is outside i64")).into())
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let value = Reflect::get(value, &JsValue::from_str(key))?;
    if value.is_null() || value.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(value)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let output = Object::new();
    for (key, value) in entries {
        Reflect::set(&output, &JsValue::from_str(key), value)?;
    }
    Ok(output)
}

fn function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    required_property(value, key, "object")?.dyn_into::<Function>()
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}

fn index_to_f64(index: usize) -> f64 {
    index.to_string().parse().expect("usize renders as f64")
}

fn create_element(
    react: &JsValue,
    tag: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    create_node(react, &JsValue::from_str(tag), props, children)
}

fn create_node(
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
    function(react, "createElement")?.apply(react, &arguments)
}
