//! Required workspace-peer closure for executable deployment manifests.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
};

use serde_json::Value;

/// Runtime dependency-closure result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeClosureReport {
    /// Workspace packages reachable through runtime dependencies.
    pub packages: usize,
    /// Missing required peer chains in traversal order.
    pub failures: Vec<String>,
}

/// Verifies one deployment manifest's required workspace-peer closure.
///
/// # Errors
///
/// Returns workspace traversal, manifest read, JSON, or manifest-shape failures.
pub fn inspect_runtime_closure(
    root: &Path,
    runtime_manifest_path: &Path,
) -> anyhow::Result<RuntimeClosureReport> {
    let runtime_manifest = load_manifest(runtime_manifest_path)?;
    let runtime_name = runtime_manifest
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("python/sdk-runtime");
    let workspace = load_workspace_packages(root)?;
    let runtime_dependencies = object_field(&runtime_manifest, "dependencies");
    let mut parents = HashMap::<String, Option<String>>::new();
    let mut queue = VecDeque::new();
    for dependency in sorted_keys(runtime_dependencies) {
        if !workspace.contains_key(&dependency) {
            continue;
        }
        parents.insert(dependency.clone(), None);
        queue.push_back(dependency);
    }

    let mut failures = Vec::new();
    let mut visited_count = 0;
    while let Some(package_name) = queue.pop_front() {
        visited_count += 1;
        let Some(current) = workspace.get(&package_name) else {
            continue;
        };
        let peers = object_field(current, "peerDependencies");
        let peer_meta = object_field(current, "peerDependenciesMeta");
        for peer in sorted_keys(peers) {
            let optional = peer_meta
                .and_then(|meta| meta.get(&peer))
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get("optional"))
                .and_then(Value::as_bool)
                == Some(true);
            if !workspace.contains_key(&peer) || optional {
                continue;
            }
            let supplied = runtime_dependencies
                .and_then(|dependencies| dependencies.get(&peer))
                .and_then(Value::as_str)
                .is_some_and(|version| version.starts_with("workspace:"));
            if !supplied {
                failures.push(format!(
                    "{} -> {peer}",
                    format_chain(runtime_name, &package_name, &parents)
                ));
            }
        }

        let mut dependencies = object_field(current, "dependencies")
            .into_iter()
            .flatten()
            .map(|(name, version)| (name.clone(), version.clone()))
            .collect::<serde_json::Map<_, _>>();
        if let Some(optional) = object_field(current, "optionalDependencies") {
            for (name, version) in optional {
                dependencies.insert(name.clone(), version.clone());
            }
        }
        for dependency in sorted_keys(Some(&dependencies)) {
            if !workspace.contains_key(&dependency) || parents.contains_key(&dependency) {
                continue;
            }
            parents.insert(dependency.clone(), Some(package_name.clone()));
            queue.push_back(dependency);
        }
    }

    Ok(RuntimeClosureReport {
        packages: visited_count,
        failures,
    })
}

/// Renders the source-compatible success or failure report.
#[must_use]
pub fn render_runtime_closure_report(report: &RuntimeClosureReport) -> String {
    if report.failures.is_empty() {
        return format!(
            "verify-runtime-closure: {} workspace packages form a closed runtime dependency graph.\n",
            report.packages
        );
    }
    let mut output = "verify-runtime-closure: required workspace peers are missing from python/sdk-runtime dependencies:\n".to_owned();
    for failure in &report.failures {
        output.push_str("  ");
        output.push_str(failure);
        output.push('\n');
    }
    output
}

fn load_workspace_packages(root: &Path) -> anyhow::Result<HashMap<String, Value>> {
    let mut manifests = workspace_manifests(root)?;
    manifests.sort_by(|left, right| {
        relative_path(root, left)
            .encode_utf16()
            .cmp(relative_path(root, right).encode_utf16())
    });
    let mut workspace = HashMap::new();
    for path in manifests {
        let manifest = load_manifest(&path)?;
        if let Some(name) = manifest.get("name").and_then(Value::as_str) {
            workspace.insert(name.to_owned(), manifest);
        }
    }
    Ok(workspace)
}

fn workspace_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    let packages = root.join("packages");
    if packages.is_dir() {
        for group in child_directories(&packages)? {
            for package in child_directories(&group)? {
                let manifest = package.join("package.json");
                if manifest.is_file() {
                    manifests.push(manifest);
                }
            }
        }
    }
    let vendor = root.join("vendor");
    if vendor.is_dir() {
        for package in child_directories(&vendor)? {
            let manifest = package.join("package.json");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    Ok(manifests)
}

fn load_manifest(path: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn object_field<'a>(value: &'a Value, field: &str) -> Option<&'a serde_json::Map<String, Value>> {
    value.get(field).and_then(Value::as_object)
}

fn sorted_keys(object: Option<&serde_json::Map<String, Value>>) -> Vec<String> {
    let mut keys = object
        .into_iter()
        .flatten()
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    keys
}

fn format_chain(
    runtime_name: &str,
    package_name: &str,
    parents: &HashMap<String, Option<String>>,
) -> String {
    let mut chain = vec![package_name.to_owned()];
    let mut parent = parents.get(package_name).and_then(Clone::clone);
    while let Some(package) = parent {
        chain.insert(0, package.clone());
        parent = parents.get(&package).and_then(Clone::clone);
    }
    let mut complete = vec![runtime_name.to_owned()];
    complete.extend(chain);
    complete.join(" -> ")
}

fn child_directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
