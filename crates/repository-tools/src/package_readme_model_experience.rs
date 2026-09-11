//! Package README Model Experience grammar and audited classification policy.

use std::{collections::HashSet, fmt::Write as _, path::Path, sync::LazyLock};

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::markdown_util::{MarkdownProseLine, markdown_heading_lines, markdown_prose_lines};

const HEADING: &str = "## Model Experience";
const LIMITATIONS: &str = "## Known Limitations and Deferred Work";
const MODEL_VIEW: &str = "#### What the model sees";
const TOKEN_EFFECT: &str = "#### Token effect";
const KV_CACHE: &str = "#### KV Cache effect";
const FIELDS: [&str; 3] = [MODEL_VIEW, TOKEN_EFFECT, KV_CACHE];

/// Audited shape for a package with no direct model-context contribution.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SentenceKind {
    /// No model-visible effects.
    None,
    /// Effects rendered by another package.
    Indirect,
}

/// Rationale for using the short README contract.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SentenceContract {
    /// Required sentence prefix.
    pub kind: SentenceKind,
    /// Audited package-specific justification.
    pub reason: String,
}

/// Reviewed package classifications, preserving source declaration order.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelExperiencePolicy {
    /// Packages required to omit every Model Experience heading.
    pub omissions: IndexMap<String, String>,
    /// Packages using a sentence and one KV-cache field.
    pub sentences: IndexMap<String, SentenceContract>,
}

impl Default for ModelExperiencePolicy {
    fn default() -> Self {
        serde_json::from_str(include_str!("package_readme_model_experience_policy.json"))
            .expect("embedded Model Experience policy")
    }
}

/// One package-attributed policy violation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelExperienceFailure {
    /// Repository-relative README path.
    pub path: String,
    /// Source-compatible diagnostic, including line when applicable.
    pub message: String,
}

/// Counts of successfully validated sections and ordered violations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelExperienceReport {
    /// Package manifests inspected.
    pub checked: usize,
    /// Audited absent sections.
    pub omitted_sections: usize,
    /// Full structured sections.
    pub structured: usize,
    /// Accepted H3 model-context entries.
    pub model_context_entries: usize,
    /// Accepted KV-cache fields.
    pub kv_cache_effects: usize,
    /// Direct system prompt entries with exact text fences.
    pub system_prompt_entries: usize,
    /// Tool-schema entries pointing to catalog H2s.
    pub tool_schema_entries: usize,
    /// Accepted no-effect short sections.
    pub explained_none: usize,
    /// Accepted indirect-effect short sections.
    pub indirect: usize,
    /// Accepted nested Markdown literal blocks.
    pub verbatim_blocks: usize,
    /// Ordered policy failures.
    pub failures: Vec<ModelExperienceFailure>,
}

/// Inspects the package tree with the pinned audited classifications.
///
/// # Errors
///
/// Returns filesystem, catalog, or Markdown parser failures.
pub fn inspect_package_readme_model_experience(
    root: &Path,
) -> anyhow::Result<ModelExperienceReport> {
    inspect_with_policy(root, &ModelExperiencePolicy::default())
}

/// Inspects the package tree with an explicit reviewed classification policy.
///
/// # Errors
///
/// Returns filesystem, catalog, or Markdown parser failures.
pub fn inspect_with_policy(
    root: &Path,
    policy: &ModelExperiencePolicy,
) -> anyhow::Result<ModelExperienceReport> {
    let catalog = std::fs::read_to_string(root.join("docs/tool-catalog.md"))?;
    let catalog_fragments = catalog
        .split('\n')
        .filter_map(|line| line.strip_prefix("## "))
        .filter(|title| !title.is_empty())
        .map(heading_fragment)
        .collect::<HashSet<_>>();
    let packages = package_directories(root)?;
    let scanned = packages.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut report = ModelExperienceReport {
        checked: packages.len(),
        ..ModelExperienceReport::default()
    };
    for (package, reason) in &policy.omissions {
        if !scanned.contains(package.as_str()) {
            fail(
                &mut report,
                package,
                "no-section allowlist entry does not name a scanned package",
            );
        }
        if reason.trim().is_empty() {
            fail(
                &mut report,
                package,
                "no-section allowlist entry must retain its audit justification",
            );
        }
        if policy.sentences.contains_key(package) {
            fail(
                &mut report,
                package,
                "package cannot appear in both Model Experience allowlists",
            );
        }
    }
    for (package, contract) in &policy.sentences {
        if !scanned.contains(package.as_str()) {
            fail(
                &mut report,
                package,
                "sentence allowlist entry does not name a scanned package",
            );
        }
        if contract.reason.trim().is_empty() {
            fail(
                &mut report,
                package,
                "sentence allowlist entry must justify why structured model-context entries are unnecessary",
            );
        }
    }
    for package in packages {
        let path = root.join(&package).join("README.md");
        if !path.exists() {
            fail(&mut report, &package, "missing package README");
            continue;
        }
        let source = std::fs::read_to_string(path)?;
        inspect_readme(&source, &package, policy, &catalog_fragments, &mut report)?;
    }
    Ok(report)
}

