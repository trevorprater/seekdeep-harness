//! Compiled Markdown component and append-only streaming renderer lifecycle.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use markdown::mdast::Node;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    IncrementalMarkdownParser, MarkdownPlainTextMode,
    browser_code_block::inject_namespaced_style,
    browser_markdown_render::{
        MarkdownModules, MarkdownRenderContext, ReferenceTargets, collect_reference_targets,
        create_reference_targets, defensive_render_fixtures, render_blocks,
        render_footnote_section, render_positioned_blocks, wrap_block_children,
    },
    code_block_component, extract_markdown_plain_text, parse_gfm_with_math,
};

const MARKDOWN_CSS: &str =
    include_str!("../../../packages/client/ui-primitives/src/markdown/MarkdownText.module.css");

thread_local! {
    static MODULES: RefCell<Option<MarkdownModules>> = const { RefCell::new(None) };
}

/// Configures React and the thin KaTeX/URI dependency capability.
///
/// # Errors
///
/// Returns on missing dependency methods, unavailable `CodeBlock`, or stylesheet failure.
#[wasm_bindgen(js_name = configureClientUiPrimitiveMarkdown)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_primitive_markdown(
    react: JsValue,
    backend: JsValue,
) -> Result<(), JsValue> {
    for method in ["createElement", "memo", "useMemo", "useRef"] {
        required_function(&react, method, "React")?;
    }
    for method in ["normalizeUri", "renderTex"] {
        required_function(&backend, method, "Markdown backend")?;
    }
    let css_url = required_string(&backend, "cssUrl", "Markdown backend")?;
    let fragment = required_property(&react, "Fragment", "React")?;
    let code_block = code_block_component()?;
    inject_katex_stylesheet(&css_url)?;
    inject_style()?;
    MODULES.with(|modules| {
        *modules.borrow_mut() = Some(MarkdownModules {
            react,
            fragment,
            code_block,
            backend,
        });
    });
    Ok(())
}

/// Returns the memoized compiled `MarkdownText` component.
///
/// # Errors
///
/// Returns before configuration.
#[wasm_bindgen(js_name = markdownTextComponent)]
pub fn markdown_text_component() -> Result<JsValue, JsValue> {
    let modules = configured_modules()?;
    let react = modules.react.clone();
    let component =
        Closure::wrap(
            Box::new(move |props: JsValue| render_markdown_text(&modules, &props))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        )
        .into_js_value();
    required_function(&react, "memo", "React")?.call1(&react, &component)
}

/// Returns the compiled defensive renderer fixture catalog used by parity tests.
///
/// # Errors
///
/// Returns before configuration or when React element construction fails.
#[doc(hidden)]
#[wasm_bindgen(js_name = markdownDefensiveFixtures)]
pub fn markdown_defensive_fixtures() -> Result<Object, JsValue> {
    defensive_render_fixtures(&configured_modules()?)
}

/// Projects Markdown into the source-compatible complete or compact plain-text modes.
///
/// # Errors
///
/// Returns malformed options or parser diagnostics.
#[wasm_bindgen(js_name = extractMarkdownPlainText)]
#[allow(clippy::needless_pass_by_value)]
pub fn browser_extract_markdown_plain_text(
    markdown: String,
    options: JsValue,
) -> Result<JsValue, JsValue> {
    let mode = if options.is_undefined() {
        JsValue::UNDEFINED
    } else {
        Reflect::get(&options, &JsValue::from_str("mode"))?
    };
    let (mode, known) = if mode.is_undefined() {
        (MarkdownPlainTextMode::All, true)
    } else {
        match mode.as_string().as_deref() {
            Some("all") => (MarkdownPlainTextMode::All, true),
            Some("first-line") => (MarkdownPlainTextMode::FirstLine, true),
            Some("first-paragraph") => (MarkdownPlainTextMode::FirstParagraph, true),
            _ => (MarkdownPlainTextMode::All, false),
        }
    };
    let projected = extract_markdown_plain_text(&markdown, mode).map_err(|error| {
        JsValue::from(js_sys::Error::new(&format!(
            "Markdown plain-text projection failed: {error}"
        )))
    })?;
    Ok(if known {
        JsValue::from_str(&projected)
    } else {
        JsValue::UNDEFINED
    })
}

