//! Position-aware GFM parsing, plain-text projection, and incremental freezing.

use std::rc::Rc;

use markdown::{
    Constructs, ParseOptions,
    mdast::{Math, Node, Paragraph, Text},
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

/// Parses settled GFM plus dollar and TeX-compatible math delimiters.
///
/// Backslash delimiters are normalized only in parsed text regions, so fenced
/// code, inline code, HTML, and link destinations stay literal. Replacements
/// keep the original byte length, preserving mdast positions.
///
/// # Errors
///
/// Returns the parser's source diagnostic.
pub fn parse_gfm_with_math(text: &str) -> Result<Node, String> {
    let plain = to_mdast(text, &ParseOptions::gfm()).map_err(|error| error.to_string())?;
    let mut eligible = Vec::<(usize, usize)>::new();
    collect_text_ranges(&plain, &mut eligible);
    let normalized = normalize_tex_delimiters(text, &eligible);
    let mut options = ParseOptions::gfm();
    options.constructs = Constructs {
        math_flow: true,
        math_text: true,
        ..options.constructs
    };
    let mut root = to_mdast(&normalized, &options).map_err(|error| error.to_string())?;
    restore_compatibility_math_values(&mut root, text);
    promote_same_line_display_math(&mut root, text);
    rewrite_cjk_friendly_strong(&mut root, text);
    Ok(root)
}

fn collect_text_ranges(node: &Node, output: &mut Vec<(usize, usize)>) {
    if let Node::Text(text) = node
        && let Some(position) = &text.position
    {
        output.push((position.start.offset, position.end.offset));
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_text_ranges(child, output);
        }
    }
}

fn normalize_tex_delimiters(text: &str, eligible: &[(usize, usize)]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    normalize_same_line_dollars(text, eligible, &mut bytes);
    protect_escaped_dollars_in_math(text, eligible, &mut bytes);
    let paren_closes = next_compatibility_closes(text.as_bytes(), eligible, b')');
    let bracket_closes = next_compatibility_closes(text.as_bytes(), eligible, b']');
    let mut cursor = 0_usize;
    while cursor + 1 < bytes.len() {
        let closes = match (bytes[cursor], bytes[cursor + 1]) {
            (b'\\', b'(') => Some(&paren_closes),
            (b'\\', b'[') => Some(&bracket_closes),
            _ => None,
        };
        let Some(closes) = closes else {
            cursor += 1;
            continue;
        };
        if !delimiter_is_text(eligible, cursor) || is_escaped_backslash(&bytes, cursor) {
            cursor += 2;
            continue;
        }
        if let Some(close_start) = closes[cursor + 2]
            && (bytes[cursor + 1] != b'['
                || explicit_container_continuations(text, cursor, close_start))
        {
            bytes[cursor] = b'$';
            bytes[cursor + 1] = b'@';
            bytes[close_start] = b'@';
            bytes[close_start + 1] = b'$';
            cursor = close_start + 2;
        } else {
            cursor += 2;
        }
    }
    String::from_utf8(bytes).expect("same-width ASCII delimiter replacement preserves UTF-8")
}

fn next_compatibility_closes(
    bytes: &[u8],
    eligible: &[(usize, usize)],
    close: u8,
) -> Vec<Option<usize>> {
    let mut output = vec![None; bytes.len() + 1];
    let mut next = None;
    for index in (0..bytes.len()).rev() {
        if index + 1 < bytes.len()
            && bytes[index] == b'\\'
            && bytes[index + 1] == close
            && delimiter_is_text(eligible, index)
            && !is_escaped_backslash(bytes, index)
        {
            next = Some(index);
        }
        output[index] = next;
    }
    output
}

fn protect_escaped_dollars_in_math(text: &str, eligible: &[(usize, usize)], bytes: &mut [u8]) {
    let source = text.as_bytes();
    let mut line_start = 0_usize;
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let open = line_start + flow_content_start(body);
        if source.get(open..open + 2) != Some(b"$$")
            || source.get(open + 2) == Some(&b'$')
            || !range_contains(eligible, open)
        {
            line_start += line.len();
            continue;
        }
        let mut scan = open + 2;
        let mut close = None;
        while scan + 1 < source.len() {
            if source[scan] == b'$' && source[scan + 1] == b'$' && !dollar_is_escaped(source, scan)
            {
                close = Some(scan);
                break;
            }
            scan += 1;
        }
        if let Some(close) = close {
            for index in open + 2..close {
                if source[index] == b'$' && dollar_is_escaped(source, index) {
                    bytes[index] = b'@';
                }
            }
        }
        line_start += line.len();
    }
}

