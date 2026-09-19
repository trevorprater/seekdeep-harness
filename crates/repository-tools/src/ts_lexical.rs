//! Lexical TypeScript scanning shared by the Cordis and Slot catalog
//! generators: parsing without a type-checker program, workspace globbing,
//! package ownership, and the compiler's trivia conventions (`getStart`,
//! `getFullStart`, line numbers) expressed over `oxc` spans.

use std::{cmp::Ordering, collections::HashMap, path::Path, sync::OnceLock};

use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences, options::CollatorOptions};
use icu_locale::Locale;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Class, Declaration, ExportDefaultDeclarationKind, Program, PropertyKey, Statement,
    TSEnumDeclaration, TSInterfaceDeclaration, TSModuleDeclaration, TSTypeAliasDeclaration,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};

use crate::jsdoc::{is_js_space, leading_comment_ranges};

/// Parses one TypeScript or TSX source the way `ts.createSourceFile` does:
/// never failing, keeping whatever tree the recoverable parse produced.
#[must_use]
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the compiler's script-kind rule is case-sensitive"
)]
pub fn parse<'a>(allocator: &'a Allocator, text: &'a str, rel: &str) -> Program<'a> {
    let source_type = if rel.ends_with(".tsx") {
        SourceType::tsx()
    } else {
        SourceType::ts()
    };
    Parser::new(allocator, text, source_type).parse().program
}