/// Renders the source checker output without changing diagnostic ordering.
#[must_use]
pub fn render_report(report: &ModelExperienceReport) -> String {
    if report.failures.is_empty() {
        return format!(
            "verify-package-readme-model-experience: {} README(s) checked ({} audited omissions, {} structured, {} model-context entries, {} KV-cache fields, {} fenced system-prompt entries, {} catalog-linked tool-schema entries, {} explained none, {} indirect, {} verbatim markdown blocks), all conform.\n",
            report.checked,
            report.omitted_sections,
            report.structured,
            report.model_context_entries,
            report.kv_cache_effects,
            report.system_prompt_entries,
            report.tool_schema_entries,
            report.explained_none,
            report.indirect,
            report.verbatim_blocks
        );
    }
    let mut output = "verify-package-readme-model-experience failed:\n".to_owned();
    for failure in &report.failures {
        let _ = writeln!(output, "  {}: {}", failure.path, failure.message);
    }
    output
}

#[allow(
    clippy::too_many_lines,
    reason = "Heading precedence follows the source checker and returns its first package-local violation"
)]
fn inspect_readme(
    source: &str,
    package: &str,
    policy: &ModelExperiencePolicy,
    catalog: &HashSet<String>,
    report: &mut ModelExperienceReport,
) -> anyhow::Result<()> {
    let raw = source.split('\n').collect::<Vec<_>>();
    let lines = markdown_prose_lines(source).map_err(anyhow::Error::msg)?;
    let headings = markdown_heading_lines(source).map_err(anyhow::Error::msg)?;
    let h2 = headings
        .iter()
        .filter(|heading| heading.depth == 2)
        .collect::<Vec<_>>();
    let model = headings
        .iter()
        .filter(|heading| {
            heading
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
                == "model experience"
        })
        .collect::<Vec<_>>();
    if policy.omissions.contains_key(package) {
        if model.is_empty() {
            report.omitted_sections += 1;
        }
        for heading in model {
            fail(
                report,
                package,
                format!(
                    "line {}: audited model-agnostic package must omit every Model Experience heading; found {}",
                    heading.index,
                    quote(&heading.raw)
                ),
            );
        }
        return Ok(());
    }
    if let Some(heading) = model
        .iter()
        .find(|heading| heading.depth != 2 || heading.raw != HEADING)
    {
        fail(
            report,
            package,
            format!(
                "line {}: non-canonical Model Experience heading {}; use exactly {}",
                heading.index,
                quote(&heading.raw),
                quote(HEADING)
            ),
        );
        return Ok(());
    }
    let Some(heading) = model.first() else {
        fail(report, package, format!("missing {HEADING}"));
        return Ok(());
    };
    if model.len() != 1 {
        fail(
            report,
            package,
            format!("contains {} copies of {HEADING}", model.len()),
        );
        return Ok(());
    }
    let model_h2 = h2
        .iter()
        .position(|entry| entry.index == heading.index)
        .expect("canonical H2 exists");
    if let Some(limitations) = h2.iter().position(|heading| heading.raw == LIMITATIONS) {
        if model_h2 + 2 != h2.len() || limitations + 1 != h2.len() {
            fail(
                report,
                package,
                format!(
                    "{HEADING} and {LIMITATIONS} must be the final two H2 sections, in that order"
                ),
            );
            return Ok(());
        }
    } else if model_h2 + 1 != h2.len() {
        fail(
            report,
            package,
            format!("{HEADING} must be the final H2 when {LIMITATIONS} is absent"),
        );
        return Ok(());
    }
    let heading_at = lines.iter().position(|line| line.index == heading.index);
    let body = &lines[heading_at.map_or(0, |index| index + 1)..];
    let end = body
        .iter()
        .position(|line| h2.iter().any(|heading| heading.index == line.index))
        .unwrap_or(body.len());
    let next_h2_line = body.get(end).map_or(raw.len() + 1, |line| line.index);
    let content = body[..end]
        .iter()
        .filter(|line| !line.raw.trim().is_empty())
        .collect::<Vec<_>>();
    if let Some(contract) = policy.sentences.get(package) {
        inspect_sentence(
            &raw[heading.index..next_h2_line - 1],
            &content,
            heading.index,
            package,
            contract.kind,
            report,
        );
        return Ok(());
    }
    if let Some(line) = content.iter().find(|line| {
        line.raw == "None."
            || line.raw.starts_with("None, as ")
            || line.raw.starts_with("Indirectly, through ")
    }) {
        fail(
            report,
            package,
            format!(
                "line {}: short Model Experience form requires an audited entry in SENTENCE_MODEL_EXPERIENCE",
                line.index
            ),
        );
        return Ok(());
    }
    let starts = content
        .iter()
        .enumerate()
        .filter_map(|(index, line)| titled_heading(&line.raw, "### ").then_some(index))
        .collect::<Vec<_>>();
    if starts.first() != Some(&0) {
        fail(
            report,
            package,
            "must contain one or more complete model-context entries",
        );
        return Ok(());
    }
    let mut entries = Vec::new();
    let mut fragments = HashSet::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(content.len());
        let entry_lines = &content[start..end];
        let entry_heading = entry_lines[0];
        let title = &entry_heading.raw["### ".len()..];
        let fragment = heading_fragment(title);
        if fragment.is_empty() {
            fail(
                report,
                package,
                format!(
                    "line {}: each model-context entry requires a non-empty H3 heading",
                    entry_heading.index
                ),
            );
            return Ok(());
        }
        if fragments.contains(&fragment) {
            fail(
                report,
                package,
                format!(
                    "line {}: duplicate model-context entry link fragment {}",
                    entry_heading.index,
                    quote(&fragment)
                ),
            );
            return Ok(());
        }
        let field_starts = entry_lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| titled_heading(&line.raw, "#### ").then_some(index))
            .collect::<Vec<_>>();
        if field_starts.len() != FIELDS.len() || field_starts.first() != Some(&1) {
            fail(
                report,
                package,
                format!(
                    "line {}: model-context entry requires exactly three ordered H4 fields: {}",
                    entry_heading.index,
                    FIELDS.join(", ")
                ),
            );
            return Ok(());
        }
        if (index == 0 && entry_heading.index != heading.index + 2)
            || !blank_before(&raw, entry_heading.index)
            || entry_lines[field_starts[0]].index != entry_heading.index + 2
        {
            fail(
                report,
                package,
                format!(
                    "line {}: model-context entry heading and first field require one blank line between them",
                    entry_heading.index
                ),
            );
            return Ok(());
        }
        let next_entry = content.get(end).map_or(next_h2_line, |line| line.index);
        let Some(fields) = inspect_fields(
            entry_lines,
            &field_starts,
            next_entry,
            &raw,
            package,
            report,
        ) else {
            return Ok(());
        };
        if fields
            .iter()
            .any(|field| local_link().is_match(&field.value.raw))
        {
            fail(
                report,
                package,
                format!(
                    "line {}: Model Experience fields must not link between local subsections; nest the H5 in its owning H4 field",
                    entry_heading.index
                ),
            );
            return Ok(());
        }
        fragments.insert(fragment);
        entries.push(Entry {
            heading: entry_heading,
            title,
            fields,
        });
    }
    if let Some(entry) = entries
        .iter()
        .find(|entry| system_prompt().is_match(entry.title) && entry.fields[0].blocks == 0)
    {
        fail(
            report,
            package,
            format!(
                "line {}: system-prompt entry must contain a titled H5 plus verbatim `markdown` block under {MODEL_VIEW}",
                entry.heading.index
            ),
        );
        return Ok(());
    }
    if !entries.iter().any(|entry| {
        entry.fields.iter().any(|field| field.blocks > 0)
            || entry.fields[0].value.raw.contains('`')
            || entry.fields[1].value.raw.contains('`')
            || !tool_catalog_fragments(&entry.fields[0].value.raw).is_empty()
    }) {
        fail(
            report,
            package,
            "structured Model Experience must ground at least one entry with inline code, a nested `markdown` block, or an anchored tool-catalog link",
        );
        return Ok(());
    }
    for entry in &entries {
        if !schema().is_match(entry.title) {
            continue;
        }
        let fragments = tool_catalog_fragments(&entry.fields[0].value.raw);
        if fragments.is_empty() {
            fail(
                report,
                package,
                format!(
                    "line {}: tool-schema entry must link an anchored section of ../../../docs/tool-catalog.md",
                    entry.heading.index
                ),
            );
            return Ok(());
        }
        if let Some(invalid) = fragments
            .iter()
            .find(|fragment| !catalog.contains(**fragment))
        {
            fail(
                report,
                package,
                format!(
                    "line {}: tool-catalog link fragment {} does not name an H2 section",
                    entry.fields[0].value.index,
                    quote(invalid)
                ),
            );
            return Ok(());
        }
    }
    report.verbatim_blocks += entries
        .iter()
        .flat_map(|entry| &entry.fields)
        .map(|field| field.blocks)
        .sum::<usize>();
    report.model_context_entries += entries.len();
    report.system_prompt_entries += entries
        .iter()
        .filter(|entry| system_prompt().is_match(entry.title))
        .count();
    report.tool_schema_entries += entries
        .iter()
        .filter(|entry| schema().is_match(entry.title))
        .count();
    report.kv_cache_effects += entries.len();
    report.structured += 1;
    Ok(())
}