fn normalize_same_line_dollars(text: &str, eligible: &[(usize, usize)], bytes: &mut [u8]) {
    let mut line_start = 0_usize;
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let open_in_line = flow_content_start(body);
        let open = line_start + open_in_line;
        if !interrupts_open_paragraph(text, line_start)
            || body.as_bytes().get(open_in_line..open_in_line + 2) != Some(b"$$")
            || body.as_bytes().get(open_in_line + 2) == Some(&b'$')
            || !range_contains(eligible, open)
        {
            line_start += line.len();
            continue;
        }
        let mut scan = open + 2;
        let line_end = line_start + body.len();
        let mut close = None;
        while scan + 1 < line_end {
            if bytes[scan] == b'$'
                && bytes[scan + 1] == b'$'
                && !dollar_is_escaped(bytes, scan)
                && bytes[scan + 2..line_end]
                    .iter()
                    .all(u8::is_ascii_whitespace)
            {
                close = Some(scan);
                break;
            }
            scan += 1;
        }
        if let Some(close) = close
            && close > open + 2
        {
            bytes[open + 1] = b'@';
            bytes[close] = b'@';
        }
        line_start += line.len();
    }
}

fn interrupts_open_paragraph(text: &str, line_start: usize) -> bool {
    if line_start == 0 {
        return false;
    }
    !text[..line_start - 1]
        .rsplit_once('\n')
        .map_or(&text[..line_start - 1], |(_, line)| line)
        .trim()
        .is_empty()
}

fn flow_content_start(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut cursor = 0_usize;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    while bytes.get(cursor) == Some(&b'>') {
        cursor += 1;
        if bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
    }
    if matches!(bytes.get(cursor), Some(b'-' | b'+' | b'*'))
        && bytes.get(cursor + 1).is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 2;
    } else {
        let number_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor > number_start
            && matches!(bytes.get(cursor), Some(b'.' | b')'))
            && bytes.get(cursor + 1).is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 2;
        } else {
            cursor = number_start;
        }
    }
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn dollar_is_escaped(bytes: &[u8], offset: usize) -> bool {
    let mut slashes = 0_usize;
    let mut cursor = offset;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn explicit_container_continuations(text: &str, open: usize, close: usize) -> bool {
    let line_start = text[..open].rfind('\n').map_or(0, |index| index + 1);
    let prefix = &text[line_start..open];
    let quote = prefix.trim_start().starts_with('>');
    let list_indent = if prefix.trim_start().starts_with(['-', '+', '*']) {
        open - line_start
    } else {
        0
    };
    let Some(first_break) = text[open..close].find('\n') else {
        return true;
    };
    let continuation = &text[open + first_break + 1..close];
    continuation.lines().all(|line| {
        (!quote || line.trim_start().starts_with('>'))
            && (list_indent == 0
                || line
                    .bytes()
                    .take_while(|byte| matches!(byte, b' ' | b'\t'))
                    .count()
                    >= list_indent)
    })
}

fn range_contains(ranges: &[(usize, usize)], offset: usize) -> bool {
    ranges
        .iter()
        .any(|(start, end)| *start <= offset && offset < *end)
}

fn delimiter_is_text(ranges: &[(usize, usize)], offset: usize) -> bool {
    range_contains(ranges, offset) || range_contains(ranges, offset + 1)
}

fn is_escaped_backslash(bytes: &[u8], offset: usize) -> bool {
    let mut preceding = 0_usize;
    let mut cursor = offset;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        preceding += 1;
        cursor -= 1;
    }
    preceding % 2 == 1
}

