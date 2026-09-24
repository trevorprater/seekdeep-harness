//! Pure assembly of the minimal-update briefing for one out-of-sync
//! translation pair: the authored side's changes since the last confirmed
//! state at the narrowest safely mapped granularity (code-fence-only splice,
//! changed Markdown units, heading sections, whole document), the terminology
//! rows those changes touch, first-occurrence movement notes, and a digest of
//! the binding update rules. The CLI wrapper is
//! [`crate::translation_brief_command`].

use std::collections::{BTreeMap, BTreeSet};

use markdown::mdast::Node;
use regex::Regex;

use crate::translation_pairing::parse_translation_markdown;

/// One block-level span of a Markdown document, in document order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownSpan {
    /// Position in the span list; briefing ids derive from it.
    pub index: usize,
    /// Structural kind compared for alignment, language-neutral: container
    /// path plus node type for units (`root.3:tableRow`), depth for sections
    /// (`section:2`).
    pub kind: String,
    /// Reader-facing label: heading text for sections, node type for units.
    pub label: String,
    /// 1-based first source line.
    pub start_line: usize,
    /// 1-based last source line.
    pub end_line: usize,
    /// The span's text, trailing newline normalized to exactly one.
    pub text: String,
}

fn lines_of(markdown: &str) -> Vec<String> {
    let mut lines = markdown
        .replace("\r\n", "\n")
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn slice_lines(lines: &[String], start_line: usize, end_line: usize) -> String {
    let start = start_line.saturating_sub(1).min(lines.len());
    let end = end_line.min(lines.len()).max(start);
    format!("{}\n", lines[start..end].join("\n"))
}

/// The mdast node type name of a unit node, or `None` for containers.
fn unit_type(node: &Node) -> Option<&'static str> {
    Some(match node {
        Node::Heading(_) => "heading",
        Node::Paragraph(_) => "paragraph",
        Node::Code(_) => "code",
        Node::TableRow(_) => "tableRow",
        Node::ListItem(_) => "listItem",
        Node::Blockquote(_) => "blockquote",
        Node::Html(_) => "html",
        Node::ThematicBreak(_) => "thematicBreak",
        Node::Definition(_) => "definition",
        _ => return None,
    })
}

struct RawSpan {
    kind: String,
    label: String,
    start_line: usize,
    end_line: usize,
}

fn collect_units(node: &Node, path: &str, out: &mut Vec<RawSpan>) {
    let kind = match (unit_type(node), node) {
        (Some("heading"), Node::Heading(heading)) => {
            Some(format!("{path}:heading:{}", heading.depth))
        }
        (Some(unit), _) => Some(format!("{path}:{unit}")),
        (None, _) => None,
    };
    if let (Some(kind), Some(position)) = (kind, node.position()) {
        out.push(RawSpan {
            kind,
            label: unit_type(node).unwrap_or_default().to_owned(),
            start_line: position.start.line,
            end_line: position.end.line,
        });
        return;
    }
    if let Some(children) = node.children() {
        for (index, child) in children.iter().enumerate() {
            collect_units(child, &format!("{path}.{index}"), out);
        }
    }
}

fn finish_spans(markdown: &str, mut raw: Vec<RawSpan>) -> Vec<MarkdownSpan> {
    raw.sort_by_key(|span| span.start_line);
    let lines = lines_of(markdown);
    raw.into_iter()
        .enumerate()
        .map(|(index, span)| MarkdownSpan {
            index,
            text: slice_lines(&lines, span.start_line, span.end_line),
            kind: span.kind,
            label: span.label,
            start_line: span.start_line,
            end_line: span.end_line,
        })
        .collect()
}

/// List a document's translation units: the outermost block nodes a minimal
/// update can replace independently. Headings, paragraphs, code fences, table
/// rows, list items, block quotes, HTML blocks, thematic breaks, and link
/// definitions are units; the container path is part of the kind so kind
/// sequences only align when container membership also aligns.
///
/// # Errors
/// Returns Markdown parser diagnostics.
pub fn markdown_units(markdown: &str) -> anyhow::Result<Vec<MarkdownSpan>> {
    let tree = parse_translation_markdown(markdown).map_err(anyhow::Error::msg)?;
    let mut raw = Vec::new();
    collect_units(&tree, "root", &mut raw);
    Ok(finish_spans(markdown, raw))
}

