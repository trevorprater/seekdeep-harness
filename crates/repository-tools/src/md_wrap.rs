//! One-physical-line Markdown paragraph verification.

use std::{fmt::Write as _, path::Path};

use crate::{
    markdown_util::{parse_markdown, visit_markdown},
    repo_files::{archived_agent_note_path, unique_repo_files},
};

/// Documentation globs checked by the wrap policy.
pub const MARKDOWN_WRAP_PATTERNS: &[&str] = &[
    "README.md",
    "README.zh.md",
    ".agents/notes/**/*.md",
    "docs/**/*.md",
    "packages/*/*.md",
    "packages/*/*/*.md",
    "examples/**/system-prompt.expected.md",
    "packages/**/system-prompt.expected.md",
    "AGENTS.md",
    "packages/AGENTS.md",
];

/// One hard-wrapped prose paragraph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownWrapViolation {
    /// Repository-relative file.
    pub file: String,
    /// 1-based paragraph start line.
    pub line: usize,
    /// Trimmed authored first line.
    pub text: String,
}

/// Full wrap-policy inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkdownWrapReport {
    /// Canonically unique files checked.
    pub checked: usize,
    /// Located hard wraps.
    pub violations: Vec<MarkdownWrapViolation>,
}

/// Inspects the repository documentation scope.
///
/// # Errors
///
/// Returns repository discovery, file, or Markdown parser failures.
pub fn inspect_markdown_wrap(root: &Path) -> anyhow::Result<MarkdownWrapReport> {
    let files = unique_repo_files(root, MARKDOWN_WRAP_PATTERNS, archived_agent_note_path)?;
    let mut violations = Vec::new();
    for file in &files {
        let relative = file.absolute.strip_prefix(root)?.to_path_buf();
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        violations.extend(
            find_markdown_wraps(&relative, &std::fs::read_to_string(&file.absolute)?)
                .map_err(anyhow::Error::msg)?,
        );
    }
    Ok(MarkdownWrapReport {
        checked: files.len(),
        violations,
    })
}

/// Finds hard-wrapped paragraphs in one named Markdown source.
///
/// # Errors
///
/// Returns the Markdown parser diagnostic.
pub fn find_markdown_wraps(file: &str, source: &str) -> Result<Vec<MarkdownWrapViolation>, String> {
    let parsed_source = mask_vitepress_structure(source);
    let tree = parse_markdown(&parsed_source)?;
    let raw_lines = source.split('\n').collect::<Vec<_>>();
    let mut violations = Vec::new();
    visit_markdown(&tree, &mut |node| {
        if let markdown::mdast::Node::Paragraph(paragraph) = node
            && let Some(position) = &paragraph.position
        {
            if position.end.line > position.start.line {
                violations.push(MarkdownWrapViolation {
                    file: file.to_owned(),
                    line: position.start.line,
                    text: raw_lines
                        .get(position.start.line - 1)
                        .copied()
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                });
            }
            return false;
        }
        true
    });
    Ok(violations)
}

/// Renders the source-compatible success or violation report.
#[must_use]
pub fn render_markdown_wrap_report(report: &MarkdownWrapReport) -> String {
    if report.violations.is_empty() {
        return format!(
            "verify-md-wrap: {} file(s) checked, no hard-wrapped prose paragraphs.\n",
            report.checked
        );
    }
    let mut output = "verify-md-wrap: hard-wrapped prose paragraphs found (write one physical line per paragraph):\n".to_owned();
    for violation in &report.violations {
        let (text, truncated) = first_utf16_units(&violation.text, 80);
        writeln!(
            output,
            "  {}:{}  {}{}",
            violation.file,
            violation.line,
            text,
            if truncated { "…" } else { "" }
        )
        .expect("writing to a String cannot fail");
    }
    output
}

fn mask_vitepress_structure(source: &str) -> String {
    let mut lines = source.split('\n').collect::<Vec<_>>();
    if lines.first().copied() == Some("---")
        && let Some(closing) = lines.iter().skip(1).position(|line| *line == "---")
    {
        for line in &mut lines[..=closing + 1] {
            *line = "";
        }
    }
    lines
        .into_iter()
        .map(|line| {
            if line.trim_start().starts_with(":::") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_utf16_units(value: &str, limit: usize) -> (String, bool) {
    let units = value.encode_utf16().collect::<Vec<_>>();
    if units.len() <= limit {
        return (value.to_owned(), false);
    }
    (String::from_utf16_lossy(&units[..limit]), true)
}
