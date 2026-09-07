//! Shared `JSDoc` parsing and completeness checks for the Cordis catalogs.
//!
//! Mirrors `scripts/jsdoc.ts`: prose ends at the first block tag, paragraphs
//! collapse to one line, bullet items stay on their own lines, `{@link X}`
//! renders as `X`, and `@param`/`@returns` completeness is judged against the
//! declared parameter list.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

/// ECMAScript `\s`: `WhiteSpace` plus `LineTerminator` code points.
#[must_use]
pub const fn is_js_space(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

const JS_SPACE_CLASS: &str = r"[\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";

fn js_regex(source: &str) -> Regex {
    Regex::new(
        &source
            .replace(r"\s", JS_SPACE_CLASS)
            .replace(r"\w", "A-Za-z0-9_"),
    )
    .expect("static JSDoc expression")
}

static MODE_TAG: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^@mode\s+(emit|waterfall|parallel|serial|bail)\s*$"));
static PARAM_TAG: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^@param\s+(\[?[\w$]+\]?)\s*(?:[-—–]\s*)?(.*)$"));
static RETURNS_TAG: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^@returns?(?:\s+[-—–]?\s*(.*))?$"));
static LINK: LazyLock<Regex> = LazyLock::new(|| js_regex(r"\{@link\s+([^}]+)\}"));

/// A dispatch mode, rendered as the badge after an event name in the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Synchronous broadcast.
    Emit,
    /// Listener-controlled continuation.
    Waterfall,
    /// Concurrent awaited listeners.
    Parallel,
    /// Sequential awaited listeners.
    Serial,
    /// First nonempty result.
    Bail,
}

impl Mode {
    /// Source documentation spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Emit => "emit",
            Self::Waterfall => "waterfall",
            Self::Parallel => "parallel",
            Self::Serial => "serial",
            Self::Bail => "bail",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "emit" => Some(Self::Emit),
            "waterfall" => Some(Self::Waterfall),
            "parallel" => Some(Self::Parallel),
            "serial" => Some(Self::Serial),
            "bail" => Some(Self::Bail),
            _ => None,
        }
    }
}

/// Description prose plus the optional `@mode` tag of one `JSDoc` block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedJsDoc {
    /// Collapsed description prose.
    pub doc: String,
    /// Parsed valid `@mode`, absent when the tag is missing or invalid.
    pub mode: Option<Mode>,
    /// Whether any `@mode` tag was present.
    pub has_mode: bool,
}

/// `@param` descriptions by name plus the `@returns` description.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedTags {
    /// `@param` name to description, in tag order.
    pub params: IndexMap<String, String>,
    /// `@returns` description: absent when the tag is missing, empty when present but empty.
    pub returns: Option<String>,
}

/// Repo-relative source pointer `file:line` for a node's first character.
#[must_use]
pub fn pointer(rel: &str, line: usize) -> String {
    format!("{rel}:{line}")
}

/// One comment range in a source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommentRange {
    /// Byte offset of the comment opener.
    pub pos: usize,
    /// Byte offset just past the comment.
    pub end: usize,
}

/// Comments starting at `pos` up to the next token, mirroring the compiler's
/// `getLeadingCommentRanges`: past position zero, a comment on the same line
/// as the preceding token is that token's trailing comment and is skipped
/// until a line break starts the leading trivia.
#[must_use]
pub fn leading_comment_ranges(text: &str, pos: usize) -> Vec<CommentRange> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut index = pos.min(bytes.len());
    let mut collecting = pos == 0;
    if pos == 0 && text.starts_with("#!") {
        while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
            index += 1;
        }
    }
    loop {
        let Some(character) = text[index..].chars().next() else {
            return ranges;
        };
        if matches!(character, '\n' | '\r') {
            collecting = true;
            index += 1;
            continue;
        }
        if is_js_space(character) {
            index += character.len_utf8();
            continue;
        }
        if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'/' {
            let mut end = index;
            while end < bytes.len() && !matches!(bytes[end], b'\n' | b'\r') {
                end += 1;
            }
            if collecting {
                ranges.push(CommentRange { pos: index, end });
            }
            index = end;
            continue;
        }
        if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
            let close = text[index + 2..]
                .find("*/")
                .map_or(bytes.len(), |offset| index + 2 + offset + 2);
            if collecting {
                ranges.push(CommentRange {
                    pos: index,
                    end: close,
                });
            }
            index = close;
            continue;
        }
        return ranges;
    }
}

