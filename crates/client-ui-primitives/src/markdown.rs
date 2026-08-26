//! Position-aware GFM parsing, plain-text projection, and incremental freezing.

use std::rc::Rc;

use markdown::{
    ParseOptions,
    mdast::{Node, Text},
    to_mdast,
    unist::{Point, Position},
};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_script::{Script, UnicodeScript as _};

const UNSTABLE_TAIL_BLOCKS: usize = 2;

/// Parses the streaming grammar: GFM without math constructs.
///
/// # Errors
///
/// Returns the parser's source diagnostic.
pub fn parse_gfm(text: &str) -> Result<Node, String> {
    let mut root = to_mdast(text, &ParseOptions::gfm()).map_err(|error| error.to_string())?;
    rewrite_cjk_friendly_strong(&mut root, text);
    Ok(root)
}

fn rewrite_cjk_friendly_strong(root: &mut Node, source: &str) {
    let Some(children) = root.children_mut() else {
        return;
    };
    let mut rewritten = Vec::with_capacity(children.len());
    for mut child in std::mem::take(children) {
        if let Node::Text(text) = &child
            && let Some(parts) = cjk_strong_text_parts(text, source)
        {
            rewritten.extend(parts);
            continue;
        }
        rewrite_cjk_friendly_strong(&mut child, source);
        rewritten.push(child);
    }
    *children = rewritten;
}

fn cjk_strong_text_parts(text: &Text, source: &str) -> Option<Vec<Node>> {
    let position = text.position.as_ref()?;
    let raw = source.get(position.start.offset..position.end.offset)?;
    if raw != text.value {
        return None;
    }
    let mut output = Vec::new();
    let mut emitted = 0_usize;
    let mut search = 0_usize;
    while let Some(relative_open) = raw[search..].find("**") {
        let open = search + relative_open;
        let content_start = open + 2;
        if is_escaped_marker(source, position.start.offset + open) {
            search = content_start;
            continue;
        }
        let mut close_search = content_start;
        let mut accepted = None;
        while let Some(relative_close) = raw[close_search..].find("**") {
            let close = close_search + relative_close;
            let content = &raw[content_start..close];
            let after = raw[close + 2..].chars().next();
            if !content.is_empty()
                && content.chars().last().is_some_and(is_unicode_punctuation)
                && after.is_some_and(is_cjk_character)
                && let Some(strong) = parse_standard_strong(content)
            {
                accepted = Some((close, strong));
                break;
            }
            close_search = close + 2;
        }
        let Some((close, mut strong)) = accepted else {
            search = content_start;
            continue;
        };
        if emitted < open {
            output.push(Node::Text(Text {
                value: raw[emitted..open].to_owned(),
                position: Some(subposition(position, raw, emitted, open)),
            }));
        }
        remap_relative_positions(
            &mut strong,
            &point_after(&position.start, raw, open),
            &raw[open..close + 2],
        );
        output.push(strong);
        emitted = close + 2;
        search = emitted;
    }
    if output.is_empty() {
        return None;
    }
    if emitted < raw.len() {
        output.push(Node::Text(Text {
            value: raw[emitted..].to_owned(),
            position: Some(subposition(position, raw, emitted, raw.len())),
        }));
    }
    Some(output)
}

fn is_escaped_marker(source: &str, offset: usize) -> bool {
    source[..offset]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

fn parse_standard_strong(content: &str) -> Option<Node> {
    let candidate = format!("**{content}** ");
    let Node::Root(root) = to_mdast(&candidate, &ParseOptions::gfm()).ok()? else {
        return None;
    };
    let Node::Paragraph(paragraph) = root.children.into_iter().next()? else {
        return None;
    };
    let mut children = paragraph.children.into_iter();
    let strong = children.next()?;
    let spans_whole_candidate = strong.position().is_some_and(|position| {
        position.start.offset == 0 && position.end.offset == content.len() + 4
    });
    (matches!(&strong, Node::Strong(_)) && spans_whole_candidate).then_some(strong)
}

fn subposition(position: &Position, raw: &str, start: usize, end: usize) -> Position {
    Position {
        start: point_after(&position.start, raw, start),
        end: point_after(&position.start, raw, end),
    }
}

fn point_after(start: &Point, raw: &str, end: usize) -> Point {
    let prefix = raw
        .get(..end)
        .expect("text-node offsets are UTF-8 boundaries");
    let lines = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rsplit_once('\n')
        .map_or(start.column + prefix.chars().count(), |(_, tail)| {
            1 + tail.chars().count()
        });
    Point {
        line: start.line + lines,
        column,
        offset: start.offset + end,
    }
}

fn remap_relative_positions(node: &mut Node, start: &Point, raw: &str) {
    if let Some(position) = node.position() {
        node.position_set(Some(Position {
            start: point_after(start, raw, position.start.offset),
            end: point_after(start, raw, position.end.offset),
        }));
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            remap_relative_positions(child, start, raw);
        }
    }
}

