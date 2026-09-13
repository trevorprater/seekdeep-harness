//! Relative Markdown target and fragment verification.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use percent_encoding::percent_decode_str;
use regex::Regex;

use crate::{
    markdown_util::{markdown_heading_lines, parse_markdown, visit_markdown},
    repo_files::{archived_agent_note_path, unique_repo_files},
};

/// Repository-authored Markdown checked for relative links.
pub const MARKDOWN_LINK_PATTERNS: &[&str] = &[
    "README.md",
    "README.zh.md",
    ".agents/notes/**/*.md",
    "docs/**/*.md",
    "packages/*/*.md",
    "packages/*/*/*.md",
    "examples/**/*.md",
    "AGENTS.md",
    "packages/AGENTS.md",
    ".agents/skills/**/*.md",
];

/// The part of one relative Markdown link that failed to resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownLinkViolationReason {
    /// The decoded target path does not exist.
    Target,
    /// The Markdown target exposes no matching fragment.
    Anchor,
}

/// One broken relative Markdown link, image, or definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownLinkViolation {
    /// Repository-relative source file.
    pub file: String,
    /// 1-based source line where the node starts.
    pub line: usize,
    /// Authored URL exactly as parsed from Markdown.
    pub url: String,
    /// Failed target component.
    pub reason: MarkdownLinkViolationReason,
}

/// Full repository Markdown-link inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkdownLinkReport {
    /// Canonically unique files checked.
    pub checked: usize,
    /// Broken links in file and document order.
    pub violations: Vec<MarkdownLinkViolation>,
}

/// Lazily parsed Markdown target-anchor cache.
#[derive(Debug, Default)]
pub struct MarkdownAnchorCache {
    anchors: HashMap<PathBuf, HashSet<String>>,
}

impl MarkdownAnchorCache {
    /// Creates an empty anchor cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn anchors_of(&mut self, path: &Path) -> anyhow::Result<&HashSet<String>> {
        if !self.anchors.contains_key(path) {
            let source = std::fs::read_to_string(path)?;
            let anchors = document_anchors(&source).map_err(anyhow::Error::msg)?;
            self.anchors.insert(path.to_owned(), anchors);
        }
        Ok(self
            .anchors
            .get(path)
            .expect("the requested anchor set was inserted"))
    }
}

/// Inspects the full repository Markdown-link scope.
///
/// Archived Agent Notes remain valid targets, but their historical outbound
/// links are excluded from the evolving check.
///
/// # Errors
///
/// Returns repository discovery, file, path, or Markdown parser failures.
pub fn inspect_markdown_links(root: &Path) -> anyhow::Result<MarkdownLinkReport> {
    let files = unique_repo_files(root, MARKDOWN_LINK_PATTERNS, archived_agent_note_path)?;
    let mut cache = MarkdownAnchorCache::new();
    let mut violations = Vec::new();
    for file in &files {
        violations.extend(find_markdown_link_violations(
            root,
            &file.absolute,
            &mut cache,
        )?);
    }
    Ok(MarkdownLinkReport {
        checked: files.len(),
        violations,
    })
}

/// Applies GitHub's Markdown heading-slug transformation.
#[must_use]
pub fn github_slug(heading: &str) -> String {
    static REJECTED: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"[^\p{L}\p{N}_ -]").expect("static GitHub slug regex")
    });
    REJECTED
        .replace_all(&heading.to_lowercase(), "")
        .replace(' ', "-")
}

/// Returns every GitHub heading slug and explicit live `<a id="…">` anchor.
///
/// # Errors
///
/// Returns the Markdown parser diagnostic.
pub fn document_anchors(source: &str) -> Result<HashSet<String>, String> {
    static HTML_COMMENTS: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?s)<!--[\s\S]*?-->").expect("static HTML comment regex")
    });
    static EXPLICIT_ANCHOR: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"<a id="([^"]+)""#).expect("static explicit anchor regex")
    });

    let mut anchors = HashSet::new();
    let mut occurrences = HashMap::<String, usize>::new();
    for heading in markdown_heading_lines(source)? {
        let base = github_slug(&heading.text);
        let mut result = base.clone();
        let mut bump = occurrences.get(&base).copied().unwrap_or_default();
        while anchors.contains(&result) {
            bump += 1;
            result = format!("{base}-{bump}");
        }
        occurrences.insert(base, bump);
        anchors.insert(result);
    }

    let tree = parse_markdown(source)?;
    visit_markdown(&tree, &mut |node| {
        if let markdown::mdast::Node::Html(html) = node {
            let visible = HTML_COMMENTS.replace_all(&html.value, "");
            for captures in EXPLICIT_ANCHOR.captures_iter(&visible) {
                anchors.insert(captures[1].to_owned());
            }
        }
        true
    });
    Ok(anchors)
}

