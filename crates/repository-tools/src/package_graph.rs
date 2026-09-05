//! Dependency-safe workspace package graph discovery and Mermaid helpers.

use std::{collections::HashSet, path::Path};

use indexmap::IndexMap;
use serde_json::Value;

const PACKAGE_SCOPE: &str = "@seekdeep-ai/seekdeep-";

/// One `SeekDeep` package and its in-repository peer-dependency edges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageGraphNode {
    /// Package name without the `@seekdeep-ai/seekdeep-` prefix.
    pub short: String,
    /// Full npm package name.
    pub name: String,
    /// Package group from `packages/<group>/<package>`.
    pub group: String,
    /// Repository-relative package directory.
    pub relative: String,
    /// Short in-repository peer-dependency names, sorted.
    pub dependencies: Vec<String>,
}

/// Reads every `SeekDeep` manifest and returns dependency-safe graph nodes.
///
/// # Errors
///
/// Returns package traversal, file-read, JSON, manifest-shape, path-shape, or
/// dependency-cycle diagnostics.
pub fn collect_package_graph(
    root: &Path,
    group_order: &[String],
    gate: &str,
) -> anyhow::Result<Vec<PackageGraphNode>> {
    let mut manifests = package_manifests(root)?;
    manifests.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    let mut packages = Vec::new();
    for manifest_path in manifests {
        let manifest =
            serde_json::from_str::<Value>(&std::fs::read_to_string(root.join(&manifest_path))?)?;
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("{manifest_path}: package name must be a string"))?;
        let Some(short) = name.strip_prefix(PACKAGE_SCOPE) else {
            continue;
        };
        let parts = manifest_path.split('/').collect::<Vec<_>>();
        let (Some(group), Some(package)) = (parts.get(1), parts.get(2)) else {
            anyhow::bail!("{gate}: unexpected package path {manifest_path}");
        };
        let mut dependencies = manifest
            .get("peerDependencies")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(dependency, _)| dependency.strip_prefix(PACKAGE_SCOPE).map(str::to_owned))
            .collect::<Vec<_>>();
        dependencies.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        packages.push(PackageGraphNode {
            short: short.to_owned(),
            name: name.to_owned(),
            group: (*group).to_owned(),
            relative: format!("packages/{group}/{package}"),
            dependencies,
        });
    }
    topological_sort(packages, group_order, gate)
}

/// Returns a stable Mermaid identifier using JavaScript UTF-16 replacement.
#[must_use]
pub fn graph_node_id(prefix: &str, value: &str) -> String {
    let mut result = format!("{prefix}_");
    for unit in value.encode_utf16() {
        let ascii = u8::try_from(unit)
            .ok()
            .filter(|ascii| ascii.is_ascii_alphanumeric() || *ascii == b'_');
        if let Some(ascii) = ascii {
            result.push(char::from(ascii));
        } else {
            result.push('_');
        }
    }
    result
}

/// Escapes double quotes inside a quoted Mermaid label.
#[must_use]
pub fn escape_mermaid_label(value: &str) -> String {
    value.replace('"', "\\\"")
}

fn topological_sort(
    packages: Vec<PackageGraphNode>,
    group_order: &[String],
    gate: &str,
) -> anyhow::Result<Vec<PackageGraphNode>> {
    let mut remaining = packages
        .into_iter()
        .map(|package| (package.short.clone(), package))
        .collect::<IndexMap<_, _>>();
    let mut placed = HashSet::new();
    let mut output = Vec::new();
    while !remaining.is_empty() {
        let mut ready = remaining
            .values()
            .filter(|package| {
                package
                    .dependencies
                    .iter()
                    .all(|dependency| placed.contains(dependency))
            })
            .cloned()
            .collect::<Vec<_>>();
        ready.sort_by(|left, right| compare_packages(left, right, group_order));
        if ready.is_empty() {
            anyhow::bail!(
                "{gate}: dependency cycle among {}",
                remaining.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        for package in ready {
            placed.insert(package.short.clone());
            remaining.shift_remove(&package.short);
            output.push(package);
        }
    }
    Ok(output)
}

fn compare_packages(
    left: &PackageGraphNode,
    right: &PackageGraphNode,
    group_order: &[String],
) -> std::cmp::Ordering {
    let left_group = group_order.iter().position(|group| group == &left.group);
    let right_group = group_order.iter().position(|group| group == &right.group);
    left_group
        .unwrap_or(usize::MAX)
        .cmp(&right_group.unwrap_or(usize::MAX))
        .then_with(|| utf16_compare(&left.group, &right.group))
        .then_with(|| utf16_compare(&left.short, &right.short))
}

fn package_manifests(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut manifests = Vec::new();
    let packages = root.join("packages");
    if !packages.is_dir() {
        return Ok(manifests);
    }
    for group in std::fs::read_dir(packages)? {
        let group = group?;
        if !group.file_type()?.is_dir() || hidden(&group.file_name()) {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            if !package.file_type()?.is_dir() || hidden(&package.file_name()) {
                continue;
            }
            if package.path().join("package.json").is_file() {
                manifests.push(format!(
                    "packages/{}/{}/package.json",
                    group.file_name().to_string_lossy(),
                    package.file_name().to_string_lossy()
                ));
            }
        }
    }
    Ok(manifests)
}

fn utf16_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}