fn inspect_sentence(
    raw: &[&str],
    content: &[&MarkdownProseLine],
    heading: usize,
    package: &str,
    kind: SentenceKind,
    report: &mut ModelExperienceReport,
) {
    let prefix = match kind {
        SentenceKind::None => "None, as ",
        SentenceKind::Indirect => "Indirectly, through ",
    };
    let valid_sentence = content.first().is_some_and(|line| {
        line.raw.starts_with(prefix) && line.raw.ends_with('.') && line.raw.len() > prefix.len() + 1
    });
    if content.len() != 3
        || raw.iter().filter(|line| !line.trim().is_empty()).count() != 3
        || !valid_sentence
    {
        fail(
            report,
            package,
            format!(
                "must contain exactly one sentence beginning {} and ending with a period, followed by {KV_CACHE} and one non-empty paragraph",
                quote(prefix)
            ),
        );
        return;
    }
    let sentence = content[0];
    let kv_heading = content[1];
    let effect = content[2];
    if kv_heading.raw != KV_CACHE
        || any_heading().is_match(&effect.raw)
        || effect.raw.trim().is_empty()
    {
        fail(
            report,
            package,
            format!(
                "line {}: short Model Experience form requires exact {KV_CACHE} and one non-empty paragraph",
                kv_heading.index
            ),
        );
        return;
    }
    if sentence.index != heading + 2
        || kv_heading.index != sentence.index + 2
        || effect.index != kv_heading.index + 2
    {
        fail(
            report,
            package,
            "short Model Experience sentence, KV-cache H4, and paragraph require one blank line between each element",
        );
        return;
    }
    match kind {
        SentenceKind::None => report.explained_none += 1,
        SentenceKind::Indirect => report.indirect += 1,
    }
    report.kv_cache_effects += 1;
}

