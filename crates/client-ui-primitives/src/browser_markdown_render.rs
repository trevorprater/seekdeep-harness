//! Rust-owned mdast-to-React rendering and `KaTeX` tree projection.

use std::collections::BTreeMap;

use js_sys::{Array, Function, JsString, Object, Reflect};
use markdown::mdast::{AlignKind, Definition, FootnoteDefinition, Node, ReferenceKind};
use wasm_bindgen::{JsCast as _, JsValue};

use crate::PositionedMarkdownBlock;

#[derive(Clone)]
pub(crate) struct MarkdownModules {
    pub(crate) react: JsValue,
    pub(crate) fragment: JsValue,
    pub(crate) code_block: JsValue,
    pub(crate) backend: JsValue,
}

#[derive(Clone, Default)]
pub(crate) struct ReferenceTargets {
    definitions: BTreeMap<String, Definition>,
    footnotes: BTreeMap<String, FootnoteDefinition>,
}

pub(crate) struct MarkdownRenderContext<'a> {
    pub(crate) modules: &'a MarkdownModules,
    pub(crate) streaming: bool,
    pub(crate) code_labels: JsValue,
    pub(crate) file_mentions: JsValue,
    in_link: bool,
    pub(crate) targets: ReferenceTargets,
    pub(crate) footnote_order: Vec<String>,
    pub(crate) footnote_counts: BTreeMap<String, u32>,
}

impl<'a> MarkdownRenderContext<'a> {
    pub(crate) fn new(
        modules: &'a MarkdownModules,
        streaming: bool,
        code_labels: JsValue,
        file_mentions: JsValue,
        targets: ReferenceTargets,
    ) -> Self {
        Self {
            modules,
            streaming,
            code_labels,
            file_mentions,
            in_link: false,
            targets,
            footnote_order: Vec::new(),
            footnote_counts: BTreeMap::new(),
        }
    }
}

pub(crate) fn create_reference_targets() -> ReferenceTargets {
    ReferenceTargets::default()
}

pub(crate) fn collect_reference_targets(nodes: &[Node], targets: &mut ReferenceTargets) {
    for node in nodes {
        match node {
            Node::Definition(definition) => {
                targets
                    .definitions
                    .entry(identifier_key(&definition.identifier))
                    .or_insert_with(|| definition.clone());
            }
            Node::FootnoteDefinition(definition) => {
                targets
                    .footnotes
                    .entry(identifier_key(&definition.identifier))
                    .or_insert_with(|| definition.clone());
            }
            _ => {}
        }
        if let Some(children) = node.children() {
            collect_reference_targets(children, targets);
        }
    }
}

pub(crate) fn render_blocks(
    blocks: &[Node],
    context: &mut MarkdownRenderContext<'_>,
) -> Result<Vec<JsValue>, JsValue> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            let key = index_key(index);
            match render_node(node, key, context) {
                Ok(value) if value.is_null() => None,
                result => Some(result),
            }
        })
        .collect()
}

pub(crate) fn render_positioned_blocks(
    blocks: &[std::rc::Rc<PositionedMarkdownBlock>],
    context: &mut MarkdownRenderContext<'_>,
) -> Result<Vec<JsValue>, JsValue> {
    blocks
        .iter()
        .filter_map(|block| {
            match render_node(
                &block.node,
                JsValue::from_str(&block.key.to_string()),
                context,
            ) {
                Ok(value) if value.is_null() => None,
                result => Some(result),
            }
        })
        .collect()
}

pub(crate) fn wrap_block_children(elements: &[JsValue], edges: bool) -> Vec<JsValue> {
    let mut wrapped = Vec::new();
    for element in elements {
        if edges || !wrapped.is_empty() {
            wrapped.push(JsValue::from_str("\n"));
        }
        wrapped.push(element.clone());
    }
    if edges && !elements.is_empty() {
        wrapped.push(JsValue::from_str("\n"));
    }
    wrapped
}

enum BlockEntry {
    Paragraph(Vec<JsValue>),
    Element(JsValue),
}

fn render_block_entries(
    blocks: &[Node],
    context: &mut MarkdownRenderContext<'_>,
) -> Result<Vec<BlockEntry>, JsValue> {
    let mut entries = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        if let Node::Paragraph(paragraph) = block {
            entries.push(BlockEntry::Paragraph(render_children(
                &paragraph.children,
                context,
            )?));
        } else {
            let element = render_node(block, index_key(index), context)?;
            if !element.is_null() {
                entries.push(BlockEntry::Element(element));
            }
        }
    }
    Ok(entries)
}

fn render_children(
    nodes: &[Node],
    context: &mut MarkdownRenderContext<'_>,
) -> Result<Vec<JsValue>, JsValue> {
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| render_node(node, index_key(index), context))
        .collect()
}