fn heading_label(node: &Node, label: &mut String) {
    match node {
        Node::Text(text) => label.push_str(&text.value),
        Node::InlineCode(code) => label.push_str(&code.value),
        Node::InlineMath(math) => label.push_str(&math.value),
        Node::Html(html) => label.push_str(&html.value),
        Node::MdxTextExpression(expression) => label.push_str(&expression.value),
        _ => {}
    }
    if let Some(children) = node.children() {
        for child in children {
            heading_label(child, label);
        }
    }
}

fn collect_headings(node: &Node, out: &mut Vec<(u8, usize, String)>) {
    if let (Node::Heading(heading), Some(position)) = (node, node.position()) {
        let mut label = String::new();
        for child in &heading.children {
            heading_label(child, &mut label);
        }
        out.push((heading.depth, position.start.line, label));
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_headings(child, out);
        }
    }
}

/// List a document's heading-delimited sections, including a leading
/// `preamble` span when content precedes the first heading.
///
/// # Errors
/// Returns Markdown parser diagnostics.
pub fn section_spans(markdown: &str) -> anyhow::Result<Vec<MarkdownSpan>> {
    let tree = parse_translation_markdown(markdown).map_err(anyhow::Error::msg)?;
    let mut headings = Vec::new();
    collect_headings(&tree, &mut headings);
    headings.sort_by_key(|(_, line, _)| *line);
    let lines = lines_of(markdown);
    let mut spans = Vec::new();
    let first_heading_line = headings
        .first()
        .map_or(lines.len() + 1, |(_, line, _)| *line);
    if first_heading_line > 1 {
        spans.push(MarkdownSpan {
            index: 0,
            kind: "preamble".to_owned(),
            label: "(preamble before the first heading)".to_owned(),
            start_line: 1,
            end_line: first_heading_line - 1,
            text: slice_lines(&lines, 1, first_heading_line - 1),
        });
    }
    for (order, (depth, line, label)) in headings.iter().enumerate() {
        let end_line = headings
            .get(order + 1)
            .map_or(lines.len() + 1, |(_, next, _)| *next)
            - 1;
        spans.push(MarkdownSpan {
            index: spans.len(),
            // Depth only: heading TEXT is translated across a pair, so it
            // cannot participate in cross-language alignment.
            kind: format!("section:{depth}"),
            label: if label.is_empty() {
                "(untitled section)".to_owned()
            } else {
                label.clone()
            },
            start_line: *line,
            end_line,
            text: slice_lines(&lines, *line, end_line),
        });
    }
    Ok(spans)
}

/// Whether two span lists map one to one: same non-zero length and the same
/// kind at every position.
#[must_use]
pub fn spans_aligned(left: &[MarkdownSpan], right: &[MarkdownSpan]) -> bool {
    !left.is_empty()
        && left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(span, other)| span.kind == other.kind)
}

/// Indices whose text differs between two aligned span lists.
#[must_use]
pub fn changed_span_indices(before: &[MarkdownSpan], after: &[MarkdownSpan]) -> Vec<usize> {
    before
        .iter()
        .enumerate()
        .filter(|(index, span)| {
            after
                .get(*index)
                .is_none_or(|other| span.text != other.text)
        })
        .map(|(_, span)| span.index)
        .collect()
}

fn code_spans_of(markdown: &str) -> anyhow::Result<Vec<MarkdownSpan>> {
    Ok(markdown_units(markdown)?
        .into_iter()
        .filter(|span| span.kind.ends_with(":code"))
        .enumerate()
        .map(|(index, span)| MarkdownSpan { index, ..span })
        .collect())
}

