//! Host/Client isolation validation across compatibility Project References.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use path_clean::PathClean as _;
use serde_json::Value;

use crate::clean::parse_jsonc_value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectFace {
    Host,
    Client,
}

/// Finds references that enter the wrong leaf of a split Host/Client project.
///
/// # Errors
///
/// Returns workspace traversal, file-read, JSONC, or relative-path failures.
pub fn collect_project_reference_face_violations(root: &Path) -> anyhow::Result<Vec<String>> {
    let root = root.clean();
    let split_roots = split_project_roots(&root)?;
    let mut violations = Vec::new();
    let mut pending = vec![
        root.join("tsconfig.host.json"),
        root.join("tsconfig.client.json"),
    ];
    let mut visited = HashSet::new();
    while let Some(config_path) = pending.pop() {
        if !visited.insert(config_path.clone()) || !config_path.exists() {
            continue;
        }
        let config = project_config(&root, &config_path)?;
        let face = project_face(&root, &config_path, &config, &mut HashSet::new())?;
        for reference in project_references(&config) {
            let target_config = reference_config_path(&config_path, &reference);
            if let Some(split_root) = containing_split_root(&split_roots, &target_config) {
                let Some(face) = face else {
                    violations.push(format!(
                        "{}: Project Reference {} enters split project {} from a config with no Host/Client face",
                        repo_path(&root, &config_path),
                        json_string(&reference),
                        repo_path(&root, split_root)
                    ));
                    continue;
                };
                let expected = split_root.join(format!("tsconfig.{}.json", face_name(face)));
                if target_config != expected {
                    violations.push(format!(
                        "{}: Project Reference {} enters split project {} from a {} config; reference {} instead",
                        repo_path(&root, &config_path),
                        json_string(&reference),
                        repo_path(&root, split_root),
                        face_label(face),
                        json_string(&repo_path(&root, &expected))
                    ));
                    continue;
                }
            }
            pending.push(target_config);
        }
    }
    violations.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(violations)
}

fn split_project_roots(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut roots = workspace_directories(root)?
        .into_iter()
        .filter(|directory| {
            directory.join("tsconfig.host.json").exists()
                && directory.join("tsconfig.client.json").exists()
        })
        .collect::<Vec<_>>();
    roots.sort_by_key(|path| std::cmp::Reverse(path.to_string_lossy().encode_utf16().count()));
    Ok(roots)
}

fn workspace_directories(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    let packages = root.join("packages");
    if packages.is_dir() {
        for group in child_directories(&packages)? {
            for package in child_directories(&group)? {
                if package.join("package.json").is_file() {
                    directories.push(package);
                }
            }
        }
    }
    for parent in [root.join("apps"), root.join("vendor")] {
        if parent.is_dir() {
            for directory in child_directories(&parent)? {
                if directory.join("package.json").is_file() {
                    directories.push(directory);
                }
            }
        }
    }
    Ok(directories)
}

fn project_config(root: &Path, config_path: &Path) -> anyhow::Result<Value> {
    let source = std::fs::read_to_string(config_path)?;
    parse_jsonc_value(&source)
        .map_err(|error| anyhow::anyhow!("{}: {error}", repo_path(root, config_path)))
}

fn project_references(config: &Value) -> Vec<String> {
    config
        .get("references")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reference| reference.get("path").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn project_face(
    root: &Path,
    config_path: &Path,
    config: &Value,
    seen: &mut HashSet<PathBuf>,
) -> anyhow::Result<Option<ProjectFace>> {
    match config_path.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("tsconfig.host.json") => return Ok(Some(ProjectFace::Host)),
        Some("tsconfig.client.json") => return Ok(Some(ProjectFace::Client)),
        _ => {}
    }
    if config_path == root.join("tsconfig.base.json") {
        return Ok(Some(ProjectFace::Host));
    }
    if config_path == root.join("tsconfig.base.client.json") {
        return Ok(Some(ProjectFace::Client));
    }
    if !seen.insert(config_path.to_owned()) {
        return Ok(None);
    }
    let Some(parent) = local_extends_config(config_path, config.get("extends")) else {
        return Ok(None);
    };
    if !parent.exists() {
        return Ok(None);
    }
    let parent_config = project_config(root, &parent)?;
    project_face(root, &parent, &parent_config, seen)
}

fn local_extends_config(config_path: &Path, value: Option<&Value>) -> Option<PathBuf> {
    let value = value?.as_str()?;
    if !value.starts_with('.') {
        return None;
    }
    let target = config_path.parent()?.join(value).clean();
    Some(if target.to_string_lossy().ends_with(".json") {
        target
    } else {
        PathBuf::from(format!("{}.json", target.to_string_lossy()))
    })
}

fn reference_config_path(source_config: &Path, reference: &str) -> PathBuf {
    let target = source_config
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(reference)
        .clean();
    if target.to_string_lossy().ends_with(".json") {
        target
    } else {
        target.join("tsconfig.json")
    }
}

fn containing_split_root<'a>(roots: &'a [PathBuf], target: &Path) -> Option<&'a PathBuf> {
    roots.iter().find(|root| target.starts_with(root))
}

fn repo_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn face_name(face: ProjectFace) -> &'static str {
    match face {
        ProjectFace::Host => "host",
        ProjectFace::Client => "client",
    }
}

fn face_label(face: ProjectFace) -> &'static str {
    match face {
        ProjectFace::Host => "Host",
        ProjectFace::Client => "Client",
    }
}

fn child_directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            directories.push(entry.path().clean());
        }
    }
    Ok(directories)
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}