#[allow(clippy::too_many_lines)]
fn render_node(
    node: &Node,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let react = &context.modules.react;
    match node {
        Node::Text(node) => Ok(JsValue::from_str(&node.value)),
        Node::Paragraph(node) => create_keyed_element(
            react,
            &JsValue::from_str("p"),
            key,
            &render_children(&node.children, context)?,
        ),
        Node::Heading(node) => create_keyed_element(
            react,
            &JsValue::from_str(&format!("h{}", node.depth)),
            key,
            &render_children(&node.children, context)?,
        ),
        Node::Blockquote(node) => {
            let children = render_children(&node.children, context)?;
            let children = children
                .into_iter()
                .filter(|child| !child.is_null())
                .collect::<Vec<_>>();
            create_keyed_element(
                react,
                &JsValue::from_str("blockquote"),
                key,
                &wrap_block_children(&children, true),
            )
        }
        Node::ThematicBreak(_) => create_keyed_element(react, &JsValue::from_str("hr"), key, &[]),
        Node::Break(_) => {
            let line_break = create_element(react, &JsValue::from_str("br"), None, &[])?;
            create_keyed_element(
                react,
                &context.modules.fragment,
                key,
                &[line_break, JsValue::from_str("\n")],
            )
        }
        Node::Strong(node) => create_keyed_element(
            react,
            &JsValue::from_str("strong"),
            key,
            &render_children(&node.children, context)?,
        ),
        Node::Emphasis(node) => create_keyed_element(
            react,
            &JsValue::from_str("em"),
            key,
            &render_children(&node.children, context)?,
        ),
        Node::Delete(node) => create_keyed_element(
            react,
            &JsValue::from_str("del"),
            key,
            &render_children(&node.children, context)?,
        ),
        Node::InlineCode(node) => render_inline_code(&node.value, key, context),
        Node::Html(node) => Ok(JsValue::from_str(&node.value)),
        Node::Code(node) => render_code(node, key, context),
        Node::Math(node) => render_math(&node.value, true, key, context),
        Node::InlineMath(node) => render_math(&node.value, false, key, context),
        Node::List(node) => render_list(node, key, context),
        Node::ListItem(node) => {
            render_list_item(node, list_item_loose(node, Some(node.spread)), key, context)
        }
        Node::Table(node) => render_table(node, key, context),
        Node::Link(node) => {
            let prior = context.in_link;
            context.in_link = true;
            let children = render_children(&node.children, context);
            context.in_link = prior;
            render_anchor(&node.url, &children?, key, context)
        }
        Node::LinkReference(node) => render_link_reference(node, key, context),
        Node::Image(node) => render_image(&node.url, &node.alt, key, context),
        Node::ImageReference(node) => render_image_reference(node, key, context),
        Node::FootnoteReference(node) => render_footnote_reference(&node.identifier, key, context),
        _ => Ok(JsValue::NULL),
    }
}

fn render_inline_code(
    source: &str,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let value = source.replace("\r\n", " ").replace(['\r', '\n'], " ");
    if let Some(href) = inline_code_http_url(&value) {
        let link = render_safe_link(
            &href,
            &[JsValue::from_str(&value)],
            JsValue::from_str("link"),
            context,
        )?;
        return create_keyed_element(
            &context.modules.react,
            &JsValue::from_str("code"),
            key,
            &[link],
        );
    }
    if !context.in_link && !context.file_mentions.is_null() && !context.file_mentions.is_undefined()
    {
        let mention = call_method(
            &context.file_mentions,
            "resolve",
            &[JsValue::from_str(&value)],
        )?;
        if !mention.is_null() && !mention.is_undefined() {
            let open = required_function(&mention, "open", "file mention")?;
            let label = required_string(&mention, "label", "file mention")?;
            let title = required_string(&mention, "title", "file mention")?;
            let button = create_element(
                &context.modules.react,
                &JsValue::from_str("button"),
                Some(&object(&[
                    ("type", JsValue::from_str("button")),
                    (
                        "className",
                        JsValue::from_str("seekdeep-primitive-markdown-fileMention"),
                    ),
                    ("title", JsValue::from_str(&title)),
                    ("aria-label", JsValue::from_str(&label)),
                    ("onClick", open.into()),
                ])?),
                &[JsValue::from_str(&value)],
            )?;
            return create_keyed_element(
                &context.modules.react,
                &JsValue::from_str("code"),
                key,
                &[button],
            );
        }
    }
    create_keyed_element(
        &context.modules.react,
        &JsValue::from_str("code"),
        key,
        &[JsValue::from_str(&value)],
    )
}

fn render_code(
    node: &markdown::mdast::Code,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let language = node.lang.as_deref();
    if node.value.is_empty() {
        let mut code_props = vec![("key", JsValue::from_str("code"))];
        if let Some(language) = language {
            code_props.push((
                "className",
                JsValue::from_str(&format!("language-{language}")),
            ));
        }
        let code = create_element(
            &context.modules.react,
            &JsValue::from_str("code"),
            Some(&object(&code_props)?),
            &[],
        )?;
        return create_keyed_element(
            &context.modules.react,
            &JsValue::from_str("pre"),
            key,
            &[code],
        );
    }
    let lang = language.and_then(code_language_prefix);
    if !context.streaming && lang == Some("math") {
        return render_math(&format!("{}\n", node.value), true, key, context);
    }
    let props = Object::new();
    Reflect::set(&props, &JsValue::from_str("key"), &key)?;
    Reflect::set(
        &props,
        &JsValue::from_str("code"),
        &JsValue::from_str(&format!("{}\n", node.value)),
    )?;
    if !context.streaming
        && let Some(lang) = lang
    {
        Reflect::set(&props, &JsValue::from_str("lang"), &JsValue::from_str(lang))?;
    }
    forward_optional_label(&props, &context.code_labels, "copyLabel")?;
    forward_optional_label(&props, &context.code_labels, "copiedLabel")?;
    create_element(
        &context.modules.react,
        &context.modules.code_block,
        Some(&props),
        &[],
    )
}