fn replace_span_texts(
    markdown: &str,
    spans: &[MarkdownSpan],
    replacements: &BTreeMap<usize, String>,
) -> anyhow::Result<String> {
    let mut lines = lines_of(markdown);
    for (index, replacement) in replacements.iter().rev() {
        let span = spans.get(*index).ok_or_else(|| {
            anyhow::anyhow!("translation brief: unknown replacement span {index}")
        })?;
        let start = span.start_line - 1;
        let end = span.end_line.min(lines.len());
        lines.splice(start..end, lines_of(replacement));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn mask_code_spans(markdown: &str, spans: &[MarkdownSpan]) -> anyhow::Result<String> {
    let masks = spans
        .iter()
        .map(|span| {
            (
                span.index,
                format!("SEEKDEEP_TRANSLATION_CODE_{}\n", span.index),
            )
        })
        .collect();
    replace_span_texts(markdown, spans, &masks)
}

/// Compute the counterpart update for a change confined to fenced code
/// blocks. Fences are byte-identical across a pair, so when the source's
/// prose is untouched and the counterpart's fences match the last-confirmed
/// source, splicing the edited fences into the counterpart is the complete
/// update — no translation judgment is involved.
///
/// # Errors
/// Returns Markdown parser diagnostics.
pub fn compute_mechanical_update(
    confirmed_source: &str,
    current_source: &str,
    counterpart: &str,
) -> anyhow::Result<Option<String>> {
    let confirmed = code_spans_of(confirmed_source)?;
    let current = code_spans_of(current_source)?;
    let target = code_spans_of(counterpart)?;
    if confirmed.is_empty() || confirmed.len() != current.len() || confirmed.len() != target.len() {
        return Ok(None);
    }
    if mask_code_spans(confirmed_source, &confirmed)? != mask_code_spans(current_source, &current)?
    {
        return Ok(None);
    }
    if confirmed
        .iter()
        .zip(&target)
        .any(|(span, other)| span.text != other.text)
    {
        return Ok(None);
    }
    let changed = current
        .iter()
        .zip(&confirmed)
        .filter(|(span, other)| span.text != other.text)
        .map(|(span, _)| (span.index, span.text.clone()))
        .collect::<BTreeMap<_, _>>();
    if changed.is_empty() {
        return Ok(None);
    }
    replace_span_texts(counterpart, &target, &changed).map(Some)
}

/// One parsed terminology-table data row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminologyRow {
    /// English term.
    pub english: String,
    /// Chinese rendering.
    pub chinese: String,
    /// The 首次出现 cell (first-occurrence rendering), possibly empty.
    pub first: String,
    /// The verbatim table row.
    pub line: String,
}

/// Strip Markdown emphasis and code markers from a terminology cell.
fn plain_term(cell: &str) -> String {
    cell.replace('`', "").replace("**", "").trim().to_owned()
}

/// Parse the data rows of the terminology table.
#[must_use]
pub fn parse_terminology_rows(terminology: &str) -> Vec<TerminologyRow> {
    static SEPARATOR: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^\|[\s:|-]+\|$").expect("valid separator pattern")
    });
    let separator = &*SEPARATOR;
    let mut rows = Vec::new();
    for line in terminology.split('\n') {
        if !line.starts_with('|') || separator.is_match(line) {
            continue;
        }
        let cells = line.split('|').map(str::trim).collect::<Vec<_>>();
        let english = plain_term(cells.get(1).copied().unwrap_or_default());
        if english.is_empty() || english == "English" {
            continue;
        }
        rows.push(TerminologyRow {
            english,
            chinese: plain_term(cells.get(2).copied().unwrap_or_default()),
            first: plain_term(cells.get(3).copied().unwrap_or_default()),
            line: line.to_owned(),
        });
    }
    rows
}

fn is_word_like(term: &str) -> bool {
    let mut characters = term.chars();
    let (Some(first), Some(last)) = (characters.next(), term.chars().next_back()) else {
        return false;
    };
    term.chars().count() >= 2
        && first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && term.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, ' ' | '.' | '_' | '-')
        })
}

