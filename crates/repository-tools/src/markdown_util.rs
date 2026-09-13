//! Shared GFM parsing and source-preserving traversal for documentation checks.

use std::collections::{HashMap, HashSet};

use markdown::{ParseOptions, mdast::Node, to_mdast};
use regex::Regex;

/// One authored Markdown line outside code and rendered-away HTML comments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownProseLine {
    /// 1-based source line.
    pub index: usize,
    /// Original source text without normalization.
    pub raw: String,
}

/// One parsed heading with its source line and reader-visible text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownHeadingLine {
    /// ATX or Setext depth from one through six.
    pub depth: u8,
    /// 1-based source line.
    pub index: usize,
    /// Authored first source line.
    pub raw: String,
    /// Rendered text with raw HTML omitted.
    pub text: String,
}

/// One fenced or indented code block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownFence {
    /// 1-based opening source line.
    pub line: usize,
    /// First info-string word, absent for bare or indented code.
    pub lang: Option<String>,
    /// Full info string.
    pub info: String,
    /// Block body without delimiters.
    pub code: String,
    /// Whether an explicit closing fence terminates the block.
    pub closed: bool,
}

/// Parses GitHub-flavored Markdown with the repository's standard grammar.
///
/// # Errors
///
/// Returns the parser diagnostic.
pub fn parse_markdown(source: &str) -> Result<Node, String> {
    to_mdast(source, &ParseOptions::gfm()).map_err(|error| error.to_string())
}

/// Visits one Markdown tree depth-first; `false` prunes the current children.
pub fn visit_markdown(node: &Node, visitor: &mut impl FnMut(&Node) -> bool) {
    if !visitor(node) {
        return;
    }
    if let Some(children) = node.children() {
        for child in children {
            visit_markdown(child, visitor);
        }
    }
}

/// Extracts every parsed code block with source metadata.
///
/// # Errors
///
/// Returns the Markdown parser diagnostic.
pub fn markdown_fences(source: &str) -> Result<Vec<MarkdownFence>, String> {
    static CLOSING: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^ {0,3}(`{3,}|~{3,})\s*$").expect("static regex"));
    let lines = source.split('\n').collect::<Vec<_>>();
    let root = parse_markdown(source)?;
    let mut fences = Vec::new();
    visit_markdown(&root, &mut |node| {
        if let Node::Code(code) = node
            && let Some(position) = &code.position
        {
            let info = code.lang.as_ref().map_or_else(String::new, |lang| {
                code.meta
                    .as_ref()
                    .filter(|meta| !meta.is_empty())
                    .map_or_else(|| lang.clone(), |meta| format!("{lang} {meta}"))
            });
            let end_line = lines
                .get(position.end.line - 1)
                .copied()
                .unwrap_or_default();
            fences.push(MarkdownFence {
                line: position.start.line,
                lang: code.lang.clone(),
                info,
                code: code.value.clone(),
                closed: CLOSING.is_match(end_line),
            });
        }
        true
    });
    Ok(fences)
}

/// Returns every parsed heading with rendered text and source location.
///
/// # Errors
///
/// Returns the Markdown parser diagnostic.
pub fn markdown_heading_lines(source: &str) -> Result<Vec<MarkdownHeadingLine>, String> {
    let raw_lines = source.split('\n').collect::<Vec<_>>();
    let root = parse_markdown(source)?;
    let mut headings = Vec::new();
    visit_markdown(&root, &mut |node| {
        if let Node::Heading(heading) = node
            && let Some(position) = &heading.position
        {
            headings.push(MarkdownHeadingLine {
                depth: heading.depth,
                index: position.start.line,
                raw: raw_lines
                    .get(position.start.line - 1)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                text: rendered_text(node),
            });
        }
        true
    });
    Ok(headings)
}

/// Returns source lines outside code blocks and rendered-away HTML comments.
///
/// # Errors
///
/// Returns the Markdown parser diagnostic.
pub fn markdown_prose_lines(source: &str) -> Result<Vec<MarkdownProseLine>, String> {
    let raw_lines = source.split('\n').collect::<Vec<_>>();
    let root = parse_markdown(source)?;
    let comments = html_comment_ranges(&root, &raw_lines);
    let mut fenced = HashSet::new();
    visit_markdown(&root, &mut |node| {
        if let Node::Code(code) = node
            && let Some(position) = &code.position
        {
            for line in position.start.line..=position.end.line {
                fenced.insert(line);
            }
        }
        true
    });
    let mut kept = Vec::new();
    for (index, raw) in raw_lines.into_iter().enumerate() {
        let line = index + 1;
        if fenced.contains(&line) {
            continue;
        }
        if has_rendered_text_outside_comments(raw, comments.get(&line).map(Vec::as_slice)) {
            kept.push(MarkdownProseLine {
                index: line,
                raw: raw.to_owned(),
            });
        }
    }
    Ok(kept)
}

fn rendered_text(node: &Node) -> String {
    match node {
        Node::Text(text) => text.value.clone(),
        Node::InlineCode(code) => code.value.clone(),
        Node::Image(image) => image.alt.clone(),
        Node::ImageReference(image) => image.alt.clone(),
        Node::Break(_) => " ".to_owned(),
        Node::Html(_) => String::new(),
        _ => node
            .children()
            .map(|children| children.iter().map(rendered_text).collect())
            .unwrap_or_default(),
    }
}

type ColumnRange = (usize, usize);
type OffsetRange = (usize, usize);

fn html_comment_ranges(root: &Node, raw_lines: &[&str]) -> HashMap<usize, Vec<ColumnRange>> {
    let mut comments = Vec::<OffsetRange>::new();
    visit_markdown(root, &mut |node| {
        if let Node::Html(html) = node
            && let Some(position) = &html.position
        {
            let mut cursor = 0;
            while let Some(relative_start) = html.value[cursor..].find("<!--") {
                let start = cursor + relative_start;
                let close = html.value[start + 4..]
                    .find("-->")
                    .map(|close| start + 4 + close);
                let end = close.map_or(html.value.len(), |close| close + 3);
                comments.push((position.start.offset + start, position.start.offset + end));
                cursor = end;
            }
        }
        true
    });
    let mut ranges = HashMap::<usize, Vec<ColumnRange>>::new();
    let mut line_offset = 0;
    for (index, raw) in raw_lines.iter().enumerate() {
        let line_end = line_offset + raw.len();
        for (start, end) in &comments {
            let from = (*start).max(line_offset);
            let to = (*end).min(line_end);
            let covers_empty = raw.is_empty() && *start <= line_offset && *end > line_offset;
            if from < to || covers_empty {
                ranges
                    .entry(index + 1)
                    .or_default()
                    .push((from - line_offset, to - line_offset));
            }
        }
        line_offset = line_end + 1;
    }
    ranges
}

fn has_rendered_text_outside_comments(raw: &str, ranges: Option<&[ColumnRange]>) -> bool {
    let Some(ranges) = ranges else {
        return true;
    };
    let mut ranges = ranges.to_vec();
    ranges.sort_by_key(|range| range.0);
    let mut cursor = 0;
    let mut visible = String::new();
    for (start, end) in ranges {
        visible.push_str(&raw[cursor..start]);
        cursor = cursor.max(end);
    }
    visible.push_str(&raw[cursor..]);
    !visible.trim().is_empty()
}