fn restore_compatibility_math_values(node: &mut Node, source: &str) {
    let replacement = if let Node::InlineMath(math) = node
        && let Some(position) = &math.position
        && let Some(raw) = source.get(position.start.offset..position.end.offset)
    {
        let raw = raw.trim();
        if let Some(value) = dollar_math_content(raw) {
            math.value = value;
            None
        } else if (raw.starts_with("\\(") || raw.starts_with("\\["))
            && let Some(value) = math
                .value
                .strip_prefix('@')
                .and_then(|value| value.strip_suffix('@'))
        {
            if raw.starts_with("\\[") {
                Some(Node::Math(Math {
                    value: value.trim().to_owned(),
                    position: math.position.clone(),
                    meta: None,
                }))
            } else {
                math.value = value.to_owned();
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    if let Some(replacement) = replacement {
        *node = replacement;
        return;
    }
    if let Node::Math(math) = node {
        math.value = math
            .position
            .as_ref()
            .and_then(|position| source.get(position.start.offset..position.end.offset))
            .and_then(|raw| dollar_math_content(raw.trim()))
            .unwrap_or_else(|| math.value.trim().to_owned());
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            restore_compatibility_math_values(child, source);
        }
    }
}

fn dollar_math_content(raw: &str) -> Option<String> {
    (raw.starts_with("$$") && !raw.starts_with("$$$") && raw.ends_with("$$"))
        .then(|| raw[2..raw.len() - 2].trim().to_owned())
}

fn promote_same_line_display_math(node: &mut Node, source: &str) {
    let Some(children) = node.children_mut() else {
        return;
    };
    let mut output = Vec::with_capacity(children.len());
    for mut child in std::mem::take(children) {
        if let Node::Paragraph(paragraph) = child {
            output.extend(split_display_paragraph(paragraph, source));
        } else {
            promote_same_line_display_math(&mut child, source);
            output.push(child);
        }
    }
    *children = output;
}

fn split_display_paragraph(paragraph: Paragraph, source: &str) -> Vec<Node> {
    let position = paragraph.position.clone();
    if let Some(position) = &position
        && let Some(raw) = source.get(position.start.offset..position.end.offset)
        && let Some(value) = same_line_dollar_value(raw)
    {
        return vec![Node::Math(Math {
            value,
            position: Some(position.clone()),
            meta: None,
        })];
    }
    let mut output = Vec::new();
    let mut inline = Vec::new();
    for child in paragraph.children {
        let display = if let Node::InlineMath(math) = &child
            && let Some(position) = &math.position
            && is_flow_display_position(source, position)
        {
            Some(Node::Math(Math {
                value: math.value.clone(),
                position: Some(position.clone()),
                meta: None,
            }))
        } else {
            None
        };
        if let Some(display) = display {
            if !inline.is_empty() {
                output.push(Node::Paragraph(Paragraph {
                    children: std::mem::take(&mut inline),
                    position: position.clone(),
                }));
            }
            output.push(display);
        } else {
            inline.push(child);
        }
    }
    if !inline.is_empty() {
        output.push(Node::Paragraph(Paragraph {
            children: inline,
            position,
        }));
    }
    output
}

fn same_line_dollar_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.contains(['\n', '\r']) || !raw.starts_with("$$") || raw.starts_with("$$$") {
        return None;
    }
    let bytes = raw.as_bytes();
    let mut cursor = 2_usize;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'$'
            && bytes[cursor + 1] == b'$'
            && !dollar_is_escaped(bytes, cursor)
            && bytes[cursor + 2..].iter().all(u8::is_ascii_whitespace)
        {
            return Some(raw[2..cursor].to_owned());
        }
        cursor += 1;
    }
    None
}

fn is_flow_display_position(source: &str, position: &Position) -> bool {
    let Some(raw) = source.get(position.start.offset..position.end.offset) else {
        return false;
    };
    if !is_same_line_display(raw) {
        return false;
    }
    let line_start = source[..position.start.offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = source[position.end.offset..]
        .find('\n')
        .map_or(source.len(), |index| position.end.offset + index);
    flow_content_start(&source[line_start..line_end]) == position.start.offset - line_start
        && source[position.end.offset..line_end]
            .bytes()
            .all(|byte| byte.is_ascii_whitespace())
}

fn is_same_line_display(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.starts_with("\\[") && raw.ends_with("\\]") {
        return true;
    }
    if raw.contains(['\n', '\r']) {
        return false;
    }
    raw.starts_with("$$") && !raw.starts_with("$$$") && raw.ends_with("$$")
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