struct Field<'a> {
    value: &'a MarkdownProseLine,
    blocks: usize,
}
struct Entry<'a> {
    heading: &'a MarkdownProseLine,
    title: &'a str,
    fields: Vec<Field<'a>>,
}

fn inspect_fields<'a>(
    entries: &[&'a MarkdownProseLine],
    starts: &[usize],
    next_entry: usize,
    raw: &[&str],
    package: &str,
    report: &mut ModelExperienceReport,
) -> Option<Vec<Field<'a>>> {
    let mut fields = Vec::new();
    let mut fragments = HashSet::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let heading = entries[start];
        let expected = FIELDS[index];
        if heading.raw != expected {
            fail(
                report,
                package,
                format!(
                    "line {}: expected exact field heading {}, found {}",
                    heading.index,
                    quote(expected),
                    quote(&heading.raw)
                ),
            );
            return None;
        }
        let end = starts.get(index + 1).copied().unwrap_or(entries.len());
        let field = &entries[start..end];
        let Some(value) = field
            .get(1)
            .filter(|line| !any_heading().is_match(&line.raw) && !line.raw.trim().is_empty())
            .copied()
        else {
            fail(
                report,
                package,
                format!(
                    "line {}: {expected} requires one non-empty paragraph",
                    heading.index
                ),
            );
            return None;
        };
        if value.index != heading.index + 2 {
            fail(
                report,
                package,
                format!(
                    "line {}: {expected} and its paragraph require one blank line between them",
                    heading.index
                ),
            );
            return None;
        }
        if let Some(line) = field[2..]
            .iter()
            .find(|line| !titled_heading(&line.raw, "##### "))
        {
            fail(
                report,
                package,
                format!(
                    "line {}: content after {expected} paragraph must be a titled H5 plus `markdown` fence owned by that field",
                    line.index
                ),
            );
            return None;
        }
        let next_heading = entries.get(end).map_or(next_entry, |line| line.index);
        if !blank_before(raw, next_heading) {
            fail(
                report,
                package,
                format!(
                    "line {next_heading}: Model Experience headings require a preceding blank line"
                ),
            );
            return None;
        }
        let blocks = match nested_verbatim(&raw[value.index..next_heading - 1], &mut fragments) {
            Ok(blocks) => blocks,
            Err(error) => {
                fail(report, package, format!("line {}: {error}", value.index));
                return None;
            }
        };
        if field.len() - 2 != blocks {
            fail(
                report,
                package,
                format!(
                    "line {}: every nested H5 must own exactly one `markdown` fence",
                    value.index
                ),
            );
            return None;
        }
        fields.push(Field { value, blocks });
    }
    Some(fields)
}

