//! Reversible vendor package-name migration over a tracked repository tree.

#![allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "The source migration recognizes only its lowercase extension spellings."
)]

use std::{
    collections::BTreeMap, fmt::Write as _, fs, path::Path, process::Command, sync::OnceLock,
};

use anyhow::{Context as _, bail};
use regex::Regex;
use serde::Deserialize;

use crate::rescope_exact_edit::{ExactEditState, exact_edit_state};

const JS_SPACE: &str = r"[\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
const EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".js", ".mjs", ".cjs", ".tpl", ".json", ".yml", ".yaml", ".md",
];

/// A vendored directory's upstream and published npm identities.
#[derive(Clone, Debug, Deserialize)]
pub struct Rename {
    /// Directory name beneath `vendor/`.
    pub directory: String,
    /// Package identity before the migration.
    pub upstream: String,
    /// Package identity after the migration.
    pub scoped: String,
}

/// A site where a package spelling also identifies unrelated product data.
#[derive(Clone, Debug, Deserialize)]
pub struct GenericSkip {
    /// Repository-relative path with an ambiguous name.
    pub file: String,
    /// Upstream identities excluded from token rewriting at this path.
    pub upstream: Vec<String>,
}

/// One migration that cannot be expressed as a delimited package token.
#[derive(Clone, Debug, Deserialize)]
pub struct ExactEdit {
    /// Stable diagnostic identity for the required edit.
    pub id: String,
    /// Repository-relative edit target.
    pub file: String,
    /// Complete source text in the forward direction.
    pub find: String,
    /// Complete replacement text in the forward direction.
    pub replace: String,
    /// Required occurrence count for one complete application.
    pub expect: usize,
}

/// An occurrence-count invariant required after the forward migration.
#[derive(Clone, Debug, Deserialize)]
pub struct Postcondition {
    /// Repository-relative validation target.
    pub file: String,
    /// Literal text whose occurrences are counted.
    pub text: String,
    /// Required occurrence count after a forward migration.
    pub count: usize,
}

/// Pinned migration data; execution and validation are owned by Rust.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RescopePolicy {
    /// Source snapshot that defines the migration.
    pub source_commit: String,
    /// Complete upstream-to-published identity mapping.
    pub renames: Vec<Rename>,
    /// Context-sensitive exclusions from generic token rewriting.
    pub generic_skips: Vec<GenericSkip>,
    /// Exact migrations, in application order.
    pub exact_edits: Vec<ExactEdit>,
    /// Required forward-migration invariants.
    pub postconditions: Vec<Postcondition>,
}

/// Returns the complete source-pinned migration mapping.
///
/// # Panics
/// Panics if the embedded migration data is invalid.
#[must_use]
pub fn policy() -> &'static RescopePolicy {
    static POLICY: OnceLock<RescopePolicy> = OnceLock::new();
    POLICY.get_or_init(|| {
        serde_json::from_str(include_str!("rescope_vendor/policy.json"))
            .expect("embedded vendor migration policy is valid JSON")
    })
}

/// Whether the migration may inspect or rewrite a tracked file.
#[must_use]
pub fn excluded(file: &str) -> bool {
    if matches!(
        file,
        "scripts/rescope-vendor.ts" | "crates/repository-tools/src/rescope_vendor/policy.json"
    ) || file.starts_with(".agents/notes/")
        || file.starts_with("scripts/snapshots/")
        || matches!(
            file,
            "docs/rescope.md" | "docs/rescope.zh.md" | "pnpm-lock.yaml"
        )
        || file.ends_with(".i18n.yaml")
    {
        return true;
    }
    let parts = file.split('/').collect::<Vec<_>>();
    if let ["vendor", _, "README.md" | "LICENSE"] = parts.as_slice() {
        return true;
    }
    !EXTENSIONS.iter().any(|extension| file.ends_with(extension))
}

struct Pattern {
    upstream: String,
    from: String,
    to: String,
    token: Regex,
    yaml_name: Regex,
}

fn patterns(mapping: &RescopePolicy, reverse: bool) -> Vec<Pattern> {
    let mut renames = mapping
        .renames
        .iter()
        .map(|rename| {
            let (from, to) = if reverse {
                (&rename.scoped, &rename.upstream)
            } else {
                (&rename.upstream, &rename.scoped)
            };
            (&rename.upstream, from, to)
        })
        .collect::<Vec<_>>();
    renames.sort_by_key(|(_, from, _)| std::cmp::Reverse(from.encode_utf16().count()));
    renames.into_iter().map(|(upstream, from, to)| {
        let escaped = regex::escape(from);
        let suffix = r#"(?:/[^'"`\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]*)?"#;
        let token = ['\'', '"', '`'].map(|quote| format!("{quote}{escaped}{suffix}{quote}")).join("|");
        let yaml_name = format!(r"^({JS_SPACE}*(?:-{JS_SPACE}*)?name:[ \t]+){escaped}([ \t]*(?:#[^\r\u{{2028}}\u{{2029}}]*)?)(\r?)$");
        Pattern {
            upstream: upstream.clone(), from: from.clone(), to: to.clone(),
            token: Regex::new(&token).expect("escaped package token pattern"),
            yaml_name: Regex::new(&yaml_name).expect("escaped YAML name pattern"),
        }
    }).collect()
}