fn code_language_prefix(language: &str) -> Option<&str> {
    let end = language
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
        })
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    Some(&language[..end])
}

fn forward_optional_label(props: &Object, labels: &JsValue, key: &str) -> Result<(), JsValue> {
    if labels.is_null() || labels.is_undefined() {
        return Ok(());
    }
    let value = Reflect::get(labels, &JsValue::from_str(key))?;
    if !value.is_undefined() {
        Reflect::set(props, &JsValue::from_str(key), &value)?;
    }
    Ok(())
}

fn list_loose(list: &markdown::mdast::List) -> bool {
    list.spread
        || list
            .children
            .iter()
            .filter_map(|node| match node {
                Node::ListItem(item) => Some(list_item_loose(item, Some(item.spread))),
                _ => None,
            })
            .any(|loose| loose)
}

fn list_item_loose(item: &markdown::mdast::ListItem, spread: Option<bool>) -> bool {
    spread.unwrap_or(item.children.len() > 1)
}

fn render_list(
    node: &markdown::mdast::List,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let loose = list_loose(node);
    let mut entries = vec![("key", key)];
    if let Some(start) = node.start
        && start != 1
    {
        entries.push(("start", JsValue::from_f64(f64::from(start))));
    }
    if node
        .children
        .iter()
        .any(|node| matches!(node, Node::ListItem(item) if item.checked.is_some()))
    {
        entries.push(("className", JsValue::from_str("contains-task-list")));
    }
    let children = node
        .children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| match child {
            Node::ListItem(item) => Some(render_list_item(item, loose, index_key(index), context)),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    create_element(
        &context.modules.react,
        &JsValue::from_str(if node.ordered { "ol" } else { "ul" }),
        Some(&object(&entries)?),
        &children,
    )
}

fn render_list_item(
    item: &markdown::mdast::ListItem,
    loose: bool,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let mut entries = render_block_entries(&item.children, context)?;
    let task = item.checked.is_some();
    if let Some(checked) = item.checked {
        let checkbox = create_element(
            &context.modules.react,
            &JsValue::from_str("input"),
            Some(&object(&[
                ("key", JsValue::from_str("task-checkbox")),
                ("type", JsValue::from_str("checkbox")),
                ("checked", JsValue::from_bool(checked)),
                ("disabled", JsValue::TRUE),
            ])?),
            &[],
        )?;
        if let Some(BlockEntry::Paragraph(children)) = entries.first_mut() {
            if children.is_empty() {
                children.push(checkbox);
            } else {
                children.insert(0, JsValue::from_str(" "));
                children.insert(0, checkbox);
            }
        } else {
            entries.insert(0, BlockEntry::Paragraph(vec![checkbox]));
        }
    }
    let mut parts = Vec::new();
    let entry_count = entries.len();
    for (index, entry) in entries.into_iter().enumerate() {
        let paragraph = matches!(entry, BlockEntry::Paragraph(_));
        if loose || index != 0 || !paragraph {
            parts.push(JsValue::from_str("\n"));
        }
        match entry {
            BlockEntry::Element(element) => parts.push(element),
            BlockEntry::Paragraph(children) if loose => parts.push(create_keyed_element(
                &context.modules.react,
                &JsValue::from_str("p"),
                JsValue::from_str(&format!("p-{index}")),
                &children,
            )?),
            BlockEntry::Paragraph(children) => parts.push(create_keyed_element(
                &context.modules.react,
                &context.modules.fragment,
                JsValue::from_str(&format!("p-{index}")),
                &children,
            )?),
        }
        if index + 1 == entry_count && (loose || !paragraph) {
            parts.push(JsValue::from_str("\n"));
        }
    }
    let mut props = vec![("key", key)];
    if task {
        props.push(("className", JsValue::from_str("task-list-item")));
    }
    create_element(
        &context.modules.react,
        &JsValue::from_str("li"),
        Some(&object(&props)?),
        &parts,
    )
}

fn render_table(
    node: &markdown::mdast::Table,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    render_table_with_alignment(node, Some(&node.align), key, context)
}

fn render_table_with_alignment(
    node: &markdown::mdast::Table,
    align: Option<&[AlignKind]>,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let rows = node
        .children
        .iter()
        .filter_map(|node| match node {
            Node::TableRow(row) => Some(row),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut table_children = Vec::new();
    if let Some(head) = rows.first() {
        let row = render_table_row(head, "th", align, 0, context)?;
        table_children.push(create_element(
            &context.modules.react,
            &JsValue::from_str("thead"),
            None,
            &[row],
        )?);
    }
    if rows.len() > 1 {
        let body = rows[1..]
            .iter()
            .enumerate()
            .map(|(index, row)| render_table_row(row, "td", align, index + 1, context))
            .collect::<Result<Vec<_>, _>>()?;
        table_children.push(create_element(
            &context.modules.react,
            &JsValue::from_str("tbody"),
            None,
            &body,
        )?);
    }
    let table = create_element(
        &context.modules.react,
        &JsValue::from_str("table"),
        None,
        &table_children,
    )?;
    create_element(
        &context.modules.react,
        &JsValue::from_str("div"),
        Some(&object(&[
            ("key", key),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-markdown-tableScroll"),
            ),
        ])?),
        &[table],
    )
}

fn render_table_row(
    row: &markdown::mdast::TableRow,
    cell_tag: &str,
    align: Option<&[AlignKind]>,
    key: usize,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let cells = row
        .children
        .iter()
        .filter_map(|node| match node {
            Node::TableCell(cell) => Some(cell),
            _ => None,
        })
        .collect::<Vec<_>>();
    let length = align.map_or(cells.len(), <[AlignKind]>::len);
    let mut rendered = Vec::new();
    for index in 0..length {
        let mut props = vec![("key", index_key(index))];
        if let Some(alignment) = align.and_then(|values| values.get(index)).copied()
            && alignment != AlignKind::None
        {
            let value = match alignment {
                AlignKind::Left => "left",
                AlignKind::Right => "right",
                AlignKind::Center => "center",
                AlignKind::None => unreachable!(),
            };
            props.push((
                "style",
                object(&[("textAlign", JsValue::from_str(value))])?.into(),
            ));
        }
        let children = if let Some(cell) = cells.get(index) {
            render_children(&cell.children, context)?
        } else {
            Vec::new()
        };
        rendered.push(create_element(
            &context.modules.react,
            &JsValue::from_str(cell_tag),
            Some(&object(&props)?),
            &children,
        )?);
    }
    create_keyed_element(
        &context.modules.react,
        &JsValue::from_str("tr"),
        index_key(key),
        &rendered,
    )
}

fn render_safe_link(
    href: &str,
    children: &[JsValue],
    key: JsValue,
    context: &MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let Some(safe_href) = sanitize_url(href) else {
        return create_keyed_element(
            &context.modules.react,
            &context.modules.fragment,
            key,
            children,
        );
    };
    let protocol = url_protocol(&safe_href)?;
    let mut props = vec![("key", key), ("href", JsValue::from_str(&safe_href))];
    if matches!(protocol.as_str(), "http:" | "https:") {
        props.push(("target", JsValue::from_str("_blank")));
        props.push(("rel", JsValue::from_str("noopener noreferrer")));
    }
    create_element(
        &context.modules.react,
        &JsValue::from_str("a"),
        Some(&object(&props)?),
        children,
    )
}

fn render_anchor(
    url: &str,
    children: &[JsValue],
    key: JsValue,
    context: &MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let normalized = normalize_uri(url, context.modules)?;
    render_safe_link(&normalized, children, key, context)
}

fn inline_code_http_url(value: &str) -> Option<String> {
    let js_value = JsString::from(value);
    if js_value.trim() != value {
        return None;
    }
    let Ok(protocol) = url_protocol(value) else {
        return None;
    };
    matches!(protocol.as_str(), "http:" | "https:").then(|| value.to_owned())
}

fn render_image(
    url: &str,
    alt: &str,
    key: JsValue,
    context: &MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let normalized = normalize_uri(url, context.modules)?;
    let source = sanitize_url(&normalized);
    let remote = if let Some(source) = source {
        let protocol = url_protocol(&source)?;
        matches!(protocol.as_str(), "http:" | "https:").then_some(source)
    } else {
        None
    };
    let Some(source) = remote else {
        return create_element(
            &context.modules.react,
            &JsValue::from_str("span"),
            Some(&object(&[
                ("key", key),
                (
                    "className",
                    JsValue::from_str("seekdeep-primitive-markdown-imageAlt"),
                ),
            ])?),
            &[JsValue::from_str(alt)],
        );
    };
    create_element(
        &context.modules.react,
        &JsValue::from_str("img"),
        Some(&object(&[
            ("key", key),
            (
                "className",
                JsValue::from_str("seekdeep-primitive-markdown-image"),
            ),
            ("src", JsValue::from_str(&source)),
            ("alt", JsValue::from_str(alt)),
            ("loading", JsValue::from_str("lazy")),
            ("decoding", JsValue::from_str("async")),
            ("referrerPolicy", JsValue::from_str("no-referrer")),
        ])?),
        &[],
    )
}

fn reference_suffix(kind: ReferenceKind, label: Option<&str>, identifier: &str) -> String {
    match kind {
        ReferenceKind::Collapsed => "][]".to_owned(),
        ReferenceKind::Full => format!("][{}]", label.unwrap_or(identifier)),
        ReferenceKind::Shortcut => "]".to_owned(),
    }
}

fn render_link_reference(
    node: &markdown::mdast::LinkReference,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let definition = context
        .targets
        .definitions
        .get(&identifier_key(&node.identifier))
        .cloned();
    let Some(definition) = definition else {
        let mut children = vec![JsValue::from_str("[")];
        children.extend(render_children(&node.children, context)?);
        children.push(JsValue::from_str(&reference_suffix(
            node.reference_kind,
            node.label.as_deref(),
            &node.identifier,
        )));
        return create_keyed_element(
            &context.modules.react,
            &context.modules.fragment,
            key,
            &children,
        );
    };
    let prior = context.in_link;
    context.in_link = true;
    let children = render_children(&node.children, context);
    context.in_link = prior;
    render_anchor(&definition.url, &children?, key, context)
}

fn render_image_reference(
    node: &markdown::mdast::ImageReference,
    key: JsValue,
    context: &MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let definition = context
        .targets
        .definitions
        .get(&identifier_key(&node.identifier));
    if let Some(definition) = definition {
        return render_image(&definition.url, &node.alt, key, context);
    }
    Ok(JsValue::from_str(&format!(
        "![{}{}",
        node.alt,
        reference_suffix(node.reference_kind, node.label.as_deref(), &node.identifier)
    )))
}

fn render_footnote_reference(
    identifier: &str,
    key: JsValue,
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let identifier = identifier_key(identifier);
    let seen = context.footnote_counts.get(&identifier).copied();
    if seen.is_none() {
        context.footnote_order.push(identifier.clone());
    }
    context
        .footnote_counts
        .insert(identifier.clone(), seen.unwrap_or(0).saturating_add(1));
    let number = context
        .footnote_order
        .iter()
        .position(|value| value == &identifier)
        .map_or(0, |index| index + 1);
    create_keyed_element(
        &context.modules.react,
        &JsValue::from_str("sup"),
        key,
        &[JsValue::from_str(&number.to_string())],
    )
}

pub(crate) fn render_footnote_section(
    context: &mut MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let mut items = Vec::new();
    for identifier in context.footnote_order.clone() {
        let definition = context.targets.footnotes.get(&identifier).cloned();
        let Some(definition) = definition else {
            continue;
        };
        let count = context
            .footnote_counts
            .get(&identifier)
            .copied()
            .unwrap_or(0);
        let mut backrefs = Vec::new();
        for reference in 1..=count {
            if !backrefs.is_empty() {
                backrefs.push(JsValue::from_str(" "));
            }
            backrefs.push(JsValue::from_str("↩"));
            if reference > 1 {
                backrefs.push(create_keyed_element(
                    &context.modules.react,
                    &JsValue::from_str("sup"),
                    JsValue::from_str(&format!("re-{reference}")),
                    &[JsValue::from_str(&reference.to_string())],
                )?);
            }
        }
        let entries = render_block_entries(&definition.children, context)?;
        let tail_is_paragraph = matches!(entries.last(), Some(BlockEntry::Paragraph(_)));
        let entry_count = entries.len();
        let mut body = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            match entry {
                BlockEntry::Paragraph(mut children) => {
                    if index + 1 == entry_count {
                        children.push(JsValue::from_str(" "));
                        children.extend(backrefs.clone());
                    }
                    body.push(create_keyed_element(
                        &context.modules.react,
                        &JsValue::from_str("p"),
                        JsValue::from_str(&format!("p-{index}")),
                        &children,
                    )?);
                }
                BlockEntry::Element(element) => body.push(element),
            }
        }
        if !tail_is_paragraph {
            body.extend(backrefs);
        }
        let id = normalize_uri(&identifier.to_lowercase(), context.modules)?;
        items.push(create_element(
            &context.modules.react,
            &JsValue::from_str("li"),
            Some(&object(&[
                ("key", JsValue::from_str(&identifier)),
                ("id", JsValue::from_str(&format!("user-content-fn-{id}"))),
            ])?),
            &wrap_block_children(&body, true),
        )?);
    }
    if items.is_empty() {
        return Ok(JsValue::NULL);
    }
    let heading = create_element(
        &context.modules.react,
        &JsValue::from_str("h2"),
        Some(&object(&[
            ("id", JsValue::from_str("footnote-label")),
            ("className", JsValue::from_str("sr-only")),
        ])?),
        &[JsValue::from_str("Footnotes")],
    )?;
    let list = create_element(
        &context.modules.react,
        &JsValue::from_str("ol"),
        None,
        &items,
    )?;
    create_element(
        &context.modules.react,
        &JsValue::from_str("section"),
        Some(&object(&[
            ("key", JsValue::from_str("footnotes")),
            ("data-footnotes", JsValue::TRUE),
            ("className", JsValue::from_str("footnotes")),
        ])?),
        &[heading, list],
    )
}

#[allow(clippy::too_many_lines)] // Closed source-suite fixture catalog stays auditable in one table.
pub(crate) fn defensive_render_fixtures(modules: &MarkdownModules) -> Result<Object, JsValue> {
    use markdown::mdast::{
        Code, FootnoteDefinition as MdFootnoteDefinition, Image, ImageReference, LinkReference,
        List, ListItem, Paragraph, Table, TableCell, TableRow, Text, Yaml,
    };

    let output = Object::new();
    let text = |value: &str| {
        Node::Text(Text {
            value: value.to_owned(),
            position: None,
        })
    };
    let paragraph = |children| {
        Node::Paragraph(Paragraph {
            children,
            position: None,
        })
    };
    let definition = |identifier: &str, url: &str| {
        Node::Definition(Definition {
            position: None,
            url: url.to_owned(),
            title: None,
            identifier: identifier.to_owned(),
            label: None,
        })
    };

    let unresolved = paragraph(vec![
        Node::LinkReference(LinkReference {
            children: vec![text("one")],
            position: None,
            reference_kind: ReferenceKind::Shortcut,
            identifier: "a".to_owned(),
            label: None,
        }),
        Node::LinkReference(LinkReference {
            children: vec![text("two")],
            position: None,
            reference_kind: ReferenceKind::Collapsed,
            identifier: "b".to_owned(),
            label: None,
        }),
        Node::LinkReference(LinkReference {
            children: vec![text("three")],
            position: None,
            reference_kind: ReferenceKind::Full,
            identifier: "c".to_owned(),
            label: Some("C".to_owned()),
        }),
        Node::ImageReference(ImageReference {
            position: None,
            alt: "pic".to_owned(),
            reference_kind: ReferenceKind::Full,
            identifier: "d".to_owned(),
            label: None,
        }),
        Node::ImageReference(ImageReference {
            position: None,
            alt: String::new(),
            reference_kind: ReferenceKind::Shortcut,
            identifier: "e".to_owned(),
            label: None,
        }),
    ]);
    set_fixture(
        &output,
        "unresolved",
        render_fixture_nodes(modules, &[unresolved])?,
    )?;

    let first_definition = vec![
        definition("dup", "https://example.com/first"),
        definition("dup", "https://example.com/second"),
        paragraph(vec![Node::LinkReference(LinkReference {
            children: vec![text("link")],
            position: None,
            reference_kind: ReferenceKind::Full,
            identifier: "dup".to_owned(),
            label: None,
        })]),
    ];
    set_fixture(
        &output,
        "firstDefinition",
        render_fixture_nodes(modules, &first_definition)?,
    )?;

    let bare_item = ListItem {
        children: vec![
            paragraph(vec![text("alpha")]),
            paragraph(vec![text("beta")]),
        ],
        position: None,
        spread: false,
        checked: None,
    };
    let mut context = MarkdownRenderContext::new(
        modules,
        false,
        JsValue::UNDEFINED,
        JsValue::UNDEFINED,
        create_reference_targets(),
    );
    let bare = render_list_item(
        &bare_item,
        list_item_loose(&bare_item, None),
        JsValue::from_str("bare"),
        &mut context,
    )?;
    set_fixture(&output, "bareListItem", fixture_root(modules, &[bare])?)?;

    let alignless_table = Table {
        children: vec![Node::TableRow(TableRow {
            children: vec![Node::TableCell(TableCell {
                children: vec![text("h")],
                position: None,
            })],
            position: None,
        })],
        position: None,
        align: Vec::new(),
    };
    let mut context = MarkdownRenderContext::new(
        modules,
        false,
        JsValue::UNDEFINED,
        JsValue::UNDEFINED,
        create_reference_targets(),
    );
    let table = render_table_with_alignment(
        &alignless_table,
        None,
        JsValue::from_str("table"),
        &mut context,
    )?;
    set_fixture(&output, "alignlessTable", fixture_root(modules, &[table])?)?;

    let padded_table = Node::Table(Table {
        children: vec![Node::TableRow(TableRow {
            children: vec![Node::TableCell(TableCell {
                children: vec![text("only")],
                position: None,
            })],
            position: None,
        })],
        position: None,
        align: vec![AlignKind::Left, AlignKind::Right],
    });
    set_fixture(
        &output,
        "paddedTable",
        render_fixture_nodes(modules, &[padded_table])?,
    )?;

    let checked = Node::List(List {
        children: vec![
            Node::ListItem(ListItem {
                children: Vec::new(),
                position: None,
                spread: false,
                checked: Some(true),
            }),
            Node::ListItem(ListItem {
                children: vec![paragraph(Vec::new())],
                position: None,
                spread: false,
                checked: Some(false),
            }),
        ],
        position: None,
        ordered: false,
        start: None,
        spread: false,
    });
    set_fixture(
        &output,
        "checkedEmpty",
        render_fixture_nodes(modules, &[checked])?,
    )?;

    let images = vec![
        definition("r", "https://example.com/r.png"),
        paragraph(vec![
            Node::Image(Image {
                position: None,
                alt: String::new(),
                url: "https://example.com/x.png".to_owned(),
                title: None,
            }),
            Node::ImageReference(ImageReference {
                position: None,
                alt: String::new(),
                reference_kind: ReferenceKind::Full,
                identifier: "r".to_owned(),
                label: None,
            }),
        ]),
    ];
    set_fixture(
        &output,
        "emptyImageAlt",
        render_fixture_nodes(modules, &images)?,
    )?;

    let nested_definition = Node::List(List {
        children: vec![Node::ListItem(ListItem {
            children: vec![
                paragraph(vec![text("body")]),
                definition("x", "https://example.com"),
            ],
            position: None,
            spread: true,
            checked: None,
        })],
        position: None,
        ordered: true,
        start: Some(3),
        spread: false,
    });
    set_fixture(
        &output,
        "nestedDefinition",
        render_fixture_nodes(modules, &[nested_definition])?,
    )?;

    let unmapped = vec![
        Node::Yaml(Yaml {
            value: "front: matter".to_owned(),
            position: None,
        }),
        Node::TableRow(TableRow {
            children: Vec::new(),
            position: None,
        }),
        paragraph(vec![text("after")]),
    ];
    set_fixture(
        &output,
        "unmapped",
        render_fixture_nodes(modules, &unmapped)?,
    )?;

    let mut missing = MarkdownRenderContext::new(
        modules,
        false,
        JsValue::UNDEFINED,
        JsValue::UNDEFINED,
        create_reference_targets(),
    );
    missing.footnote_order.push("GHOST".to_owned());
    missing.footnote_counts.insert("GHOST".to_owned(), 1);
    set_fixture(
        &output,
        "missingFootnote",
        render_footnote_section(&mut missing)?,
    )?;

    let quiet_definition = Node::FootnoteDefinition(MdFootnoteDefinition {
        children: vec![paragraph(vec![text("quiet")])],
        position: None,
        identifier: "q".to_owned(),
        label: None,
    });
    let mut quiet_targets = create_reference_targets();
    collect_reference_targets(&[quiet_definition], &mut quiet_targets);
    let mut quiet = MarkdownRenderContext::new(
        modules,
        false,
        JsValue::UNDEFINED,
        JsValue::UNDEFINED,
        quiet_targets,
    );
    quiet.footnote_order.push("Q".to_owned());
    set_fixture(
        &output,
        "uncountedFootnote",
        render_footnote_section(&mut quiet)?,
    )?;

    let code_definition = Node::FootnoteDefinition(MdFootnoteDefinition {
        children: vec![Node::Code(Code {
            value: "code body".to_owned(),
            position: None,
            lang: None,
            meta: None,
        })],
        position: None,
        identifier: "n".to_owned(),
        label: None,
    });
    let mut code_targets = create_reference_targets();
    collect_reference_targets(&[code_definition], &mut code_targets);
    let mut code_context = MarkdownRenderContext::new(
        modules,
        false,
        JsValue::UNDEFINED,
        JsValue::UNDEFINED,
        code_targets,
    );
    code_context.footnote_order.push("N".to_owned());
    code_context.footnote_counts.insert("N".to_owned(), 1);
    set_fixture(
        &output,
        "codeFootnote",
        render_footnote_section(&mut code_context)?,
    )?;
    Ok(output)
}

fn render_fixture_nodes(modules: &MarkdownModules, nodes: &[Node]) -> Result<JsValue, JsValue> {
    let mut targets = create_reference_targets();
    collect_reference_targets(nodes, &mut targets);
    let mut context = MarkdownRenderContext::new(
        modules,
        false,
        JsValue::UNDEFINED,
        JsValue::UNDEFINED,
        targets,
    );
    let rendered = render_blocks(nodes, &mut context)?;
    fixture_root(modules, &rendered)
}

fn fixture_root(modules: &MarkdownModules, children: &[JsValue]) -> Result<JsValue, JsValue> {
    create_element(&modules.react, &JsValue::from_str("div"), None, children)
}

#[allow(clippy::needless_pass_by_value)] // Callers hand off freshly rendered JS values.
fn set_fixture(output: &Object, name: &str, value: JsValue) -> Result<(), JsValue> {
    Reflect::set(output, &JsValue::from_str(name), &value).map(|_| ())
}

fn render_math(
    value: &str,
    display_mode: bool,
    key: JsValue,
    context: &MarkdownRenderContext<'_>,
) -> Result<JsValue, JsValue> {
    let children = render_tex_to_react(value, display_mode, context.modules)?;
    create_keyed_element(
        &context.modules.react,
        &context.modules.fragment,
        key,
        &children,
    )
}

fn render_tex_to_react(
    value: &str,
    display_mode: bool,
    modules: &MarkdownModules,
) -> Result<Vec<JsValue>, JsValue> {
    let strict_options = object(&[
        ("displayMode", JsValue::from_bool(display_mode)),
        ("throwOnError", JsValue::TRUE),
    ])?;
    let html = match call_method(
        &modules.backend,
        "renderTex",
        &[JsValue::from_str(value), strict_options.into()],
    ) {
        Ok(html) => html,
        Err(error) => {
            let fallback = object(&[
                ("displayMode", JsValue::from_bool(display_mode)),
                ("strict", JsValue::from_str("ignore")),
                ("throwOnError", JsValue::FALSE),
            ])?;
            if let Ok(html) = call_method(
                &modules.backend,
                "renderTex",
                &[JsValue::from_str(value), fallback.into()],
            ) {
                html
            } else {
                let title = js_string(&error)?;
                let span = create_element(
                    &modules.react,
                    &JsValue::from_str("span"),
                    Some(&object(&[
                        ("className", JsValue::from_str("katex-error")),
                        (
                            "style",
                            object(&[("color", JsValue::from_str("#cc0000"))])?.into(),
                        ),
                        ("title", JsValue::from_str(&title)),
                    ])?),
                    &[JsValue::from_str(value)],
                )?;
                return Ok(vec![span]);
            }
        }
    };
    let html = html
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("KaTeX returned a non-string"))?;
    let parser = required_function(&js_sys::global(), "DOMParser", "global")?;
    let parsed = Reflect::construct(&parser, &Array::new())?;
    let document = call_method(
        &parsed,
        "parseFromString",
        &[JsValue::from_str(&html), JsValue::from_str("text/html")],
    )?;
    let body = required_property(&document, "body", "KaTeX document")?;
    let nodes = required_property(&body, "childNodes", "KaTeX body")?;
    dom_children_to_react(&nodes, modules)
}