/// The raw `/** … */` block immediately preceding a node, or empty when none.
#[must_use]
pub fn raw_jsdoc(text: &str, full_start: usize) -> String {
    leading_comment_ranges(text, full_start)
        .into_iter()
        .rfind(|range| text.get(range.pos..range.pos + 3) == Some("/**"))
        .map(|range| text[range.pos..range.end].to_owned())
        .unwrap_or_default()
}

fn inner_lines(raw: &str) -> Vec<String> {
    let inner = raw.strip_prefix("/**").unwrap_or(raw);
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    inner
        .split('\n')
        .map(|line| {
            let mut rest = line.trim_start_matches(is_js_space);
            if let Some(after) = rest.strip_prefix('*') {
                rest = after;
                if let Some(after_space) = rest.strip_prefix(is_js_space) {
                    rest = after_space;
                }
            }
            rest.trim_end_matches(is_js_space).to_owned()
        })
        .collect()
}

fn join(parts: &[String]) -> String {
    collapse(&parts.join(" "))
}

/// `value.replace(/\s+/g, ' ').trim()`.
#[must_use]
pub fn collapse(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut in_space = false;
    for character in value.chars() {
        if is_js_space(character) {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(character);
            in_space = false;
        }
    }
    result.trim_matches(is_js_space).to_owned()
}

fn flush_item(item: &mut Vec<String>, list: &mut Vec<String>) {
    if !item.is_empty() {
        list.push(join(item));
    }
    item.clear();
}
fn flush_list(item: &mut Vec<String>, list: &mut Vec<String>, blocks: &mut Vec<String>) {
    flush_item(item, list);
    if !list.is_empty() {
        blocks.push(list.join("\n"));
    }
    list.clear();
}
fn flush_paragraph(
    item: &mut Vec<String>,
    list: &mut Vec<String>,
    paragraph: &mut Vec<String>,
    blocks: &mut Vec<String>,
) {
    flush_list(item, list, blocks);
    if !paragraph.is_empty() {
        blocks.push(join(paragraph));
    }
    paragraph.clear();
}

/// Parses a raw `JSDoc` block into description prose and an optional `@mode`.
#[must_use]
pub fn parse_jsdoc(raw: &str) -> ParsedJsDoc {
    let mut mode = None;
    let mut has_mode = false;
    let mut in_tags = false;
    let mut blocks: Vec<String> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let mut item: Vec<String> = Vec::new();
    for line in inner_lines(raw) {
        let tag_line = line.trim_start_matches(is_js_space);
        if let Some(captures) = MODE_TAG.captures(tag_line) {
            mode = Mode::parse(&captures[1]);
            has_mode = true;
            flush_paragraph(&mut item, &mut list, &mut paragraph, &mut blocks);
            in_tags = true;
            continue;
        }
        if tag_line == "@mode"
            || tag_line.strip_prefix("@mode").is_some_and(|rest| {
                rest.chars()
                    .next()
                    .is_some_and(|c| !(c.is_ascii_alphanumeric() || c == '_'))
            })
        {
            has_mode = true;
            flush_paragraph(&mut item, &mut list, &mut paragraph, &mut blocks);
            in_tags = true;
            continue;
        }
        if tag_line.starts_with('@') {
            flush_paragraph(&mut item, &mut list, &mut paragraph, &mut blocks);
            in_tags = true;
            continue;
        }
        if in_tags {
            continue;
        }
        if line.trim_matches(is_js_space).is_empty() {
            flush_paragraph(&mut item, &mut list, &mut paragraph, &mut blocks);
            continue;
        }
        if line.starts_with('-') && line[1..].starts_with(is_js_space) {
            flush_item(&mut item, &mut list);
            if !paragraph.is_empty() {
                blocks.push(join(&paragraph));
                paragraph.clear();
            }
            item.push(line);
            continue;
        }
        if !item.is_empty() {
            item.push(line);
            continue;
        }
        paragraph.push(line);
    }
    flush_paragraph(&mut item, &mut list, &mut paragraph, &mut blocks);
    let doc = LINK
        .replace_all(&blocks.join("\n\n"), "$1")
        .trim_matches(is_js_space)
        .to_owned();
    ParsedJsDoc {
        doc,
        mode,
        has_mode,
    }
}