fn rewrite_line(line: &str, file: &str, mapping: &RescopePolicy, patterns: &[Pattern]) -> String {
    let mut output = line.to_owned();
    for pattern in patterns {
        if mapping
            .generic_skips
            .iter()
            .any(|skip| skip.file == file && skip.upstream.contains(&pattern.upstream))
        {
            continue;
        }
        output = pattern
            .token
            .replace_all(&output, |capture: &regex::Captures<'_>| {
                let matched = &capture[0];
                format!(
                    "{}{}{}",
                    &matched[..1],
                    pattern.to,
                    &matched[1 + pattern.from.len()..]
                )
            })
            .into_owned();
        output = output.split_inclusive(['\r', '\u{2028}', '\u{2029}']).fold(
            String::new(),
            |mut joined, segment| {
                let body = segment.trim_end_matches(['\r', '\u{2028}', '\u{2029}']);
                let rewritten =
                    pattern
                        .yaml_name
                        .replace_all(body, |capture: &regex::Captures<'_>| {
                            format!(
                                "{}{}{}{}",
                                &capture[1], pattern.to, &capture[2], &capture[3]
                            )
                        });
                joined.push_str(&rewritten);
                joined.push_str(&segment[body.len()..]);
                joined
            },
        );
    }
    output
}

fn rewrite_with_patterns(
    text: &str,
    file: &str,
    mapping: &RescopePolicy,
    patterns: &[Pattern],
) -> (String, usize) {
    let markdown = file.ends_with(".md");
    let prose = markdown && file.starts_with("docs/");
    let mut inside_fence = false;
    let mut changed = 0;
    let fence = Regex::new(&format!("^{JS_SPACE}*```")).expect("static fence pattern");
    let output = text
        .split('\n')
        .map(|line| {
            if markdown {
                if fence.is_match(line) {
                    inside_fence = !inside_fence;
                    return line.to_owned();
                }
                if !inside_fence && !prose {
                    return line.to_owned();
                }
            }
            let next = rewrite_line(line, file, mapping, patterns);
            changed += usize::from(next != line);
            next
        })
        .collect::<Vec<_>>()
        .join("\n");
    (output, changed)
}

/// Rewrites only the eligible lines and returns their changed-line count.
#[must_use]
pub fn rewrite(text: &str, file: &str, reverse: bool) -> (String, usize) {
    rewrite_with_patterns(text, file, policy(), &patterns(policy(), reverse))
}

/// Filesystem mutation mode, with an inspection-only default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Report the generic rewrite without writing files.
    Dry,
    /// Apply validated exact edits followed by token rewrites.
    Apply,
    /// Require an already applied, idempotent post-state.
    Check,
}

impl Mode {
    const fn name(self) -> &'static str {
        match self {
            Self::Dry => "dry",
            Self::Apply => "apply",
            Self::Check => "check",
        }
    }
}

/// Complete stdout/stderr and exit outcome for a migration invocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RescopeReport {
    /// Progress and successful completion text.
    pub stdout: String,
    /// Error diagnostics and problem-count summary.
    pub stderr: String,
    /// Individual validation failures in source order.
    pub failures: Vec<String>,
}

fn classify(file: &str) -> &'static str {
    let parts = file.split('/').collect::<Vec<_>>();
    if let ["vendor", _, "package.json"] = parts.as_slice() {
        return "vendor manifest name";
    }
    if file.ends_with("package.json") {
        return "package.json dependencies";
    }
    if [".ts", ".tsx", ".js", ".mjs", ".cjs", ".tpl"]
        .iter()
        .any(|extension| file.ends_with(extension))
    {
        return "code specifiers";
    }
    if file.ends_with(".yml") || file.ends_with(".yaml") {
        return "YAML plugin names";
    }
    if file.ends_with(".json") {
        return "JSON configuration";
    }
    "Markdown fences and docs prose"
}

fn fail(report: &mut RescopeReport, suffix: &str) {
    for failure in &report.failures {
        writeln!(&mut report.stderr, "rescope-vendor: {failure}").expect("String writes succeed");
    }
    writeln!(
        &mut report.stderr,
        "rescope-vendor: {} problem(s); {suffix}",
        report.failures.len()
    )
    .expect("String writes succeed");
}