fn dom_children_to_react(
    nodes: &JsValue,
    modules: &MarkdownModules,
) -> Result<Vec<JsValue>, JsValue> {
    let length = required_u32(nodes, "length", "DOM childNodes")?;
    (0..length)
        .filter_map(|index| {
            let node = call_method(nodes, "item", &[JsValue::from_f64(f64::from(index))]);
            match node {
                Ok(node) if !node.is_null() => Some(dom_to_react(&node, index, modules)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn dom_to_react(node: &JsValue, key: u32, modules: &MarkdownModules) -> Result<JsValue, JsValue> {
    let node_type = required_u32(node, "nodeType", "KaTeX node")?;
    if node_type == 3 {
        return Reflect::get(node, &JsValue::from_str("textContent"));
    }
    if node_type != 1 {
        return Ok(JsValue::NULL);
    }
    let element_name = required_string(node, "localName", "KaTeX element")?;
    let props = Object::new();
    Reflect::set(
        &props,
        &JsValue::from_str("key"),
        &JsValue::from_f64(f64::from(key)),
    )?;
    let attributes = required_property(node, "attributes", "KaTeX element")?;
    let attribute_count = required_u32(&attributes, "length", "KaTeX attributes")?;
    for index in 0..attribute_count {
        let attribute = call_method(&attributes, "item", &[JsValue::from_f64(f64::from(index))])?;
        if attribute.is_null() {
            continue;
        }
        let name = required_string(&attribute, "name", "KaTeX attribute")?;
        let value = required_string(&attribute, "value", "KaTeX attribute")?;
        match name.as_str() {
            "class" => {
                Reflect::set(
                    &props,
                    &JsValue::from_str("className"),
                    &JsValue::from_str(&value),
                )?;
            }
            "style" => {
                Reflect::set(
                    &props,
                    &JsValue::from_str("style"),
                    &style_object(&value)?.into(),
                )?;
            }
            _ => {
                Reflect::set(
                    &props,
                    &JsValue::from_str(&name),
                    &JsValue::from_str(&value),
                )?;
            }
        }
    }
    let children = required_property(node, "childNodes", "KaTeX element")?;
    let children = dom_children_to_react(&children, modules)?;
    create_element(
        &modules.react,
        &JsValue::from_str(&element_name),
        Some(&props),
        &children,
    )
}

fn style_object(source: &str) -> Result<Object, JsValue> {
    let style = Object::new();
    for declaration in source.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let name = camel_case_css(name.trim());
        Reflect::set(
            &style,
            &JsValue::from_str(&name),
            &JsValue::from_str(value.trim()),
        )?;
    }
    Ok(style)
}

fn camel_case_css(source: &str) -> String {
    let mut output = String::new();
    let mut uppercase = false;
    for character in source.chars() {
        if character == '-' {
            uppercase = true;
        } else if uppercase && character.is_ascii_lowercase() {
            output.push(character.to_ascii_uppercase());
            uppercase = false;
        } else {
            output.push(character);
            uppercase = false;
        }
    }
    output
}

fn sanitize_url(url: &str) -> Option<String> {
    let Ok(protocol) = url_protocol(url) else {
        return None;
    };
    matches!(protocol.as_str(), "http:" | "https:" | "mailto:").then(|| url.to_owned())
}

fn index_key(index: usize) -> JsValue {
    JsValue::from_str(&index.to_string())
}

fn url_protocol(url: &str) -> Result<String, JsValue> {
    let constructor = required_function(&js_sys::global(), "URL", "global")?;
    let value = Reflect::construct(&constructor, &Array::of1(&JsValue::from_str(url)))?;
    required_string(&value, "protocol", "URL")
}

fn normalize_uri(url: &str, modules: &MarkdownModules) -> Result<String, JsValue> {
    call_method(&modules.backend, "normalizeUri", &[JsValue::from_str(url)])?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("normalizeUri returned a non-string").into())
}

fn identifier_key(identifier: &str) -> String {
    identifier.to_uppercase()
}

fn js_string(value: &JsValue) -> Result<String, JsValue> {
    required_function(&js_sys::global(), "String", "global")?
        .call1(&JsValue::UNDEFINED, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("String returned a non-string").into())
}

fn required_u32(value: &JsValue, key: &str, owner: &str) -> Result<u32, JsValue> {
    let number = required_property(value, key, owner)?
        .as_f64()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a number")))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(number as u32)
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

fn create_keyed_element(
    react: &JsValue,
    kind: &JsValue,
    key: JsValue,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    create_element(react, kind, Some(&object(&[("key", key)])?), children)
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