fn is_word_char(character: Option<char>) -> bool {
    character.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Byte offsets of a term's occurrences (see [`term_offsets`] for the
/// UTF-16 form the source reports).
fn term_byte_offsets(text: &str, term: &str, english_inflections: bool) -> Vec<usize> {
    if term.is_empty() {
        return Vec::new();
    }
    let word_like = is_word_like(term);
    let inflected = if english_inflections && word_like {
        let stem_y = term.len() >= 2
            && term.ends_with(['y', 'Y'])
            && !term[..term.len() - 1]
                .chars()
                .next_back()
                .is_some_and(|character| "aeiouAEIOU".contains(character));
        if stem_y {
            format!("{}(?:y|ies)", regex::escape(&term[..term.len() - 1]))
        } else {
            format!("{}(?:s|es)?", regex::escape(term))
        }
    } else {
        regex::escape(term)
    };
    let expression = Regex::new(&format!("(?i){inflected}")).expect("escaped term pattern");
    let mut offsets = Vec::new();
    let mut position = 0;
    while position <= text.len() {
        let Some(found) = expression.find_at(text, position) else {
            break;
        };
        let before = text[..found.start()].chars().next_back();
        let after = text[found.end()..].chars().next();
        let bounded = !word_like || (!is_word_char(before) && !is_word_char(after));
        if bounded {
            offsets.push(found.start());
            position = found.end().max(found.start() + 1);
        } else {
            position = found.start()
                + text[found.start()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
        }
    }
    offsets
}

fn utf16_offset(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].encode_utf16().count()
}

/// Character offsets (UTF-16 units, as the source reports) of a term's
/// occurrences. English word-like terms match on word boundaries and accept
/// plural inflections (`agents`, `registries`); other terms match as
/// case-insensitive substrings.
#[must_use]
pub fn term_offsets(text: &str, term: &str, english_inflections: bool) -> Vec<usize> {
    term_byte_offsets(text, term, english_inflections)
        .into_iter()
        .map(|offset| utf16_offset(text, offset))
        .collect()
}

/// The two update directions a pair supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BriefDirection {
    /// The English side changed; bring the Chinese counterpart along.
    EnToZh,
    /// The Chinese side changed; bring the English counterpart along.
    ZhToEn,
}

impl BriefDirection {
    /// The source spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnToZh => "en-to-zh",
            Self::ZhToEn => "zh-to-en",
        }
    }

    const fn source_language(self) -> &'static str {
        match self {
            Self::EnToZh => "English",
            Self::ZhToEn => "Chinese",
        }
    }

    const fn counterpart_language(self) -> &'static str {
        match self {
            Self::EnToZh => "Chinese",
            Self::ZhToEn => "English",
        }
    }
}

fn has_chinese(term: &str) -> bool {
    term.chars()
        .any(|character| ('\u{4E00}'..='\u{9FFF}').contains(&character))
}

/// Whether a row's source-language term occurs in the given text.
fn row_occurs(row: &TerminologyRow, direction: BriefDirection, text: &str) -> bool {
    match direction {
        BriefDirection::EnToZh => !term_byte_offsets(text, &row.english, true).is_empty(),
        BriefDirection::ZhToEn => [&row.first, &row.chinese]
            .into_iter()
            .filter(|term| has_chinese(term))
            .any(|term| !term_byte_offsets(text, term, false).is_empty()),
    }
}

/// Select the terminology rows whose source-language term occurs in the
/// changed text (old and new states combined).
#[must_use]
pub fn relevant_terminology_rows(
    terminology: &str,
    direction: BriefDirection,
    changed_text: &str,
) -> Vec<TerminologyRow> {
    parse_terminology_rows(terminology)
        .into_iter()
        .filter(|row| row_occurs(row, direction, changed_text))
        .collect()
}

fn line_at_offset(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].matches('\n').count() + 1
}

fn span_index_at_offset(
    text: &str,
    spans: &[MarkdownSpan],
    byte_offset: Option<usize>,
) -> Option<usize> {
    let line = line_at_offset(text, byte_offset?);
    spans
        .iter()
        .find(|span| line >= span.start_line && line <= span.end_line)
        .map(|span| span.index)
}

/// First-occurrence guidance computed for a Chinese-target update.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirstOccurrenceContext {
    /// Human-readable notes for the briefing.
    pub notes: Vec<String>,
    /// Unchanged span indices that must join the briefing because a first
    /// occurrence moved into or out of them.
    pub extra_span_indices: Vec<usize>,
}