fn is_unicode_punctuation(character: char) -> bool {
    matches!(
        get_general_category(character),
        GeneralCategory::ClosePunctuation
            | GeneralCategory::ConnectorPunctuation
            | GeneralCategory::CurrencySymbol
            | GeneralCategory::DashPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::MathSymbol
            | GeneralCategory::ModifierSymbol
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::OtherSymbol
    )
}

fn is_cjk_character(character: char) -> bool {
    let scripts = character.script_extension();
    [
        Script::Han,
        Script::Hiragana,
        Script::Katakana,
        Script::Hangul,
        Script::Bopomofo,
    ]
    .into_iter()
    .any(|script| scripts.contains_script(script))
}

/// Amount of parsed Markdown returned by [`extract_markdown_plain_text`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkdownPlainTextMode {
    /// Complete document.
    #[default]
    All,
    /// First non-empty projected line.
    FirstLine,
    /// First semantic paragraph, falling back to the first visible line.
    FirstParagraph,
}

/// Parses GFM Markdown and removes presentation syntax while retaining content.
///
/// Raw HTML stays literal; links keep labels, images keep alt text, and code
/// keeps its source text.
///
/// # Errors
///
/// Returns the parser's source diagnostic.
pub fn extract_markdown_plain_text(
    markdown: &str,
    mode: MarkdownPlainTextMode,
) -> Result<String, String> {
    let root = parse_gfm(markdown)?;
    let all = full_text(&root);
    Ok(match mode {
        MarkdownPlainTextMode::All => all,
        MarkdownPlainTextMode::FirstLine => first_visible_line(&all),
        MarkdownPlainTextMode::FirstParagraph => {
            find_first_paragraph(&root).unwrap_or_else(|| first_visible_line(&all))
        }
    })
}

fn inline_text(node: &Node) -> String {
    match node {
        Node::Text(node) => node.value.clone(),
        Node::InlineCode(node) => node.value.clone(),
        Node::Code(node) => node.value.clone(),
        Node::Image(node) => node.alt.clone(),
        Node::ImageReference(node) => node.alt.clone(),
        Node::Break(_) => "\n".to_owned(),
        Node::Html(node) => node.value.clone(),
        _ => node
            .children()
            .map(|children| children.iter().map(inline_text).collect())
            .unwrap_or_default(),
    }
}