/// Repo-relative, `/`-normalized, sorted, de-duplicated matches of `patterns`
/// under `scan_root`, with dot-files excluded as Node's `globSync` does.
///
/// # Errors
/// Returns an invalid glob pattern or an unreadable directory.
pub fn glob_relative(scan_root: &Path, patterns: &[&str]) -> anyhow::Result<Vec<String>> {
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
    };
    let mut relative = Vec::new();
    for pattern in patterns {
        let absolute = scan_root.join(pattern);
        let absolute = absolute.to_str().ok_or_else(|| {
            anyhow::anyhow!(
                "glob pattern root is not valid UTF-8: {}",
                scan_root.display()
            )
        })?;
        for entry in glob::glob_with(absolute, options)? {
            let path = entry?;
            let rel = path.strip_prefix(scan_root)?;
            relative.push(
                rel.components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    relative.sort();
    relative.dedup();
    Ok(relative)
}

/// The workspace package name owning a repo-relative file, memoized per
/// directory the way the source generators cache it.
#[must_use]
pub fn package_name_of<S: std::hash::BuildHasher>(
    scan_root: &Path,
    rel: &str,
    cache: &mut HashMap<String, String, S>,
) -> String {
    let mut directory = scan_root.join(rel);
    directory.pop();
    let root_length = scan_root.as_os_str().len();
    while directory.as_os_str().len() > root_length {
        let key = directory.to_string_lossy().into_owned();
        if let Some(cached) = cache.get(&key) {
            return cached.clone();
        }
        if let Some(name) = std::fs::read_to_string(directory.join("package.json"))
            .ok()
            .and_then(|manifest| serde_json::from_str::<serde_json::Value>(&manifest).ok())
            .and_then(|manifest| manifest["name"].as_str().map(str::to_owned))
        {
            cache.insert(key, name.clone());
            return name;
        }
        if !directory.pop() {
            break;
        }
    }
    "(unknown package)".to_owned()
}

/// One-based line of an offset, counting the compiler's line breaks.
#[must_use]
pub fn line_of(text: &str, offset: usize) -> usize {
    let mut line = 1;
    let mut previous_cr = false;
    for character in text[..offset.min(text.len())].chars() {
        match character {
            '\r' => {
                line += 1;
                previous_cr = true;
                continue;
            }
            '\n' if previous_cr => {}
            '\n' | '\u{2028}' | '\u{2029}' => line += 1,
            _ => {}
        }
        previous_cr = false;
    }
    line
}

/// The compiler's `getFullStart` for a node whose predecessor ended at
/// `previous_end`: `oxc` spans omit the separator tokens a member may end
/// with, so the scan resumes after them.
#[must_use]
pub fn full_start_after(text: &str, previous_end: usize) -> usize {
    let bytes = text.as_bytes();
    let mut index = previous_end.min(bytes.len());
    while index < bytes.len() && matches!(bytes[index], b';' | b',') {
        index += 1;
    }
    index
}

/// `getStart(sourceFile, includeJsDoc = true)`: the first `/**` leading comment
/// of a node, or the node's own start when it carries none.
#[must_use]
pub fn start_with_jsdoc(text: &str, full_start: usize, start: usize) -> usize {
    leading_comment_ranges(text, full_start)
        .into_iter()
        .find(|range| {
            text.get(range.pos..range.pos + 3) == Some("/**")
                && text.get(range.pos..range.pos + 4) != Some("/**/")
        })
        .map_or(start, |range| range.pos)
}

/// The source text a span covers.
#[must_use]
pub fn text_of(text: &str, span: Span) -> &str {
    &text[span.start as usize..span.end as usize]
}

/// A property name's text with quotes removed for identifiers and string
/// literals, otherwise the source spelling (a computed name keeps its brackets).
#[must_use]
pub fn member_name(key: &PropertyKey<'_>, computed: bool, text: &str) -> String {
    if computed {
        return key_source_text(key, true, text).to_owned();
    }
    match key {
        PropertyKey::StaticIdentifier(identifier) => identifier.name.to_string(),
        PropertyKey::StringLiteral(literal) => literal.value.to_string(),
        other => text_of(text, other.span()).to_owned(),
    }
}

/// `name.getText()`: the property name as written, brackets included for a
/// computed name (whose `oxc` span covers only the inner expression).
#[must_use]
pub fn key_source_text<'a>(key: &PropertyKey<'_>, computed: bool, text: &'a str) -> &'a str {
    let span = key.span();
    if !computed {
        return text_of(text, span);
    }
    let bytes = text.as_bytes();
    let mut start = span.start as usize;
    while start > 0 && bytes[start - 1] != b'[' {
        start -= 1;
    }
    let mut end = span.end as usize;
    while end < bytes.len() && bytes[end] != b']' {
        end += 1;
    }
    &text[start.saturating_sub(1)..(end + 1).min(bytes.len())]
}

/// Strip the shared leading indentation of a multi-line source slice.
#[must_use]
pub fn dedent(text: &str) -> String {
    let lines = text.split('\n').collect::<Vec<_>>();
    let shared = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim_matches(is_js_space).is_empty())
        .map(|line| {
            line.chars()
                .take_while(|character| is_js_space(*character))
                .count()
        })
        .min()
        .unwrap_or(0);
    let mut output = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            output.push((*line).to_owned());
        } else {
            output.push(line.chars().skip(shared).collect());
        }
    }
    output.join("\n").trim_end_matches(is_js_space).to_owned()
}

/// `value.replace(/\s+/g, ' ').trim()`.
#[must_use]
pub fn collapse(value: &str) -> String {
    crate::jsdoc::collapse(value)
}

/// `left.localeCompare(right)` under the process locale.
///
/// # Panics
/// Panics only if the compiled collation data cannot build the fallback
/// `en-US` collator, which the pinned ICU data always provides.
#[must_use]
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| {
            let locale = sys_locale::get_locale()
                .and_then(|locale| locale.parse::<Locale>().ok())
                .unwrap_or_else(|| "en-US".parse().expect("valid fallback locale"));
            Collator::try_new(
                CollatorPreferences::from(&locale),
                CollatorOptions::default(),
            )
            .expect("compiled locale collation data")
        })
        .compare(left, right)
}

