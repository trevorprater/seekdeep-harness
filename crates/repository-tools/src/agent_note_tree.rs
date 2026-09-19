//! Agent Note tree discovery plus lifecycle-specific structure and format gates.

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use regex::Regex;

/// Closed active Agent Note lifecycle folders.
pub const AGENT_NOTE_LIFECYCLES: &[&str] = &["proposed", "implemented", "rejected"];
/// Closed Agent Note classification folders.
pub const AGENT_NOTE_CLASSES: &[&str] = &[
    "feature",
    "bug-fix",
    "simplification",
    "architecture",
    "process",
    "testing",
];

const AGENT_NOTE_ARCHIVE: &str = "archived";
const ROOT_ALLOWLIST: &[&str] = &["AGENTS.md", "CLAUDE.md"];
const FORMAT_ADOPTED: &str = "2026-07-05";
const GRANDFATHER: &str =
    "<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->";
const LEGACY_MARKERS: &[&str] = &[
    "XXX: legacy ADR/RFC body format",
    "XXX: legacy ADR/Agent Note body format",
];

static DATED_NOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}-.+\.md$").expect("valid regex"));
static BANNED_IMPLEMENTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^## (?:Proposal\b|Plan\b|Migration plan\b|Acceptance criteria\b)")
        .expect("valid regex")
});
static COMPILED_REPOSITORY_ROOT: LazyLock<PathBuf> =
    LazyLock::new(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));

/// One valid English Agent Note discovered in the active tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentNote {
    /// Lifecycle directory containing the note.
    pub lifecycle: String,
    /// Slash-normalized path relative to `.agents/notes`.
    pub relative_path: String,
    /// `yyyy-mm-dd` prefix from the filename.
    pub date: String,
}

/// Tree walk result, including every structural violation in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentNoteTree {
    /// Valid active English notes.
    pub notes: Vec<AgentNote>,
    /// Structural violations.
    pub errors: Vec<String>,
}

/// Gate outcome rendered by the two root commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentNoteCheck {
    /// Number of valid notes inspected.
    pub checked: usize,
    /// Ordered violations; empty means success.
    pub errors: Vec<String>,
}

/// Returns the checkout root containing this compiled repository tool.
#[must_use]
pub fn compiled_repository_root() -> &'static Path {
    &COMPILED_REPOSITORY_ROOT
}

/// Whether a repository path is frozen Agent Note history.
#[must_use]
pub fn is_archived_agent_note_path(path: &str) -> bool {
    path.replace('\\', "/")
        .starts_with(".agents/notes/archived/")
}

/// Walks the active Agent Note tree and enforces lifecycle/class/path rules.
///
/// # Errors
///
/// Returns directory traversal, metadata, or relative-path failures.
pub fn walk_agent_note_tree(repository_root: &Path) -> anyhow::Result<AgentNoteTree> {
    let note_root = repository_root.join(".agents/notes");
    let mut result = AgentNoteTree::default();
    for entry in std::fs::read_dir(&note_root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "INDEX.md" {
            result.errors.push(
                "structure: INDEX.md — centralized Agent Note indexes are forbidden; browse the lifecycle/class tree or search the repository".into(),
            );
            continue;
        }
        if entry.file_type()?.is_dir()
            && name != AGENT_NOTE_ARCHIVE
            && !AGENT_NOTE_LIFECYCLES.contains(&name.as_str())
        {
            result.errors.push(format!(
                "structure: {name}/ — unknown lifecycle folder (allowed: {}, plus {AGENT_NOTE_ARCHIVE}/)",
                AGENT_NOTE_LIFECYCLES.join(", ")
            ));
        }
    }

    for lifecycle in AGENT_NOTE_LIFECYCLES {
        let lifecycle_root = note_root.join(lifecycle);
        let mut matches = Vec::new();
        if lifecycle_root.exists() {
            let entries = walkdir::WalkDir::new(&lifecycle_root)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || !entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| name.starts_with('.'))
                });
            for entry in entries {
                let entry = entry?;
                if entry.depth() == 0
                    || entry.path().extension().and_then(std::ffi::OsStr::to_str) != Some("md")
                {
                    continue;
                }
                matches.push(slash_path(entry.path().strip_prefix(&note_root)?));
            }
        }
        matches.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        for relative in matches {
            let segments = relative.split('/').collect::<Vec<_>>();
            if segments.len() == 2
                && segments
                    .get(1)
                    .is_some_and(|name| ROOT_ALLOWLIST.contains(name))
            {
                continue;
            }
            if relative.ends_with(".zh.md") {
                continue;
            }
            let class = segments.get(1).copied();
            let basename = segments.get(2).copied();
            let (Some(class), Some(basename)) = (class, basename) else {
                result.errors.push(format!(
                    "structure: {relative} — expected {{lifecycle}}/{{class}}/file.md (got depth {})",
                    segments.len()
                ));
                continue;
            };
            if segments.len() != 3 {
                result.errors.push(format!(
                    "structure: {relative} — expected {{lifecycle}}/{{class}}/file.md (got depth {})",
                    segments.len()
                ));
                continue;
            }
            if !AGENT_NOTE_CLASSES.contains(&class) {
                let class = serde_json::to_string(class)?;
                result.errors.push(format!(
                    "structure: {relative} — unknown class folder {class} (allowed: {})",
                    AGENT_NOTE_CLASSES.join(", ")
                ));
                continue;
            }
            if !DATED_NOTE.is_match(basename) {
                result.errors.push(format!(
                    "structure: {relative} — filename must be yyyy-mm-dd-topic.md"
                ));
                continue;
            }
            let date = basename[..10].to_owned();
            result.notes.push(AgentNote {
                lifecycle: (*lifecycle).to_owned(),
                relative_path: relative,
                date,
            });
        }
    }
    Ok(result)
}