fn compact_inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn block_text(node: &Node) -> String {
    match node {
        Node::Root(_) | Node::Blockquote(_) => join_blocks(node, "\n\n"),
        Node::Code(node) => node.value.trim().to_owned(),
        Node::List(_) | Node::Table(_) => join_blocks(node, "\n"),
        Node::ListItem(_) => join_blocks(node, " "),
        Node::TableRow(_) => node
            .children()
            .map(|children| {
                children
                    .iter()
                    .map(block_text)
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .unwrap_or_default(),
        Node::Html(node) => node.value.clone(),
        Node::ThematicBreak(_) | Node::Definition(_) => String::new(),
        _ => compact_inline(&inline_text(node)),
    }
}

fn join_blocks(node: &Node, separator: &str) -> String {
    node.children()
        .map(|children| {
            children
                .iter()
                .map(block_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(separator)
        })
        .unwrap_or_default()
}

fn find_first_paragraph(node: &Node) -> Option<String> {
    if matches!(node, Node::Paragraph(_)) {
        let text = compact_inline(&inline_text(node));
        if !text.is_empty() {
            return Some(text);
        }
    }
    node.children()
        .and_then(|children| children.iter().find_map(find_first_paragraph))
}

fn full_text(root: &Node) -> String {
    let mut output = Vec::<String>::new();
    let mut empty_run = 0_usize;
    for line in block_text(root).lines().map(str::trim) {
        if line.is_empty() {
            empty_run += 1;
            if empty_run == 1 {
                output.push(String::new());
            }
        } else {
            empty_run = 0;
            output.push(line.to_owned());
        }
    }
    while output.first().is_some_and(String::is_empty) {
        output.remove(0);
    }
    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    output.join("\n")
}

fn first_visible_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// One top-level block and its stream-stable JavaScript source-offset key.
#[derive(Clone)]
pub struct PositionedMarkdownBlock {
    /// Parsed node; positions remain relative to its parse slice.
    pub node: Rc<Node>,
    /// Absolute UTF-16 source start, or a negative sibling fallback.
    pub key: i64,
}

impl std::fmt::Debug for PositionedMarkdownBlock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PositionedMarkdownBlock")
            .field("node", &self.node)
            .field("key", &self.key)
            .finish()
    }
}

/// One incremental update result.
#[derive(Clone, Debug)]
pub struct IncrementalMarkdownBlocks {
    /// Monotonically growing settled prefix.
    pub frozen: Vec<Rc<PositionedMarkdownBlock>>,
    /// Re-parsed frontier.
    pub tail: Vec<Rc<PositionedMarkdownBlock>>,
    /// Changes whenever non-append input discards the prefix.
    pub generation: u64,
}

type MarkdownParser = dyn Fn(&str) -> Result<Node, String>;

/// Append-only block parser that freezes all but the trailing two blocks.
pub struct IncrementalMarkdownParser {
    parser: Rc<MarkdownParser>,
    previous_text: String,
    tail_start_utf16: usize,
    frozen: Vec<Rc<PositionedMarkdownBlock>>,
    generation: u64,
    cached: Option<Rc<IncrementalMarkdownBlocks>>,
}

impl std::fmt::Debug for IncrementalMarkdownParser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncrementalMarkdownParser")
            .field("previous_text", &self.previous_text)
            .field("tail_start_utf16", &self.tail_start_utf16)
            .field("frozen", &self.frozen)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Default for IncrementalMarkdownParser {
    fn default() -> Self {
        Self::new(Rc::new(parse_gfm))
    }
}

impl IncrementalMarkdownParser {
    /// Creates a parser over the same grammar the caller renders.
    #[must_use]
    pub fn new(parser: Rc<MarkdownParser>) -> Self {
        Self {
            parser,
            previous_text: String::new(),
            tail_start_utf16: 0,
            frozen: Vec::new(),
            generation: 0,
            cached: None,
        }
    }

    /// Updates the full accumulated source, resetting on non-append input.
    ///
    /// Identical input returns the same result allocation.
    ///
    /// # Errors
    ///
    /// Returns the configured parser's diagnostic.
    pub fn update(&mut self, text: &str) -> Result<Rc<IncrementalMarkdownBlocks>, String> {
        if text == self.previous_text
            && let Some(cached) = &self.cached
        {
            return Ok(cached.clone());
        }
        if !text.starts_with(&self.previous_text) {
            self.previous_text.clear();
            self.tail_start_utf16 = 0;
            self.frozen.clear();
            self.generation = self.generation.wrapping_add(1);
        }
        text.clone_into(&mut self.previous_text);
        let base = self.tail_start_utf16;
        let tail_source = slice_from_utf16(text, base);
        let root = (self.parser)(tail_source)?;
        let Node::Root(root) = root else {
            return Err("Markdown parser did not return a root node".to_owned());
        };
        let mut blocks = root.children;
        let mut first_unstable = blocks.len().saturating_sub(UNSTABLE_TAIL_BLOCKS);
        if first_unstable > 0 {
            let cut = blocks[first_unstable - 1]
                .position()
                .map(|position| byte_offset_to_utf16(tail_source, position.end.offset));
            if let Some(cut) = cut {
                for node in blocks.drain(..first_unstable) {
                    let key = block_key(&node, tail_source, base, self.frozen.len());
                    self.frozen.push(Rc::new(PositionedMarkdownBlock {
                        node: Rc::new(node),
                        key,
                    }));
                }
                self.tail_start_utf16 = base + cut;
                first_unstable = 0;
            } else {
                first_unstable = 0;
            }
        }
        let tail = blocks
            .into_iter()
            .skip(first_unstable)
            .enumerate()
            .map(|(index, node)| {
                Rc::new(PositionedMarkdownBlock {
                    key: block_key(&node, tail_source, base, index),
                    node: Rc::new(node),
                })
            })
            .collect();
        let result = Rc::new(IncrementalMarkdownBlocks {
            frozen: self.frozen.clone(),
            tail,
            generation: self.generation,
        });
        self.cached = Some(result.clone());
        Ok(result)
    }
}

fn block_key(node: &Node, source: &str, base: usize, index: usize) -> i64 {
    node.position().map_or_else(
        || -i64::try_from(index + 1).unwrap_or(i64::MAX),
        |position| {
            i64::try_from(base + byte_offset_to_utf16(source, position.start.offset))
                .unwrap_or(i64::MAX)
        },
    )
}

fn byte_offset_to_utf16(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).encode_utf16().count()
}

fn slice_from_utf16(text: &str, offset: usize) -> &str {
    if offset == 0 {
        return text;
    }
    let mut units = 0_usize;
    for (byte, character) in text.char_indices() {
        if units >= offset {
            return &text[byte..];
        }
        units += character.len_utf16();
    }
    ""
}