fn render_markdown_text(modules: &MarkdownModules, props: &JsValue) -> Result<JsValue, JsValue> {
    let react = &modules.react;
    let text = required_string(props, "text", "MarkdownText props")?;
    let streaming_value = Reflect::get(props, &JsValue::from_str("streaming"))?;
    let streaming = !streaming_value.is_undefined() && streaming_value.is_truthy();
    let code_labels = Reflect::get(props, &JsValue::from_str("codeLabels"))?;
    let file_mentions = Reflect::get(props, &JsValue::from_str("fileMentions"))?;
    let stream_ref = use_ref(react, &JsValue::NULL)?;
    let labels_ref = use_ref(react, &code_labels)?;

    let render_modules = modules.clone();
    let render_text = text.clone();
    let render_labels = code_labels.clone();
    let render_mentions = file_mentions.clone();
    let render_stream_ref = stream_ref.clone();
    let render_labels_ref = labels_ref.clone();
    let factory = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if !streaming {
            set_current(&render_stream_ref, &JsValue::NULL)?;
            return render_settled(
                &render_modules,
                &render_text,
                render_labels.clone(),
                render_mentions.clone(),
            );
        }
        let current_renderer = current(&render_stream_ref)?;
        let prior_labels = current(&render_labels_ref)?;
        let renderer = if current_renderer.is_null()
            || current_renderer.is_undefined()
            || !Object::is(&prior_labels, &render_labels)
        {
            let renderer = streaming_renderer_face(StreamingRenderer::new(
                render_modules.clone(),
                render_labels.clone(),
            ))?;
            set_current(&render_stream_ref, renderer.as_ref())?;
            set_current(&render_labels_ref, &render_labels)?;
            renderer
        } else {
            current_renderer.dyn_into::<Function>()?
        };
        renderer.call1(&JsValue::UNDEFINED, &JsValue::from_str(&render_text))
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    let dependencies = Array::new();
    dependencies.push(&JsValue::from_str(&text));
    dependencies.push(&JsValue::from_bool(streaming));
    dependencies.push(&code_labels);
    dependencies.push(&file_mentions);
    let children = required_function(react, "useMemo", "React")?.call2(
        react,
        &factory.into_js_value(),
        &dependencies,
    )?;
    let children = Array::from(&children).iter().collect::<Vec<_>>();
    create_element(
        react,
        &JsValue::from_str("div"),
        Some(&class_props("seekdeep-primitive-markdown-markdown")?),
        &children,
    )
}

fn render_settled(
    modules: &MarkdownModules,
    text: &str,
    code_labels: JsValue,
    file_mentions: JsValue,
) -> Result<JsValue, JsValue> {
    let root = parse_gfm_with_math(text).map_err(|error| js_sys::Error::new(&error))?;
    let Node::Root(root) = root else {
        return Err(js_sys::Error::new("Markdown parser did not return a root node").into());
    };
    let mut targets = create_reference_targets();
    collect_reference_targets(&root.children, &mut targets);
    let mut context =
        MarkdownRenderContext::new(modules, false, code_labels, file_mentions, targets);
    let blocks = render_blocks(&root.children, &mut context)?;
    let mut children = wrap_block_children(&blocks, false);
    let section = render_footnote_section(&mut context)?;
    if !section.is_null() {
        children.push(JsValue::from_str("\n"));
        children.push(section);
    }
    Ok(children.into_iter().collect::<Array>().into())
}

struct StreamingRenderer {
    modules: MarkdownModules,
    parser: IncrementalMarkdownParser,
    code_labels: JsValue,
    generation: u64,
    frozen_count: usize,
    frozen_elements: Vec<JsValue>,
    frozen_targets: ReferenceTargets,
    frozen_footnote_order: Vec<String>,
    frozen_footnote_counts: std::collections::BTreeMap<String, u32>,
    last_text: Option<String>,
    last_rendered: JsValue,
}

