//! Shared repository glob discovery and line-oriented reference scanning.

use std::{collections::HashSet, path::PathBuf};

use regex::Regex;

use crate::agent_note_tree::is_archived_agent_note_path;

/// One authored path plus its canonical target for symlink deduplication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoFile {
    /// Absolute path matched by the caller's glob.
    pub absolute: PathBuf,
    /// Absolute canonical path used only for deduplication.
    pub canonical: PathBuf,
}

/// One rejected line-oriented repository reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceViolation {
    /// Slash-normalized repository-relative file containing the reference.
    pub file: String,
    /// One-based line containing the reference.
    pub line: usize,
    /// Normalized reference text.
    pub reference: String,
}

/// Expands repository-relative globs and deduplicates canonical files.
///
/// Supported patterns use the repository's `*` within one path segment and
/// `**` across zero or more segments. Patterns are processed in order; each
/// pattern's matches use JavaScript-compatible UTF-16 sorting.
///
/// # Errors
///
/// Returns traversal, relative-path, or canonicalization failures.
pub fn unique_repo_files(
    root: &std::path::Path,
    patterns: &[&str],
    is_excluded: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<RepoFile>> {
    let mut entries = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || entry.path().strip_prefix(root).is_ok_and(|relative| {
                    let relative = slash_path(relative);
                    patterns
                        .iter()
                        .any(|pattern| glob_prefix_possible(pattern, &relative))
                })
        });
    for entry in walker {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }
        let relative = slash_path(entry.path().strip_prefix(root)?);
        entries.push((relative, entry.path().to_owned()));
    }

    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for pattern in patterns {
        let mut matches = entries
            .iter()
            .filter(|(relative, _)| glob_matches(pattern, relative))
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
        for (relative, absolute) in matches {
            if is_excluded(relative) {
                continue;
            }
            let canonical = std::fs::canonicalize(absolute)?;
            if !seen.insert(canonical.clone()) {
                continue;
            }
            files.push(RepoFile {
                absolute: absolute.clone(),
                canonical,
            });
        }
    }
    Ok(files)
}

/// Scans regex matches line by line and returns rejected normalized references.
///
/// # Errors
///
/// Returns file-read or repository-relative-path failures.
pub fn find_reference_violations(
    root: &std::path::Path,
    absolute_path: &std::path::Path,
    pattern: &Regex,
    normalize: impl Fn(&str) -> String,
    is_violation: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<ReferenceViolation>> {
    let relative = absolute_path.strip_prefix(root).map_or_else(
        |_| {
            let canonical_root = std::fs::canonicalize(root)?;
            absolute_path
                .strip_prefix(canonical_root)
                .map(std::path::Path::to_owned)
                .map_err(anyhow::Error::from)
        },
        |relative| Ok(relative.to_owned()),
    )?;
    let file = slash_path(&relative);
    let bytes = std::fs::read(absolute_path)?;
    let source = String::from_utf8_lossy(&bytes);
    let mut violations = Vec::new();
    for (index, line) in source.split('\n').enumerate() {
        for matched in pattern.find_iter(line) {
            let reference = normalize(matched.as_str());
            if is_violation(&reference) {
                violations.push(ReferenceViolation {
                    file: file.clone(),
                    line: index + 1,
                    reference,
                });
            }
        }
    }
    Ok(violations)
}

/// Whether a repository path belongs to frozen Agent Note history.
#[must_use]
pub fn archived_agent_note_path(path: &str) -> bool {
    is_archived_agent_note_path(path)
}

/// Matches one repository glob against one slash-separated path.
#[must_use]
pub fn repository_glob_matches(pattern: &str, candidate: &str) -> bool {
    glob_matches(pattern, candidate)
}

fn glob_matches(pattern: &str, candidate: &str) -> bool {
    let (pattern, candidate) = if cfg!(any(target_os = "macos", windows)) {
        (pattern.to_ascii_lowercase(), candidate.to_ascii_lowercase())
    } else {
        (pattern.to_owned(), candidate.to_owned())
    };
    match_segments(
        &pattern.split('/').collect::<Vec<_>>(),
        &candidate.split('/').collect::<Vec<_>>(),
    )
}

fn glob_prefix_possible(pattern: &str, candidate: &str) -> bool {
    let (pattern, candidate) = if cfg!(any(target_os = "macos", windows)) {
        (pattern.to_ascii_lowercase(), candidate.to_ascii_lowercase())
    } else {
        (pattern.to_owned(), candidate.to_owned())
    };
    match_prefix_segments(
        &pattern.split('/').collect::<Vec<_>>(),
        &candidate.split('/').collect::<Vec<_>>(),
    )
}

fn match_segments(pattern: &[&str], candidate: &[&str]) -> bool {
    let Some((head, tail)) = pattern.split_first() else {
        return candidate.is_empty();
    };
    if *head == "**" {
        return match_segments(tail, candidate)
            || candidate.split_first().is_some_and(|(value, rest)| {
                !value.starts_with('.') && match_segments(pattern, rest)
            });
    }
    candidate.split_first().is_some_and(|(value, rest)| {
        (!value.starts_with('.') || head.starts_with('.'))
            && match_segment(head, value)
            && match_segments(tail, rest)
    })
}

fn match_prefix_segments(pattern: &[&str], candidate: &[&str]) -> bool {
    if candidate.is_empty() {
        return true;
    }
    let Some((head, tail)) = pattern.split_first() else {
        return false;
    };
    if *head == "**" {
        return match_prefix_segments(tail, candidate)
            || candidate.split_first().is_some_and(|(value, rest)| {
                !value.starts_with('.') && match_prefix_segments(pattern, rest)
            });
    }
    candidate.split_first().is_some_and(|(value, rest)| {
        (!value.starts_with('.') || head.starts_with('.'))
            && match_segment(head, value)
            && match_prefix_segments(tail, rest)
    })
}

fn match_segment(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.as_bytes();
    let candidate = candidate.as_bytes();
    let (mut pattern_index, mut candidate_index, mut star, mut checkpoint) = (0, 0, None, 0);
    while candidate_index < candidate.len() {
        if pattern.get(pattern_index) == candidate.get(candidate_index) {
            pattern_index += 1;
            candidate_index += 1;
        } else if pattern.get(pattern_index) == Some(&b'*') {
            star = Some(pattern_index);
            pattern_index += 1;
            checkpoint = candidate_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            checkpoint += 1;
            candidate_index = checkpoint;
        } else {
            return false;
        }
    }
    while pattern.get(pattern_index) == Some(&b'*') {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn slash_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_subset_handles_zero_or_more_directories_and_segment_wildcards() {
        for (pattern, candidate, expected) in [
            ("packages/**/*.ts", "packages/a.ts", true),
            ("packages/**/*.ts", "packages/core/a/src/index.ts", true),
            ("packages/*/*.md", "packages/core/README.md", true),
            ("packages/*/*.md", "packages/core/a/README.md", false),
            ("README.md", "README.md", true),
            ("README.md", "docs/README.md", false),
        ] {
            assert_eq!(
                glob_matches(pattern, candidate),
                expected,
                "{pattern} {candidate}"
            );
        }
    }
}
