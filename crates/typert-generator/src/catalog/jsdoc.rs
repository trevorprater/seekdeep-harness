//! Source `JSDoc` paragraphs, lists, and ordered block-tag contracts.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

#[derive(Default)]
pub(super) struct Parsed {
    pub(super) doc: String,
    pub(super) params: IndexMap<String, String>,
    pub(super) returns: Option<String>,
    pub(super) throws: Vec<String>,
    pub(super) deprecated: bool,
}

enum Sink {
    Parameter(String),
    Returns,
    Throws(usize),
}

static PARAMETER: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"^@param\s+(\[?[\w$]+\]?)\s*(?:[-—–]\s*)?(.*)$"));
static RETURNS: LazyLock<Regex> = LazyLock::new(|| pattern(r"^@returns?(?:\s+[-—–]?\s*(.*))?$"));
static THROWS: LazyLock<Regex> = LazyLock::new(|| pattern(r"^@throws?(?:\s+[-—–]?\s*(.*))?$"));
static DEPRECATED: LazyLock<Regex> = LazyLock::new(|| pattern(r"^@deprecated(?:\s|$)"));
static LINK: LazyLock<Regex> = LazyLock::new(|| pattern(r"\{@link\s+([^}]+)\}"));

fn pattern(source: &str) -> Regex {
    let source = source
        .replace(r"\s", r"[\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]")
        .replace(r"\w", "A-Za-z0-9_")
        .replace("(.*)", r"([^\r\n\u{2028}\u{2029}]*)");
    Regex::new(&source).expect("static JSDoc expression")
}

pub(super) fn parse(raw: &str) -> Parsed {
    let raw = raw.strip_prefix("/**").unwrap_or(raw);
    let raw = raw.strip_suffix("*/").unwrap_or(raw);
    let lines = raw.split('\n').map(decomment).collect::<Vec<_>>();
    let mut parsed = Parsed {
        doc: prose(&lines),
        ..Parsed::default()
    };
    let mut sink = None;
    for line in lines {
        if DEPRECATED.is_match(line) {
            parsed.deprecated = true;
            sink = None;
        } else if let Some(captures) = PARAMETER.captures(line) {
            let name = captures.get(1).map_or("", |value| value.as_str());
            let name = name.strip_prefix('[').unwrap_or(name);
            let name = name.strip_suffix(']').unwrap_or(name).to_owned();
            let value = captures
                .get(2)
                .map_or("", |value| value.as_str())
                .to_owned();
            parsed.params.insert(name.clone(), value);
            sink = Some(Sink::Parameter(name));
        } else if let Some(captures) = RETURNS.captures(line) {
            parsed.returns = Some(
                captures
                    .get(1)
                    .map_or("", |value| value.as_str())
                    .to_owned(),
            );
            sink = Some(Sink::Returns);
        } else if let Some(captures) = THROWS.captures(line) {
            parsed.throws.push(
                captures
                    .get(1)
                    .map_or("", |value| value.as_str())
                    .to_owned(),
            );
            sink = Some(Sink::Throws(parsed.throws.len() - 1));
        } else if line.starts_with('@') || trim(line).is_empty() {
            sink = None;
        } else if let Some(sink) = &sink {
            let value = match sink {
                Sink::Parameter(name) => parsed.params.get_mut(name).expect("active parameter tag"),
                Sink::Returns => parsed.returns.as_mut().expect("active returns tag"),
                Sink::Throws(index) => &mut parsed.throws[*index],
            };
            if !value.is_empty() {
                value.push(' ');
            }
            value.push_str(trim(line));
        }
    }
    parsed
}

fn decomment(line: &str) -> &str {
    let line = line.trim_start_matches(is_whitespace);
    let line = if let Some(line) = line.strip_prefix('*') {
        if let Some(first) = line
            .chars()
            .next()
            .filter(|character| is_whitespace(*character))
        {
            &line[first.len_utf8()..]
        } else {
            line
        }
    } else {
        line
    };
    line.trim_end_matches(is_whitespace)
}

fn prose(lines: &[&str]) -> String {
    let mut blocks = Prose::default();
    for line in lines {
        if line.trim_start_matches(is_whitespace).starts_with('@') {
            break;
        }
        if trim(line).is_empty() {
            blocks.flush();
        } else if line
            .strip_prefix('-')
            .and_then(|tail| tail.chars().next())
            .is_some_and(is_whitespace)
        {
            blocks.flush_item();
            if !blocks.paragraph.is_empty() {
                blocks.blocks.push(join(&blocks.paragraph));
                blocks.paragraph.clear();
            }
            blocks.item.push((*line).to_owned());
        } else if blocks.item.is_empty() {
            blocks.paragraph.push((*line).to_owned());
        } else {
            blocks.item.push((*line).to_owned());
        }
    }
    blocks.flush();
    trim(&LINK.replace_all(&blocks.blocks.join("\n\n"), "$1")).to_owned()
}

#[derive(Default)]
struct Prose {
    blocks: Vec<String>,
    paragraph: Vec<String>,
    list: Vec<String>,
    item: Vec<String>,
}

impl Prose {
    fn flush_item(&mut self) {
        if !self.item.is_empty() {
            self.list.push(join(&self.item));
            self.item.clear();
        }
    }

    fn flush(&mut self) {
        self.flush_item();
        if !self.list.is_empty() {
            self.blocks.push(self.list.join("\n"));
            self.list.clear();
        }
        if !self.paragraph.is_empty() {
            self.blocks.push(join(&self.paragraph));
            self.paragraph.clear();
        }
    }
}

fn join(parts: &[String]) -> String {
    parts
        .iter()
        .flat_map(|part| part.split(is_whitespace))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn trim(value: &str) -> &str {
    value.trim_matches(is_whitespace)
}

pub(super) const fn is_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