impl StreamingRenderer {
    fn new(modules: MarkdownModules, code_labels: JsValue) -> Self {
        Self {
            modules,
            parser: IncrementalMarkdownParser::default(),
            code_labels,
            generation: u64::MAX,
            frozen_count: 0,
            frozen_elements: Vec::new(),
            frozen_targets: create_reference_targets(),
            frozen_footnote_order: Vec::new(),
            frozen_footnote_counts: std::collections::BTreeMap::new(),
            last_text: None,
            last_rendered: Array::new().into(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, text: &str) -> Result<JsValue, JsValue> {
        if self.last_text.as_deref() == Some(text) {
            return Ok(self.last_rendered.clone());
        }
        let update = self
            .parser
            .update(text)
            .map_err(|error| js_sys::Error::new(&error))?;
        if update.generation != self.generation {
            self.generation = update.generation;
            self.frozen_count = 0;
            self.frozen_elements.clear();
            self.frozen_targets = create_reference_targets();
            self.frozen_footnote_order.clear();
            self.frozen_footnote_counts.clear();
        }
        let newly_frozen = &update.frozen[self.frozen_count..];
        for block in newly_frozen {
            collect_reference_targets(
                std::slice::from_ref(block.node.as_ref()),
                &mut self.frozen_targets,
            );
        }
        let mut frame_targets = self.frozen_targets.clone();
        for block in &update.tail {
            collect_reference_targets(
                std::slice::from_ref(block.node.as_ref()),
                &mut frame_targets,
            );
        }
        if !newly_frozen.is_empty() {
            let mut context = MarkdownRenderContext::new(
                &self.modules,
                true,
                self.code_labels.clone(),
                JsValue::UNDEFINED,
                frame_targets.clone(),
            );
            context.footnote_order = std::mem::take(&mut self.frozen_footnote_order);
            context.footnote_counts = std::mem::take(&mut self.frozen_footnote_counts);
            let rendered = render_positioned_blocks(newly_frozen, &mut context)?;
            self.frozen_footnote_order = context.footnote_order;
            self.frozen_footnote_counts = context.footnote_counts;
            for element in rendered {
                if !self.frozen_elements.is_empty() {
                    self.frozen_elements.push(JsValue::from_str("\n"));
                }
                self.frozen_elements.push(element);
            }
            self.frozen_count = update.frozen.len();
        }
        let mut context = MarkdownRenderContext::new(
            &self.modules,
            true,
            self.code_labels.clone(),
            JsValue::UNDEFINED,
            frame_targets,
        );
        context
            .footnote_order
            .clone_from(&self.frozen_footnote_order);
        context
            .footnote_counts
            .clone_from(&self.frozen_footnote_counts);
        let mut children = self.frozen_elements.clone();
        for element in render_positioned_blocks(&update.tail, &mut context)? {
            if !children.is_empty() {
                children.push(JsValue::from_str("\n"));
            }
            children.push(element);
        }
        let section = render_footnote_section(&mut context)?;
        if !section.is_null() {
            children.push(JsValue::from_str("\n"));
            children.push(section);
        }
        self.last_text = Some(text.to_owned());
        self.last_rendered = children.into_iter().collect::<Array>().into();
        Ok(self.last_rendered.clone())
    }
}

fn streaming_renderer_face(renderer: StreamingRenderer) -> Result<Function, JsValue> {
    let renderer = Rc::new(RefCell::new(renderer));
    Closure::wrap(
        Box::new(move |text: String| renderer.borrow_mut().render(&text))
            as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>,
    )
    .into_js_value()
    .dyn_into()
}

fn inject_style() -> Result<(), JsValue> {
    inject_namespaced_style(
        "MarkdownText",
        MARKDOWN_CSS,
        &[
            ("fileMention", "seekdeep-primitive-markdown-fileMention"),
            ("tableScroll", "seekdeep-primitive-markdown-tableScroll"),
            ("imageAlt", "seekdeep-primitive-markdown-imageAlt"),
            ("markdown", "seekdeep-primitive-markdown-markdown"),
            ("image", "seekdeep-primitive-markdown-image"),
        ],
    )
}

fn inject_katex_stylesheet(css_url: &str) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = "@seekdeep-ai/seekdeep-client-ui-primitives/katex/katex.min.css";
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let link = call_method(&document, "createElement", &[JsValue::from_str("link")])?;
    for (name, value) in [
        ("rel", "stylesheet"),
        ("href", css_url),
        ("data-plugin-css", tag),
        ("data-plugin", "@seekdeep-ai/seekdeep-client-ui-primitives"),
    ] {
        call_method(
            &link,
            "setAttribute",
            &[JsValue::from_str(name), JsValue::from_str(value)],
        )?;
    }
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[link])?;
    Ok(())
}

fn configured_modules() -> Result<MarkdownModules, JsValue> {
    MODULES.with(|modules| {
        modules.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-primitives Markdown was not configured").into()
        })
    })
}

fn current(reference: &JsValue) -> Result<JsValue, JsValue> {
    Reflect::get(reference, &JsValue::from_str("current"))
}

fn set_current(reference: &JsValue, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(reference, &JsValue::from_str("current"), value).map(|_| ())
}

fn use_ref(react: &JsValue, initial: &JsValue) -> Result<JsValue, JsValue> {
    required_function(react, "useRef", "React")?.call1(react, initial)
}

fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
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