/// A top-level named declaration, reached through any `export` wrapper.
#[derive(Clone, Copy)]
pub enum NamedDeclaration<'b, 'a> {
    /// `interface X {}`.
    Interface(&'b TSInterfaceDeclaration<'a>),
    /// `type X = …`.
    TypeAlias(&'b TSTypeAliasDeclaration<'a>),
    /// `class X {}`.
    Class(&'b Class<'a>),
    /// `enum X {}`.
    Enum(&'b TSEnumDeclaration<'a>),
    /// `declare module 'x' {}` or `namespace X {}`.
    Module(&'b TSModuleDeclaration<'a>),
}

impl<'a> NamedDeclaration<'_, 'a> {
    /// The declared name as written (a module's string name keeps its quotes).
    #[must_use]
    pub fn name(&self, text: &'a str) -> String {
        match self {
            Self::Interface(declaration) => declaration.id.name.to_string(),
            Self::TypeAlias(declaration) => declaration.id.name.to_string(),
            Self::Class(declaration) => declaration
                .id
                .as_ref()
                .map(|id| id.name.to_string())
                .unwrap_or_default(),
            Self::Enum(declaration) => declaration.id.name.to_string(),
            Self::Module(declaration) => text_of(text, declaration.id.span()).to_owned(),
        }
    }
}

/// One statement's named declaration plus whether an `export` modifier wraps it.
#[must_use]
pub fn named_declaration<'b, 'a>(
    statement: &'b Statement<'a>,
) -> Option<(NamedDeclaration<'b, 'a>, bool)> {
    match statement {
        Statement::ExportNamedDeclaration(export) => export
            .declaration
            .as_ref()
            .and_then(from_declaration)
            .map(|named| (named, true)),
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                Some((NamedDeclaration::Class(class), true))
            }
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(interface) => {
                Some((NamedDeclaration::Interface(interface), true))
            }
            _ => None,
        },
        Statement::ClassDeclaration(class) => Some((NamedDeclaration::Class(class), false)),
        Statement::TSInterfaceDeclaration(interface) => {
            Some((NamedDeclaration::Interface(interface), false))
        }
        Statement::TSTypeAliasDeclaration(alias) => {
            Some((NamedDeclaration::TypeAlias(alias), false))
        }
        Statement::TSEnumDeclaration(declaration) => {
            Some((NamedDeclaration::Enum(declaration), false))
        }
        Statement::TSModuleDeclaration(module) => Some((NamedDeclaration::Module(module), false)),
        _ => None,
    }
}

fn from_declaration<'b, 'a>(declaration: &'b Declaration<'a>) -> Option<NamedDeclaration<'b, 'a>> {
    match declaration {
        Declaration::ClassDeclaration(class) => Some(NamedDeclaration::Class(class)),
        Declaration::TSInterfaceDeclaration(interface) => {
            Some(NamedDeclaration::Interface(interface))
        }
        Declaration::TSTypeAliasDeclaration(alias) => Some(NamedDeclaration::TypeAlias(alias)),
        Declaration::TSEnumDeclaration(declaration) => Some(NamedDeclaration::Enum(declaration)),
        Declaration::TSModuleDeclaration(module) => Some(NamedDeclaration::Module(module)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbers_follow_compiler_line_breaks() {
        let text = "a\r\nb\rc\nd\u{2028}e";
        assert_eq!(line_of(text, 0), 1);
        assert_eq!(line_of(text, 3), 2);
        assert_eq!(line_of(text, 5), 3);
        assert_eq!(line_of(text, 7), 4);
        assert_eq!(line_of(text, text.len()), 5);
    }

    #[test]
    fn dedent_keeps_the_first_line_and_strips_shared_indentation() {
        assert_eq!(dedent("a\n    b\n      c\n"), "a\nb\n  c");
        assert_eq!(dedent("only"), "only");
    }

    #[test]
    fn jsdoc_start_takes_the_first_documentation_comment_after_a_line_break() {
        let text = "x; // trailing\n/** one */\n/** two */\nlet y";
        let start = text.find("let").unwrap();
        assert_eq!(
            start_with_jsdoc(text, 2, start),
            text.find("/** one").unwrap()
        );
        assert_eq!(start_with_jsdoc("/**/\nlet y", 0, 5), 5);
    }
}
