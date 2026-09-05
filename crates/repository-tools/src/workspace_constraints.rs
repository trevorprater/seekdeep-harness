//! Workspace publication, layout, dependency, and compiler-face requirements.

mod package;

use std::{collections::HashSet, path::Path};

use serde_json::Value;

use crate::project_reference_faces::collect_project_reference_face_violations;

const WORKSPACES: &[(&str, usize)] = &[
    ("vendor", 1),
    ("packages", 2),
    ("native", 1),
    ("native/landlock-run/packages", 1),
    ("apps", 1),
];

/// Checks all workspace manifests and the Host/Client project-reference graph.
///
/// Diagnostics retain discovery and rule order; an empty list means success.
///
/// # Errors
/// Returns missing directories, unreadable manifests, malformed JSON, or invalid
/// compiler-reference configuration instead of treating an incomplete scan as valid.
pub fn inspect_workspace_constraints(root: &Path) -> anyhow::Result<Vec<String>> {
    let root_manifest = read_manifest(&root.join("package.json"))?;
    let landlock = read_manifest(&root.join("native/landlock-run/package.json"))?;
    let mut manifests = vec![(".".to_owned(), root_manifest.clone())];
    for &(base, depth) in WORKSPACES {
        for directory in package_directories(root, base, depth)? {
            let manifest = read_manifest(&root.join(&directory).join("package.json"))?;
            manifests.push((directory, manifest));
        }
    }
    let mut errors = Vec::new();
    let version = root_manifest["version"].as_str();
    let valid_version = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$")?;
    if !version.is_some_and(|version| valid_version.is_match(version)) {
        errors
            .push("package.json: version must be X.Y.Z with an optional prerelease segment".into());
    }
    for (directory, manifest) in &manifests {
        let path = if directory == "." {
            "package.json".to_owned()
        } else {
            format!("{directory}/package.json")
        };
        errors.extend(
            package::check(
                directory,
                manifest,
                &root_manifest["version"],
                &landlock["version"],
            )?
            .into_iter()
            .map(|error| format!("{path}: {error}")),
        );
    }
    errors.extend(workspace_protocol(&manifests));
    errors.extend(hierarchy(root)?);
    errors.extend(collect_project_reference_face_violations(root)?);
    Ok(errors)
}

fn read_manifest(path: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn directories(path: &Path) -> anyhow::Result<Vec<String>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.file_name().into_string().map_err(|name| {
                anyhow::anyhow!(
                    "workspace directory name is not UTF-8: {}",
                    name.to_string_lossy()
                )
            })?);
        }
    }
    directories.sort();
    Ok(directories)
}

fn package_directories(root: &Path, base: &str, depth: usize) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    for name in directories(&root.join(base))? {
        if name == "node_modules" {
            continue;
        }
        let directory = format!("{base}/{name}");
        if depth == 1 {
            if root.join(&directory).join("package.json").exists() {
                result.push(directory);
            }
        } else {
            result.extend(package_directories(root, &directory, depth - 1)?);
        }
    }
    Ok(result)
}

fn workspace_protocol(manifests: &[(String, Value)]) -> Vec<String> {
    let members: HashSet<_> = manifests
        .iter()
        .filter_map(|(_, value)| value["name"].as_str())
        .collect();
    let mut errors = Vec::new();
    for (directory, manifest) in manifests {
        for section in [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ] {
            let Some(dependencies) = manifest[section].as_object() else {
                continue;
            };
            for (name, range) in dependencies {
                if members.contains(name.as_str())
                    && !range
                        .as_str()
                        .is_some_and(|range| range.starts_with("workspace:"))
                {
                    let label = manifest["name"].as_str().unwrap_or(directory);
                    let range = range
                        .as_str()
                        .map_or_else(|| range.to_string(), str::to_owned);
                    errors.push(format!(
                        "{label}: {section}.{name} must use the workspace: protocol, got {range}"
                    ));
                }
            }
        }
    }
    errors
}

fn hierarchy(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut errors = Vec::new();
    for group in directories(&root.join("packages"))? {
        let group = format!("packages/{group}");
        if root.join(&group).join("package.json").exists() {
            errors.push(format!("{group}: a group dir must not contain a package.json — packages live at packages/<group>/<pkg>, not directly under packages/"));
            continue;
        }
        for package in directories(&root.join(&group))? {
            if package == "node_modules" {
                continue;
            }
            let package = format!("{group}/{package}");
            if !root.join(&package).join("package.json").exists() {
                errors.push(format!("{package}: expected a package here (no package.json found) — the hierarchy is exactly packages/<group>/<pkg>, no deeper nesting"));
            }
        }
    }
    Ok(errors)
}
