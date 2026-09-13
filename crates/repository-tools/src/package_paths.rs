//! Stale root-relative `packages/…` reference validation.

use std::{collections::HashSet, fmt::Write as _, hash::BuildHasher, path::Path};

use regex::Regex;

use crate::repo_files::{
    ReferenceViolation, archived_agent_note_path, find_reference_violations, unique_repo_files,
};

/// Authored prose and source scopes that may cite package paths.
pub const PACKAGE_PATH_PATTERNS: &[&str] = &[
    "README.md",
    ".agents/notes/**/*.md",
    "docs/**/*.md",
    "packages/*/*.md",
    "packages/*/*/*.md",
    "AGENTS.md",
    "packages/AGENTS.md",
    "packages/**/*.ts",
    "examples/**/*.ts",
];

/// Full package-reference drift inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackagePathReport {
    /// Canonically unique files checked.
    pub checked: usize,
    /// Missing paths that name a currently live package leaf.
    pub violations: Vec<ReferenceViolation>,
}

/// Inspects all repository-authored package references.
///
/// # Errors
///
/// Returns repository traversal, canonicalization, or file-read failures.
pub fn inspect_package_paths(root: &Path) -> anyhow::Result<PackagePathReport> {
    let package_names = real_package_names(root)?;
    let files = unique_repo_files(root, PACKAGE_PATH_PATTERNS, package_path_excluded)?;
    let mut violations = Vec::new();
    for file in &files {
        violations.extend(find_package_path_violations(
            root,
            &file.absolute,
            &package_names,
        )?);
    }
    Ok(PackagePathReport {
        checked: files.len(),
        violations,
    })
}

/// Finds drifted package references in one authored file.
///
/// # Errors
///
/// Returns file-read or repository-relative-path failures.
pub fn find_package_path_violations<S: BuildHasher>(
    root: &Path,
    absolute_path: &Path,
    package_names: &HashSet<String, S>,
) -> anyhow::Result<Vec<ReferenceViolation>> {
    static PACKAGE_REFERENCE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"\bpackages/[A-Za-z0-9._/-]+").expect("static package reference regex")
    });
    find_reference_violations(
        root,
        absolute_path,
        &PACKAGE_REFERENCE,
        |reference| reference.trim_end_matches(['.', '/']).to_owned(),
        |reference| is_drifted_package_reference(root, reference, package_names),
    )
}

/// Renders the source-compatible success or failure report.
#[must_use]
pub fn render_package_path_report(report: &PackagePathReport) -> String {
    if report.violations.is_empty() {
        return format!(
            "verify-package-paths: {} file(s) checked, all packages/* references resolve.\n",
            report.checked
        );
    }
    let mut output =
        "verify-package-paths: broken packages/* references found (target does not exist):\n"
            .to_owned();
    for violation in &report.violations {
        let _ = writeln!(
            output,
            "  {}:{}  {}",
            violation.file, violation.line, violation.reference
        );
    }
    output
}

fn real_package_names(root: &Path) -> anyhow::Result<HashSet<String>> {
    let mut names = HashSet::new();
    let packages = root.join("packages");
    if !packages.is_dir() {
        return Ok(names);
    }
    for group in std::fs::read_dir(packages)? {
        let group = group?;
        if !group.file_type()?.is_dir() || group.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            if package.file_type()?.is_dir()
                && !package.file_name().to_string_lossy().starts_with('.')
            {
                names.insert(package.file_name().to_string_lossy().into_owned());
            }
        }
    }
    Ok(names)
}

fn is_drifted_package_reference<S: BuildHasher>(
    root: &Path,
    reference: &str,
    package_names: &HashSet<String, S>,
) -> bool {
    if root.join(reference).exists() {
        return false;
    }
    let parts = reference.split('/').collect::<Vec<_>>();
    if parts.iter().position(|part| *part == "lib") == Some(3)
        && root.join(parts[..3].join("/")).exists()
    {
        return false;
    }
    let segments = parts.iter().skip(1).copied().collect::<Vec<_>>();
    let scanned = if segments.len() > 1
        && segments
            .first()
            .is_some_and(|group| root.join("packages").join(group).exists())
    {
        &segments[1..]
    } else {
        &segments[..]
    };
    scanned
        .iter()
        .any(|segment| package_names.contains(*segment))
}

fn package_path_excluded(path: &str) -> bool {
    archived_agent_note_path(path)
        || path.contains("/lib/")
        || path.ends_with(".d.ts")
        || path.starts_with("vendor/")
}