/// Parses `@param` and `@returns` descriptions, including continuation lines.
#[must_use]
pub fn parse_tags(raw: &str) -> ParsedTags {
    enum Sink {
        Param(String),
        Returns,
    }
    let mut parsed = ParsedTags::default();
    let mut sink: Option<Sink> = None;
    for line in inner_lines(raw) {
        if let Some(captures) = PARAM_TAG.captures(&line) {
            let name = captures[1]
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned();
            parsed.params.insert(
                name.clone(),
                captures.get(2).map_or("", |m| m.as_str()).to_owned(),
            );
            sink = Some(Sink::Param(name));
            continue;
        }
        if let Some(captures) = RETURNS_TAG.captures(&line) {
            parsed.returns = Some(captures.get(1).map_or("", |m| m.as_str()).to_owned());
            sink = Some(Sink::Returns);
            continue;
        }
        if line.starts_with('@') || line.trim_matches(is_js_space).is_empty() {
            sink = None;
            continue;
        }
        let continuation = line.trim_matches(is_js_space);
        match &sink {
            Some(Sink::Param(name)) => {
                let current = parsed.params.entry(name.clone()).or_default();
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(continuation);
            }
            Some(Sink::Returns) => {
                let current = parsed.returns.get_or_insert_with(String::new);
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(continuation);
            }
            None => {}
        }
    }
    parsed
}

/// One declared parameter as the completeness check sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredParameter {
    /// Identifier name when the binding is a plain identifier.
    pub identifier: Option<String>,
    /// Source text of the binding, used for binding-pattern diagnostics.
    pub text: String,
}

/// Requires a non-empty tag for each non-exempt identifier parameter, rejects
/// binding-pattern parameters, and rejects stale tags.
pub fn check_params(
    where_: &str,
    api_kind: &str,
    parameters: &[DeclaredParameter],
    tags: &IndexMap<String, String>,
    is_exempt: &dyn Fn(&DeclaredParameter) -> bool,
    violations: &mut Vec<String>,
) {
    for parameter in parameters {
        let Some(name) = &parameter.identifier else {
            violations.push(format!(
                "{where_}: parameter '{}' is a binding pattern; the {api_kind} API needs simple identifier parameters so @param can name them.",
                parameter.text
            ));
            continue;
        };
        if is_exempt(parameter) {
            continue;
        }
        match tags.get(name) {
            None => violations.push(format!("{where_} is missing @param {name}.")),
            Some(description) if description.trim_matches(is_js_space).is_empty() => {
                violations.push(format!("{where_}: @param {name} has an empty description."));
            }
            Some(_) => {}
        }
    }
    for tag in tags.keys() {
        if !parameters
            .iter()
            .any(|parameter| parameter.identifier.as_deref() == Some(tag))
        {
            violations.push(format!(
                "{where_}: @param {tag} does not match any parameter (stale tag?)."
            ));
        }
    }
}

/// Checks the `@returns` half of the completeness contract.
pub fn check_returns(
    where_: &str,
    return_type: Option<&str>,
    returns: Option<&str>,
    violations: &mut Vec<String>,
) {
    let Some(return_type) = return_type else {
        violations.push(format!(
            "{where_} has no return type annotation; annotate it explicitly so the gate can classify the result."
        ));
        return;
    };
    let rendered = collapse_keeping_edges(return_type);
    if rendered == "void" || rendered == "Promise<void>" {
        return;
    }
    match returns {
        None => violations.push(format!(
            "{where_} is missing @returns (return type: {rendered})."
        )),
        Some(description) if description.trim_matches(is_js_space).is_empty() => {
            violations.push(format!("{where_}: @returns has an empty description."));
        }
        Some(_) => {}
    }
}

/// `value.replace(/\s+/g, ' ')` without trimming.
fn collapse_keeping_edges(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut in_space = false;
    for character in value.chars() {
        if is_js_space(character) {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(character);
            in_space = false;
        }
    }
    result
}

/// Throws one aggregate error for every completeness violation a walk collected.
///
/// # Errors
/// Returns the aggregated violation list, prefixed with the gate name.
pub fn report_violations(gate: &str, violations: &[String]) -> anyhow::Result<()> {
    if violations.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{gate}: {} JSDoc completeness violation(s) (see AGENTS.md):\n{}",
        violations.len(),
        violations
            .iter()
            .map(|violation| format!("  {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}