/// Checks the Agent Note tree and forbidden legacy homes.
///
/// # Errors
///
/// Returns tree traversal failures.
pub fn verify_agent_note_classification(repository_root: &Path) -> anyhow::Result<AgentNoteCheck> {
    let mut tree = walk_agent_note_tree(repository_root)?;
    for legacy_root in ["docs/rfc", "docs/rfcs"] {
        if repository_root.join(legacy_root).exists() {
            tree.errors.push(format!(
                "legacy-path: {legacy_root}/ is forbidden — put Agent Notes under .agents/notes/"
            ));
        }
    }
    Ok(AgentNoteCheck {
        checked: tree.notes.len(),
        errors: tree.errors,
    })
}

/// Checks headers, required sections, alternatives, and retired markers.
///
/// # Errors
///
/// Returns tree traversal or note-read failures.
pub fn verify_agent_note_format(repository_root: &Path) -> anyhow::Result<AgentNoteCheck> {
    let note_root = repository_root.join(".agents/notes");
    let mut tree = walk_agent_note_tree(repository_root)?;
    for note in &tree.notes {
        let raw = std::fs::read(note_root.join(&note.relative_path))?;
        let source = String::from_utf8_lossy(&raw);
        let lines = source.split('\n').collect::<Vec<_>>();
        let mut prose = Vec::new();
        let mut in_fence = false;
        for line in &lines {
            if line.starts_with("```") {
                in_fence = !in_fence;
            } else if !in_fence {
                prose.push(*line);
            }
        }
        let mut fail = |message: String| {
            tree.errors
                .push(format!("format: {} — {message}", note.relative_path));
        };

        if !lines.first().is_some_and(|line| valid_title(line)) {
            fail("line 1 must be `# Agent Note: <title>`".into());
        }
        if lines.get(1).copied() != Some("") {
            fail("line 2 must be blank".into());
        }
        let (status_matches, status_grammar) = status_grammar(&note.lifecycle, lines.get(2));
        if !status_matches {
            fail(format!(
                "line 3 must match the {} status grammar ({status_grammar})",
                note.lifecycle
            ));
        }
        if lines.get(3).copied() != Some("") {
            fail("line 4 must be blank".into());
        }
        let expected_status = lines.get(2).copied().unwrap_or("");
        let other_status = prose
            .iter()
            .any(|line| line.starts_with("Status:") && *line != expected_status);
        let duplicate_status = prose
            .iter()
            .filter(|line| **line == expected_status)
            .count()
            > 1;
        if other_status || duplicate_status {
            fail("the line-3 `Status:` line must be the only one in the file".into());
        }

        let headings = prose
            .iter()
            .filter(|line| line.starts_with("## "))
            .map(|line| line.trim_end())
            .collect::<Vec<_>>();
        if headings.first().copied() != Some("## Problem") {
            let got = serde_json::to_string(headings.first().copied().unwrap_or("<none>"))?;
            fail(format!(
                "the first section must be `## Problem` (got {got})"
            ));
        }
        for required in required_sections(&note.lifecycle) {
            if !headings.contains(required) {
                fail(format!("missing the required `{required}` section"));
            }
        }
        if note.lifecycle == "implemented" {
            for heading in headings
                .iter()
                .filter(|heading| BANNED_IMPLEMENTED.is_match(heading))
            {
                fail(format!(
                    "`{heading}` is a proposal-era heading; an implemented Agent Note states what is (fold it into Decision/Consequences/Testing)"
                ));
            }
        }

        let has_section = headings.contains(&"## Alternatives considered");
        let has_grandfather = prose.contains(&GRANDFATHER);
        if has_section && has_grandfather {
            fail("carries both `## Alternatives considered` and the grandfather comment — drop the comment".into());
        }
        if !has_section && !has_grandfather {
            fail("missing `## Alternatives considered` (a pre-format Agent Note whose alternatives are not reconstructible carries the grandfather comment instead — see .agents/notes/README.md § The file format)".into());
        }
        if has_grandfather && note.date.as_str() >= FORMAT_ADOPTED {
            fail(format!(
                "the grandfather comment is only valid for Agent Notes dated before {FORMAT_ADOPTED}"
            ));
        }
        if prose
            .iter()
            .any(|line| LEGACY_MARKERS.iter().any(|marker| line.contains(marker)))
        {
            fail("carries the retired legacy-format debt marker".into());
        }
    }
    Ok(AgentNoteCheck {
        checked: tree.notes.len(),
        errors: tree.errors,
    })
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn valid_title(line: &str) -> bool {
    line.strip_prefix("# Agent Note: ")
        .and_then(|title| title.chars().next())
        .is_some_and(|character| !character.is_whitespace())
}

fn status_grammar(lifecycle: &str, line: Option<&&str>) -> (bool, &'static str) {
    let line = line.copied().unwrap_or("");
    match lifecycle {
        "proposed" => (line == "Status: proposed", "/^Status: proposed$/"),
        "implemented" => (line == "Status: implemented", "/^Status: implemented$/"),
        "rejected" => (
            line.strip_prefix("Status: rejected — ")
                .is_some_and(|reason| {
                    !reason.is_empty()
                        && !reason.chars().any(|character| {
                            matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
                        })
                }),
            "/^Status: rejected — .+$/",
        ),
        _ => (true, ""),
    }
}

fn required_sections(lifecycle: &str) -> &'static [&'static str] {
    match lifecycle {
        "proposed" => &["## Proposal", "## Acceptance criteria", "## Risks"],
        "implemented" => &["## Decision", "## Consequences"],
        "rejected" => &["## Proposal"],
        _ => &[],
    }
}