/// Track document-wide first occurrences of the relevant English terms. The
/// 首次出现 rendering attaches to a term's first occurrence, so when an edit
/// moves that occurrence across spans, both the old and new spans need
/// counterpart edits even when only one of them changed.
#[must_use]
pub fn first_occurrence_context(
    confirmed_source: &str,
    current_source: &str,
    confirmed_spans: &[MarkdownSpan],
    current_spans: &[MarkdownSpan],
    rows: &[TerminologyRow],
    changed: &BTreeSet<usize>,
) -> FirstOccurrenceContext {
    let mut notes = Vec::new();
    let mut extra = BTreeSet::new();
    for row in rows {
        if row.first.is_empty() {
            continue;
        }
        let old_index = span_index_at_offset(
            confirmed_source,
            confirmed_spans,
            term_byte_offsets(confirmed_source, &row.english, true)
                .first()
                .copied(),
        );
        let new_index = span_index_at_offset(
            current_source,
            current_spans,
            term_byte_offsets(current_source, &row.english, true)
                .first()
                .copied(),
        );
        if old_index == new_index {
            continue;
        }
        for index in [old_index, new_index].into_iter().flatten() {
            if !changed.contains(&index) {
                extra.insert(index);
            }
        }
        let describe = |index: Option<usize>| {
            index.map_or_else(|| "absent".to_owned(), |index| format!("#{index}"))
        };
        notes.push(format!(
            "{}: the document-wide first occurrence moved from {} to {}; the {} form moves with it (later occurrences drop the annotation).",
            row.english,
            describe(old_index),
            describe(new_index),
            row.first
        ));
    }
    FirstOccurrenceContext {
        notes,
        extra_span_indices: extra.into_iter().collect(),
    }
}

/// Smallest fence of `mark` characters that safely wraps `body`.
fn fence_for(body: &str, mark: char) -> String {
    let mut longest = 2;
    for line in body.split('\n') {
        let run = line
            .trim_start_matches(crate::jsdoc::is_js_space)
            .chars()
            .take_while(|character| *character == mark)
            .count();
        if run >= 3 && run > longest {
            longest = run;
        }
    }
    mark.to_string().repeat(longest + 1)
}

/// Why a bundle is present when its source text did not change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleReason {
    /// A first occurrence moved into or out of the span.
    FirstOccurrence,
}

/// One changed (or first-occurrence) span with its three-way context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefBundle {
    /// Span index shared by the aligned documents.
    pub index: usize,
    /// Human label: heading text or node type.
    pub label: String,
    /// Why the bundle is present when its source text did not change.
    pub reason: Option<BundleReason>,
    /// The span's last-confirmed source text.
    pub confirmed_source_text: String,
    /// The span's current source text.
    pub current_source_text: String,
    /// The counterpart span's current text.
    pub counterpart_text: String,
    /// 1-based line the counterpart span starts on.
    pub counterpart_start_line: usize,
}

/// The granularities a briefing can map the change at, narrowest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BriefScope {
    /// Every change is inside fenced code blocks.
    Mechanical,
    /// Fine-grained units align across the pair.
    Units {
        /// Changed and first-occurrence spans.
        bundles: Vec<BriefBundle>,
        /// First-occurrence movement notes.
        first_occurrence_notes: Vec<String>,
    },
    /// Only heading sections align across the pair.
    Sections {
        /// Changed and first-occurrence spans.
        bundles: Vec<BriefBundle>,
        /// First-occurrence movement notes.
        first_occurrence_notes: Vec<String>,
    },
    /// Nothing aligns; the whole document needs reconciling.
    Document {
        /// Why no narrower mapping was safe.
        reason: String,
    },
}

/// Inputs for rendering one pair's briefing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationBriefInput {
    /// Repo-relative path of the side that changed.
    pub source_path: String,
    /// Repo-relative path of the counterpart to update.
    pub counterpart_path: String,
    /// Update direction.
    pub direction: BriefDirection,
    /// Unified diff of the changed side, last-confirmed to current.
    pub diff: String,
    /// The mapped scope.
    pub scope: BriefScope,
    /// Terminology rows matching the change.
    pub terminology: Vec<TerminologyRow>,
}