fn nested_verbatim(raw: &[&str], fragments: &mut HashSet<String>) -> Result<usize, String> {
    let mut cursor = 0;
    let mut blocks = 0;
    loop {
        while raw.get(cursor).is_some_and(|line| line.trim().is_empty()) {
            cursor += 1;
        }
        let Some(line) = raw.get(cursor) else {
            return Ok(blocks);
        };
        if !titled_heading(line, "##### ") {
            return Err(
                "content after a field paragraph must be a titled H5 verbatim block".to_owned(),
            );
        }
        let title = &line["##### ".len()..];
        let fragment = heading_fragment(title);
        if fragment.is_empty() {
            return Err("verbatim H5 title must be non-empty".to_owned());
        }
        if !fragments.insert(fragment) {
            return Err(format!(
                "verbatim H5 title {} is duplicated within its model-context entry",
                quote(title)
            ));
        }
        cursor += 1;
        while raw.get(cursor).is_some_and(|line| line.trim().is_empty()) {
            cursor += 1;
        }
        if raw.get(cursor) != Some(&"```markdown") {
            return Err("each nested verbatim H5 requires an exact ```markdown fence".to_owned());
        }
        cursor += 1;
        let start = cursor;
        while raw.get(cursor).is_some_and(|line| *line != "```") {
            cursor += 1;
        }
        if cursor == raw.len() {
            return Err("unterminated nested ```markdown fence".to_owned());
        }
        if cursor == start {
            return Err("nested ```markdown fence must not be empty".to_owned());
        }
        cursor += 1;
        blocks += 1;
    }
}

fn fail(report: &mut ModelExperienceReport, package: &str, message: impl Into<String>) {
    report.failures.push(ModelExperienceFailure {
        path: format!("{package}/README.md"),
        message: message.into(),
    });
}

fn blank_before(raw: &[&str], line: usize) -> bool {
    line.checked_sub(2)
        .and_then(|index| raw.get(index))
        .is_some_and(|line| line.trim().is_empty())
}
fn titled_heading(raw: &str, prefix: &str) -> bool {
    raw.strip_prefix(prefix)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|character| !character.is_whitespace())
}
fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("string JSON")
}
fn heading_fragment(title: &str) -> String {
    let filtered = title
        .to_lowercase()
        .chars()
        .filter(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, ' ' | '_' | '-')
        })
        .collect::<String>();
    filtered.split_whitespace().collect::<Vec<_>>().join("-")
}
fn tool_catalog_fragments(text: &str) -> Vec<&str> {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\]\(\.\./\.\./\.\./docs/tool-catalog\.md#([a-z0-9_-]+)\)")
            .expect("catalog link regex")
    });
    PATTERN
        .captures_iter(text)
        .filter_map(|capture| capture.get(1).map(|group| group.as_str()))
        .collect()
}
fn local_link() -> &'static Regex {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\]\(#[^)]+\)").expect("local link regex"));
    &PATTERN
}
fn any_heading() -> &'static Regex {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^#{1,6} ").expect("heading regex"));
    &PATTERN
}
fn system_prompt() -> &'static Regex {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i-u)\bsystem prompt\b").expect("system prompt regex"));
    &PATTERN
}
fn schema() -> &'static Regex {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i-u)\bschemas?\b").expect("schema regex"));
    &PATTERN
}

fn package_directories(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut packages = Vec::new();
    let directory = root.join("packages");
    if !directory.exists() {
        return Ok(packages);
    }
    for group in std::fs::read_dir(directory)? {
        let group = group?;
        if !group.path().is_dir() || group.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            if package.file_name().to_string_lossy().starts_with('.')
                || !package.path().join("package.json").exists()
            {
                continue;
            }
            packages.push(format!(
                "packages/{}/{}",
                group.file_name().to_string_lossy(),
                package.file_name().to_string_lossy()
            ));
        }
    }
    packages.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(packages)
}
