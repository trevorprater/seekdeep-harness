//! Canonical package-README limitations-section policy.

use std::{collections::HashSet, fmt::Write as _, path::Path};

use regex::Regex;

use crate::markdown_util::{markdown_heading_lines, markdown_prose_lines};

/// Required limitations heading, including exact Markdown syntax.
pub const CANONICAL_LIMITATIONS_HEADING: &str = "## Known Limitations and Deferred Work";

const NO_LIMITATIONS: &[(&str, &str)] = &[(
    "packages/util/brand",
    "Type-only nominal-branding primitive with no runtime behavior or deferred work.",
)];

/// Full package-README policy inspection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageReadmeLimitationsReport {
    /// Package manifests inspected.
    pub checked: usize,
    /// Audited packages allowed to omit the section.
    pub whitelisted: usize,
    /// Ordered policy diagnostics.
    pub failures: Vec<String>,
}

/// Inspects every depth-two package manifest and sibling README.
///
/// # Errors
///
/// Returns directory traversal, file-read, relative-path, or Markdown parser
/// failures.
pub fn inspect_package_readme_limitations(
    root: &Path,
) -> anyhow::Result<PackageReadmeLimitationsReport> {
    let packages = package_directories(root)?;
    let scanned = packages.iter().cloned().collect::<HashSet<_>>();
    let mut failures = Vec::new();

    for (entry, reason) in NO_LIMITATIONS {
        if !scanned.contains(*entry) {
            failures.push(format!(
                "whitelist entry {entry} does not name a scanned package — renamed or removed? update NO_LIMITATIONS in scripts/verify-package-readme-limitations.ts in the same change"
            ));
        }
        if reason.trim().is_empty() {
            failures.push(format!(
                "whitelist entry {entry} has no justification — state why a limitations section would be empty boilerplate"
            ));
        }
    }

    for package in &packages {
        inspect_one_package(root, package, &mut failures)?;
    }
    Ok(PackageReadmeLimitationsReport {
        checked: packages.len(),
        whitelisted: NO_LIMITATIONS.len(),
        failures,
    })
}

/// Renders the source-compatible success or violation report.
#[must_use]
pub fn render_package_readme_limitations_report(report: &PackageReadmeLimitationsReport) -> String {
    if report.failures.is_empty() {
        return format!(
            "verify-package-readme-limitations: {} package READMEs checked ({} whitelisted), all conform.\n",
            report.checked, report.whitelisted
        );
    }
    let mut output = "verify-package-readme-limitations: violations found:\n".to_owned();
    for failure in &report.failures {
        let _ = writeln!(output, "  {failure}");
    }
    output
}

fn inspect_one_package(
    root: &Path,
    package: &str,
    failures: &mut Vec<String>,
) -> anyhow::Result<()> {
    let readme = format!("{package}/README.md");
    let readme_path = root.join(&readme);
    if !readme_path.exists() {
        failures.push(format!(
            "{readme}: package manifest has no sibling README with the `{CANONICAL_LIMITATIONS_HEADING}` section"
        ));
        return Ok(());
    }
    let source = std::fs::read_to_string(readme_path)?;
    let lines = markdown_prose_lines(&source).map_err(anyhow::Error::msg)?;
    let headings = markdown_heading_lines(&source).map_err(anyhow::Error::msg)?;
    let limitations = headings
        .iter()
        .filter(|heading| limitations_like(&heading.text))
        .collect::<Vec<_>>();

    if NO_LIMITATIONS.iter().any(|(entry, _)| *entry == package) {
        for heading in limitations {
            failures.push(format!(
                "{readme}:{}: whitelisted as having no known limitations, but carries {} — drop the section or remove the package from NO_LIMITATIONS",
                heading.index,
                json_string(&heading.raw)
            ));
        }
        return Ok(());
    }

    let Some(heading) = limitations.first().copied() else {
        failures.push(format!(
            "{readme}: missing the `{CANONICAL_LIMITATIONS_HEADING}` section (a package with genuinely nothing to declare joins NO_LIMITATIONS in scripts/verify-package-readme-limitations.ts instead)"
        ));
        return Ok(());
    };
    if limitations.len() > 1 {
        let locations = limitations
            .iter()
            .map(|heading| heading.index.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        failures.push(format!(
            "{readme}: {} limitations-like headings (lines {locations}) — keep exactly one `{CANONICAL_LIMITATIONS_HEADING}` section",
            limitations.len()
        ));
        return Ok(());
    }
    if heading.depth != 2 || heading.raw.trim_end() != CANONICAL_LIMITATIONS_HEADING {
        failures.push(format!(
            "{readme}:{}: non-canonical heading {} — use `{CANONICAL_LIMITATIONS_HEADING}`",
            heading.index,
            json_string(&heading.raw)
        ));
        return Ok(());
    }

    let heading_at = lines
        .iter()
        .position(|line| line.index == heading.index)
        .unwrap_or_default();
    let heading_lines = headings
        .iter()
        .map(|heading| heading.index)
        .collect::<HashSet<_>>();
    let body = &lines[heading_at.saturating_add(1)..];
    let end = body
        .iter()
        .position(|line| heading_lines.contains(&line.index))
        .unwrap_or(body.len());
    if !body[..end].iter().any(|line| line.raw.starts_with("- ")) {
        failures.push(format!(
            "{readme}:{}: the `{CANONICAL_LIMITATIONS_HEADING}` section has no top-level `- ` bullet — state the limitations, or whitelist the package if there are genuinely none",
            heading.index
        ));
    }
    Ok(())
}

fn package_directories(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut packages = Vec::new();
    let root_packages = root.join("packages");
    if !root_packages.is_dir() {
        return Ok(packages);
    }
    for group in std::fs::read_dir(&root_packages)? {
        let group = group?;
        if !group.file_type()?.is_dir() || group.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            if !package.file_type()?.is_dir()
                || package.file_name().to_string_lossy().starts_with('.')
                || !package.path().join("package.json").is_file()
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

fn limitations_like(heading: &str) -> bool {
    static LIMITATIONS: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\blimitations?\b|deferred work|what is not here|^deferred\b|^non-goals?\b")
            .expect("static limitations-heading regex")
    });
    LIMITATIONS.is_match(heading)
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}
