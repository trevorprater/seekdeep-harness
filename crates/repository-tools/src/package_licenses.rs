//! MIT license policy for repository-owned SeekDeep packages.

use std::{collections::HashSet, path::Path};

use serde_json::{Map, Value};

use crate::repo_files::unique_repo_files;

const PACKAGE_PREFIX: &str = "@seekdeep-ai/seekdeep";

/// Result of checking every first-party package reachable from root workspaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageLicenseReport {
    /// Number of first-party manifests checked.
    pub package_count: usize,
    /// Repository-relative diagnostics for non-MIT declarations.
    pub failures: Vec<String>,
}

/// Checks every `SeekDeep` package declared by the repository workspace.
///
/// # Errors
///
/// Returns manifest discovery/read/JSON/shape or workspace-list failures.
pub fn inspect_seekdeep_package_licenses(root: &Path) -> anyhow::Result<PackageLicenseReport> {
    let mut package_count = 0;
    let mut failures = Vec::new();
    for file in workspace_manifest_paths(root)? {
        let manifest = read_manifest(root, &file)?;
        let Some(name) = manifest.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name != PACKAGE_PREFIX && !name.starts_with(&format!("{PACKAGE_PREFIX}-")) {
            continue;
        }
        package_count += 1;
        if manifest.get("license").and_then(Value::as_str) != Some("MIT") {
            failures.push(format!(
                "{file}: {name} must declare \"license\": \"MIT\"; found {}.",
                printable(manifest.get("license"))
            ));
        }
    }
    Ok(PackageLicenseReport {
        package_count,
        failures,
    })
}

fn workspace_manifest_paths(root: &Path) -> anyhow::Result<Vec<String>> {
    let root_manifest = read_manifest(root, "package.json")?;
    let workspaces = root_manifest
        .get("workspaces")
        .and_then(Value::as_array)
        .filter(|workspaces| workspaces.iter().all(Value::is_string))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "verify-seekdeep-package-licenses: package.json workspaces must be a string array."
            )
        })?;
    let patterns = workspaces
        .iter()
        .filter_map(Value::as_str)
        .map(|workspace| format!("{workspace}/package.json"))
        .collect::<Vec<_>>();
    let pattern_refs = patterns.iter().map(String::as_str).collect::<Vec<_>>();
    let mut files = HashSet::from(["package.json".to_owned()]);
    for file in unique_repo_files(root, &pattern_refs, |_| false)? {
        files.insert(
            file.absolute
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
    let mut files = files.into_iter().collect::<Vec<_>>();
    files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(files)
}

fn read_manifest(root: &Path, file: &str) -> anyhow::Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(&std::fs::read(root.join(file))?)?;
    value.as_object().cloned().ok_or_else(|| {
        anyhow::anyhow!("verify-seekdeep-package-licenses: {file} must contain a JSON object.")
    })
}

fn printable(value: Option<&Value>) -> String {
    value.map_or_else(
        || "undefined".into(),
        |value| serde_json::to_string(value).unwrap_or_else(|_| "undefined".into()),
    )
}