const ZH_TARGET_DIGEST: &[&str] = &[
    "- Edit ONLY what the change requires; preserve the reviewed phrasing of everything unchanged.",
    "- Nothing added, nothing dropped: the Chinese must state exactly what the new English states.",
    "- Write natural institutional technical Chinese, not word-by-word gloss; terse stays terse.",
    "- Code fences byte-identical to the English side, comments included; inline code spans verbatim.",
    "- Relative links keep the `.md` target; only the switcher line links `.zh.md`.",
    "- Structure mirrors the counterpart: heading depths and order, list kinds and item counts, table rows and columns.",
    "- 首次出现 annotations attach to the document-wide first occurrence only; later occurrences use the bare form, and an empty 首次出现 cell means never gloss.",
    "- Typography: one half-width space between Chinese and Latin or digits; full-width punctuation in Chinese prose; 顿号 for enumerations; second person is 你.",
    "- One physical line per paragraph; exactly one trailing newline.",
];

const EN_TARGET_DIGEST: &[&str] = &[
    "- Edit ONLY what the change requires; preserve the reviewed phrasing of everything unchanged.",
    "- Nothing added, nothing dropped: the English must state exactly what the new Chinese states.",
    "- Write concise professional developer prose, not word-by-word gloss; terse stays terse.",
    "- Code fences byte-identical to the Chinese side, comments included; inline code spans verbatim.",
    "- Relative links keep the `.md` target; only the switcher line links `.zh.md`.",
    "- Structure mirrors the counterpart: heading depths and order, list kinds and item counts, table rows and columns.",
    "- One physical line per paragraph; exactly one trailing newline.",
];

fn render_bundles(
    out: &mut Vec<String>,
    input: &TranslationBriefInput,
    bundles: &[BriefBundle],
    first_occurrence_notes: &[String],
) {
    let source_language = input.direction.source_language();
    let counterpart_language = input.direction.counterpart_language();
    for bundle in bundles {
        out.push(String::new());
        out.push(format!(
            "### #{} {}{} — counterpart at {}:{}",
            bundle.index,
            bundle.label,
            if bundle.reason == Some(BundleReason::FirstOccurrence) {
                " — unchanged; included for a first-occurrence move"
            } else {
                ""
            },
            input.counterpart_path,
            bundle.counterpart_start_line
        ));
        let fence = fence_for(
            &[
                bundle.confirmed_source_text.as_str(),
                bundle.current_source_text.as_str(),
                bundle.counterpart_text.as_str(),
            ]
            .join("\n"),
            '~',
        );
        if bundle.confirmed_source_text != bundle.current_source_text {
            out.push(String::new());
            out.push(format!("Last-confirmed {source_language}:"));
            out.push(String::new());
            out.push(format!("{fence}markdown"));
            out.push(
                bundle
                    .confirmed_source_text
                    .trim_end_matches(crate::jsdoc::is_js_space)
                    .to_owned(),
            );
            out.push(fence.clone());
        }
        out.push(String::new());
        out.push(format!("Current {source_language}:"));
        out.push(String::new());
        out.push(format!("{fence}markdown"));
        out.push(
            bundle
                .current_source_text
                .trim_end_matches(crate::jsdoc::is_js_space)
                .to_owned(),
        );
        out.push(fence.clone());
        out.push(String::new());
        out.push(format!(
            "Current {counterpart_language} (bring this along):"
        ));
        out.push(String::new());
        out.push(format!("{fence}markdown"));
        out.push(
            bundle
                .counterpart_text
                .trim_end_matches(crate::jsdoc::is_js_space)
                .to_owned(),
        );
        out.push(fence);
    }
    if !first_occurrence_notes.is_empty() {
        out.push(String::new());
        out.push("## First-occurrence notes".to_owned());
        out.push(String::new());
        for note in first_occurrence_notes {
            out.push(format!("- {note}"));
        }
    }
}