/// Finds every broken relative link in one Markdown source file.
///
/// # Errors
///
/// Returns path, file, target-anchor, or Markdown parser failures.
pub fn find_markdown_link_violations(
    root: &Path,
    absolute_path: &Path,
    anchors: &mut MarkdownAnchorCache,
) -> anyhow::Result<Vec<MarkdownLinkViolation>> {
    let file = slash_path(absolute_path.strip_prefix(root)?);
    let source = std::fs::read_to_string(absolute_path)?;
    let tree = parse_markdown(&source).map_err(anyhow::Error::msg)?;
    let mut candidates = Vec::<(String, usize)>::new();
    visit_markdown(&tree, &mut |node| {
        let candidate = match node {
            markdown::mdast::Node::Link(link) => Some((&link.url, link.position.as_ref())),
            markdown::mdast::Node::Image(image) => Some((&image.url, image.position.as_ref())),
            markdown::mdast::Node::Definition(definition) => {
                Some((&definition.url, definition.position.as_ref()))
            }
            _ => None,
        };
        if let Some((url, position)) = candidate {
            candidates.push((
                url.clone(),
                position.map_or(0, |position| position.start.line),
            ));
        }
        true
    });

    let directory = absolute_path.parent().unwrap_or(root);
    let mut violations = Vec::new();
    for (url, line) in candidates {
        if is_external(&url) {
            continue;
        }
        let target = path_part(&url);
        let resolved = if target.is_empty() {
            absolute_path.to_owned()
        } else {
            directory.join(target)
        };
        if !resolved.exists() {
            violations.push(MarkdownLinkViolation {
                file: file.clone(),
                line,
                url,
                reason: MarkdownLinkViolationReason::Target,
            });
            continue;
        }
        let Some(fragment) = fragment_part(&url) else {
            continue;
        };
        if !resolved.to_string_lossy().ends_with(".md") {
            continue;
        }
        if !anchors.anchors_of(&resolved)?.contains(&fragment) {
            violations.push(MarkdownLinkViolation {
                file: file.clone(),
                line,
                url,
                reason: MarkdownLinkViolationReason::Anchor,
            });
        }
    }
    Ok(violations)
}

/// Renders the source-compatible success or violation report.
#[must_use]
pub fn render_markdown_link_report(report: &MarkdownLinkReport) -> String {
    if report.violations.is_empty() {
        return format!(
            "verify-md-links: {} file(s) checked, all relative cross-links and fragments resolve.\n",
            report.checked
        );
    }
    let mut output = "verify-md-links: broken relative cross-links found:\n".to_owned();
    for violation in &report.violations {
        writeln!(
            output,
            "  {}:{}  {}  ({})",
            violation.file,
            violation.line,
            violation.url,
            match violation.reason {
                MarkdownLinkViolationReason::Target => "target does not exist",
                MarkdownLinkViolationReason::Anchor => "no such anchor in target",
            }
        )
        .expect("writing to a String cannot fail");
    }
    output
}

fn is_external(url: &str) -> bool {
    static SCHEME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.-]*:").expect("static URL scheme regex")
    });
    url.starts_with("//") || url.starts_with('/') || SCHEME.is_match(url)
}

fn path_part(url: &str) -> String {
    let end = url.find(['#', '?']).unwrap_or(url.len());
    decode_uri_component_or_raw(&url[..end])
}

fn fragment_part(url: &str) -> Option<String> {
    let hash = url.find('#')?;
    let raw = &url[hash + 1..];
    let end = raw.find('?').unwrap_or(raw.len());
    Some(decode_uri_component_or_raw(&raw[..end]))
}

fn decode_uri_component_or_raw(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return value.to_owned();
        }
        index += 3;
    }
    percent_decode_str(value)
        .decode_utf8()
        .map_or_else(|_| value.to_owned(), std::borrow::Cow::into_owned)
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