fn check_postconditions(
    root: &Path,
    mapping: &RescopePolicy,
    report: &mut RescopeReport,
) -> anyhow::Result<()> {
    for check in &mapping.postconditions {
        let path = root.join(&check.file);
        let hits = if path.exists() {
            i64::try_from(read_text(&path)?.matches(&check.text).count())?
        } else {
            -1
        };
        if usize::try_from(hits).ok() != Some(check.count) {
            report.failures.push(format!(
                "postcondition: {} has {hits} occurrence(s) of {}, expected {}",
                check.file,
                serde_json::to_string(&check.text)?,
                check.count
            ));
        }
    }
    Ok(())
}

/// Runs the pinned migration against tracked files in the supplied repository.
///
/// # Errors
/// Returns Git, file-read, or file-write failures. Invalid exact-edit sites are
/// reported before any write, including in dry mode.
pub fn run(root: &Path, mode: Mode, reverse: bool) -> anyhow::Result<RescopeReport> {
    run_with_policy(root, mode, reverse, policy())
}

/// Runs a migration with an explicit mapping, for isolated upstream rehearsals.
///
/// # Errors
/// Returns Git, file-read, or file-write failures.
pub fn run_with_policy(
    root: &Path,
    mode: Mode,
    reverse: bool,
    mapping: &RescopePolicy,
) -> anyhow::Result<RescopeReport> {
    let tracked = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("rescope-vendor: git ls-files")?;
    if !tracked.status.success() {
        bail!(
            "rescope-vendor: git ls-files failed: {}",
            String::from_utf8_lossy(&tracked.stderr).trim_end()
        );
    }
    let tracked = String::from_utf8_lossy(&tracked.stdout);
    let files = tracked
        .split('\0')
        .filter(|file| !file.is_empty() && !excluded(file))
        .collect::<Vec<_>>();
    let mut report = RescopeReport::default();
    let mut planned = Vec::new();
    for edit in &mapping.exact_edits {
        let before = read_text(&root.join(&edit.file))?;
        let (find, replace) = if reverse {
            (&edit.replace, &edit.find)
        } else {
            (&edit.find, &edit.replace)
        };
        let state = exact_edit_state(&before, find, replace, edit.expect);
        if state == ExactEditState::Invalid {
            report.failures.push(format!("exact edit {}: {} is neither pending nor cleanly applied (duplicated, partial, or moved)", edit.id, edit.file));
        } else if mode == Mode::Check {
            if state != ExactEditState::Applied {
                report.failures.push(format!(
                    "exact edit {} did not land in {}",
                    edit.id, edit.file
                ));
            }
        } else if state == ExactEditState::Pending {
            planned.push((&edit.file, find, replace));
        }
    }
    if !report.failures.is_empty() {
        fail(&mut report, "nothing was written.");
        return Ok(report);
    }
    if mode == Mode::Apply {
        for (file, find, replace) in planned {
            let path = root.join(file);
            fs::write(&path, read_text(&path)?.replace(find, replace))?;
        }
    }
    let patterns = patterns(mapping, reverse);
    let mut counts = BTreeMap::<&str, (usize, usize)>::new();
    let mut outstanding = Vec::new();
    for file in &files {
        let path = root.join(file);
        let before = read_text(&path)?;
        let (after, lines) = rewrite_with_patterns(&before, file, mapping, &patterns);
        if after == before {
            continue;
        }
        outstanding.push(*file);
        let count = counts.entry(classify(file)).or_default();
        count.0 += 1;
        count.1 += lines;
        if mode == Mode::Apply {
            fs::write(path, after)?;
        }
    }
    writeln!(
        &mut report.stdout,
        "rescope-vendor: {}{} over {} tracked files",
        mode.name(),
        if reverse { " --reverse" } else { "" },
        files.len()
    )?;
    for (kind, (files, lines)) in counts {
        writeln!(
            &mut report.stdout,
            "  {kind:<24} {files:>4} file(s), {lines} line(s)"
        )?;
    }
    if mode != Mode::Dry {
        if !reverse {
            check_postconditions(root, mapping, &mut report)?;
        }
        if mode == Mode::Check {
            for file in outstanding {
                report.failures.push(format!(
                    "residue: {file} still carries a pre-rescope name token"
                ));
            }
        }
    }
    if !report.failures.is_empty() {
        fail(&mut report, "the mapping or an upstream site moved.");
    } else if mode == Mode::Check {
        report.stdout.push_str("rescope-vendor: post-state verified — no residue, every exact edit landed, idempotent.\n");
    } else if mode == Mode::Apply {
        report.stdout.push_str("rescope-vendor: applied. Run `pnpm install`, `pnpm run gen-third-party-notices`, and re-record the touched bilingual pairs.\n");
    }
    Ok(report)
}

fn read_text(path: &Path) -> anyhow::Result<String> {
    Ok(String::from_utf8_lossy(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .into_owned())
}