fn anchor_path(source_path: &str) -> String {
    source_path
        .strip_suffix(".zh.md")
        .map_or_else(|| source_path.to_owned(), |stem| format!("{stem}.md"))
}

/// Render the complete briefing for one out-of-sync pair.
#[must_use]
pub fn render_translation_brief(input: &TranslationBriefInput) -> String {
    let source_language = input.direction.source_language();
    let counterpart_language = input.direction.counterpart_language();
    let mut out: Vec<String> = Vec::new();
    out.push(format!(
        "# Translation update briefing: {}",
        input.source_path
    ));
    out.push(String::new());
    out.push(format!(
        "The {source_language} side changed; bring `{}` along with the smallest edit that covers the change.",
        input.counterpart_path
    ));
    if input.scope == BriefScope::Mechanical {
        out.push(String::new());
        out.push("## Mechanical update — no translation judgment involved".to_owned());
        out.push(String::new());
        out.push(format!(
            "Every change since the last confirmed state is inside fenced code blocks, which are byte-identical across the pair. Run `pnpm run gen-translation-brief --apply {}` to splice the updated fences into the counterpart (the result is structure-validated before writing), then record per the Finish steps.",
            input.source_path
        ));
    }
    out.push(String::new());
    out.push(format!(
        "## {source_language} diff (last-confirmed → current)"
    ));
    out.push(String::new());
    let diff_fence = fence_for(&input.diff, '`');
    out.push(format!("{diff_fence}diff"));
    out.push(
        input
            .diff
            .trim_end_matches(crate::jsdoc::is_js_space)
            .to_owned(),
    );
    out.push(diff_fence);
    match &input.scope {
        BriefScope::Mechanical => {}
        BriefScope::Units {
            bundles,
            first_occurrence_notes,
        } => {
            out.push(String::new());
            out.push(format!(
                "## Changed units (last-confirmed {source_language} → current {source_language}, with the current {counterpart_language})"
            ));
            render_bundles(&mut out, input, bundles, first_occurrence_notes);
        }
        BriefScope::Sections {
            bundles,
            first_occurrence_notes,
        } => {
            out.push(String::new());
            out.push("## Changed sections (fine-grained units do not align across the pair; whole heading sections shown)".to_owned());
            render_bundles(&mut out, input, bundles, first_occurrence_notes);
        }
        BriefScope::Document { reason } => {
            out.push(String::new());
            out.push("## Whole-document update required".to_owned());
            out.push(String::new());
            out.push(format!(
                "{reason} Open `{}` directly, locate the affected regions yourself, and reconcile under docs/i18n/translation-rules.md.",
                input.counterpart_path
            ));
        }
    }
    if !input.terminology.is_empty() {
        out.push(String::new());
        out.push(
            "## Binding terminology rows matching this change (docs/i18n/terminology.md)"
                .to_owned(),
        );
        out.push(String::new());
        out.push("| English | 中文 | 首次出现 | 不要译作 | 备注 |".to_owned());
        out.push("|---|---|---|---|---|".to_owned());
        for row in &input.terminology {
            out.push(row.line.clone());
        }
        out.push(String::new());
        out.push("For any term you introduce that is not listed above, consult the full table before inventing a rendering.".to_owned());
    }
    out.push(String::new());
    out.push("## Rules digest (full rules: docs/i18n/translation-rules.md)".to_owned());
    out.push(String::new());
    let digest = match input.direction {
        BriefDirection::EnToZh => ZH_TARGET_DIGEST,
        BriefDirection::ZhToEn => EN_TARGET_DIGEST,
    };
    out.extend(digest.iter().map(|line| (*line).to_owned()));
    out.push(String::new());
    out.push("## Finish".to_owned());
    out.push(String::new());
    out.push("1. Apply the smallest counterpart edit that covers the change, then verify the changed spans clause by clause against the source.".to_owned());
    let anchor = anchor_path(&input.source_path);
    out.push(format!(
        "2. `pnpm run verify-translation-pairing --write {anchor}`"
    ));
    out.push(format!("3. `pnpm run verify-translation-pairing {anchor}`"));
    out.push(String::new());
    out.join("\n")
}
