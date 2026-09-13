//! Intra-package compatibility-client domain layering policy.

use std::{fmt::Write as _, path::Path};

use regex::Regex;

const ASSEMBLY_FILES: &[&str] = &["apply.ts", "index.ts", "index.tsx"];

/// One prohibited compatibility-client domain import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientDomainViolation {
    /// Package-relative authored source path.
    pub file: String,
    /// Relative import specifier.
    pub imported: String,
    /// Domain-layer explanation.
    pub reason: String,
}

/// Scans every `packages/client/*/src/client` compatibility source tree.
///
/// # Errors
///
/// Returns directory traversal, relative-path, or file-read failures.
pub fn inspect_client_domain_graph(root: &Path) -> anyhow::Result<Vec<ClientDomainViolation>> {
    let client_packages = root.join("packages/client");
    if !client_packages.is_dir() {
        return Ok(Vec::new());
    }
    let mut violations = Vec::new();
    for package in std::fs::read_dir(client_packages)? {
        let package = package?;
        let client = package.path().join("src/client");
        if !client.is_dir() {
            continue;
        }
        violations.extend(check_client_package(
            &package.file_name().to_string_lossy(),
            &client,
        )?);
    }
    Ok(violations)
}

/// Renders the source-compatible success or violation report.
#[must_use]
pub fn render_client_domain_graph_report(violations: &[ClientDomainViolation]) -> String {
    if violations.is_empty() {
        return "verify-client-domain-graph: client domain layering clean.\n".to_owned();
    }
    let mut output = format!(
        "verify-client-domain-graph: {} violation(s):\n",
        violations.len()
    );
    for violation in violations {
        let _ = writeln!(
            output,
            "  {} -> {}\n    {}",
            violation.file, violation.imported, violation.reason
        );
    }
    output
}

fn check_client_package(
    package: &str,
    client_directory: &Path,
) -> anyhow::Result<Vec<ClientDomainViolation>> {
    static IMPORT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"from\s+['"](\.[^'"]+)['"]"#).expect("static relative import regex")
    });
    let mut files = client_sources(client_directory)?;
    files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    let mut violations = Vec::new();
    for relative in files {
        let from_domain = domain_of(&relative);
        if from_domain.is_empty() && ASSEMBLY_FILES.contains(&relative.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(client_directory.join(&relative))?;
        for captures in IMPORT.captures_iter(&source) {
            let specifier = captures[1].to_owned();
            let target = resolve_relative_client_path(&relative, &specifier);
            if target.starts_with("..") {
                continue;
            }
            let to_domain = domain_of(&target);
            if to_domain.is_empty() || to_domain == "contract" || from_domain == to_domain {
                continue;
            }
            let reason = if from_domain.is_empty() {
                format!(
                    "top-level non-assembly file imports domain \"{to_domain}\" (only apply/index may assemble)"
                )
            } else {
                format!(
                    "domain \"{from_domain}\" imports sibling domain \"{to_domain}\" (route shared API through contract/)"
                )
            };
            violations.push(ClientDomainViolation {
                file: format!("{package}/src/client/{relative}"),
                imported: specifier,
                reason,
            });
        }
    }
    Ok(violations)
}

fn client_sources(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = slash_path(entry.path().strip_prefix(root)?);
        let basename = entry.file_name().to_string_lossy();
        let extension = Path::new(basename.as_ref())
            .extension()
            .and_then(std::ffi::OsStr::to_str);
        if !matches!(extension, Some("ts" | "tsx")) || basename.contains(".legacy.") {
            continue;
        }
        files.push(relative);
    }
    Ok(files)
}

fn domain_of(relative: &str) -> &str {
    relative.split_once('/').map_or("", |(domain, _)| domain)
}

fn resolve_relative_client_path(importer: &str, specifier: &str) -> String {
    let mut parts = importer
        .rsplit_once('/')
        .map_or_else(Vec::new, |(directory, _)| {
            directory.split('/').map(str::to_owned).collect()
        });
    for segment in specifier.split('/') {
        match segment {
            "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(segment.to_owned()),
        }
    }
    parts.join("/")
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
