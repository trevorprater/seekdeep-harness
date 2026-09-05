//! Filesystem discovery and shallow Loader-dialect health checks.

use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use path_clean::PathClean as _;
use seekdeep_loader::profile_patch::{
    ProfileEntry, ProfileNode, ProfilePatchError, parse_entry_list_yaml,
};
use seekdeep_util::home_paths::expand_home_path;

use crate::{
    metadata::read_preset_metadata,
    preset::{AgentPreset, PresetRoot, valid_preset_id},
};

/// Composition filename that makes one directory a preset.
pub const COMPOSITION_FILE: &str = "agent.cordis.yml";
/// Harness-home directory holding locally authored presets.
pub const USER_PRESET_DIR: &str = ".agent-presets";

/// Scans one root, returning visible broken slots as roster rows.
///
/// # Errors
///
/// Returns non-absence directory read failures.
pub async fn scan_root(root: &PresetRoot) -> anyhow::Result<Vec<AgentPreset>> {
    let directory = absolute_root(&root.path)?;
    let mut children = match tokio::fs::read_dir(&directory).await {
        Ok(children) => children,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "agent-presets: cannot read preset root {}: {error}",
                directory.display()
            ));
        }
    };
    let mut found = Vec::new();
    while let Some(child) = children.next_entry().await? {
        if !child.file_type().await?.is_dir() {
            continue;
        }
        let id = child.file_name().to_string_lossy().into_owned();
        if !valid_preset_id(&id) {
            continue;
        }
        let preset_directory = child.path();
        let path = preset_directory.join(COMPOSITION_FILE);
        let broken = if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            composition_problem(&path).await
        } else {
            Some(format!(
                "the composition file {COMPOSITION_FILE} is missing — the directory still occupies the id; delete it or restore the file"
            ))
        };
        let metadata = read_preset_metadata(&preset_directory).await;
        found.push(AgentPreset {
            id,
            trust: root.trust,
            path,
            name: metadata.name,
            description: metadata.description,
            order: metadata.order,
            broken,
        });
    }
    found.sort_by(|left, right| {
        match (left.order, right.order) {
            (Some(left), Some(right)) => left.total_cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| left.id.cmp(&right.id))
    });
    Ok(found)
}

/// Discovers every root sequentially with first-root-wins identity precedence.
///
/// # Errors
///
/// Returns the first root read failure.
pub async fn discover_presets(roots: &[PresetRoot]) -> anyhow::Result<Vec<AgentPreset>> {
    let mut seen = HashSet::new();
    let mut presets = Vec::new();
    for root in roots {
        for preset in scan_root(root).await? {
            if seen.insert(preset.id.clone()) {
                presets.push(preset);
            }
        }
    }
    Ok(presets)
}

async fn composition_problem(path: &Path) -> Option<String> {
    let Ok(raw) = tokio::fs::read_to_string(path).await else {
        return Some(format!(
            "the composition file {COMPOSITION_FILE} cannot be read"
        ));
    };
    let entries = match parse_entry_list_yaml(&raw) {
        Ok(entries) => entries,
        Err(ProfilePatchError::TopLevelArrayRequired) => {
            return Some("the composition must be a top-level list of plugin rows".to_owned());
        }
        Err(ProfilePatchError::MappingRequired { context }) => {
            let index = context
                .split_whitespace()
                .find_map(|part| part.parse::<usize>().ok())
                .map_or(1, |index| index + 1);
            return Some(format!(
                "row {index} is not a plugin row (expected a map with a \"name\")"
            ));
        }
        Err(error) => {
            let reason = error.to_string();
            return Some(format!(
                "the composition is not valid YAML: {}",
                reason.lines().next().unwrap_or(&reason)
            ));
        }
    };
    entry_list_problem(&entries, "")
}

fn entry_list_problem(entries: &[ProfileEntry], at: &str) -> Option<String> {
    for (index, entry) in entries.iter().enumerate() {
        let label = if at.is_empty() {
            format!("row {}", index + 1)
        } else {
            format!("{at} row {}", index + 1)
        };
        if entry.name().is_none_or(str::is_empty) {
            return Some(format!(
                "{label} names no plugin (a \"name\" string is required)"
            ));
        }
        if matches!(entry.group(), Some(ProfileNode::Bool(true))) {
            let Some(nested) = entry.config().and_then(ProfileNode::as_sequence) else {
                return Some(format!("group {label} must hold a list of plugin rows"));
            };
            let mut nested_entries = Vec::with_capacity(nested.len());
            for row in nested {
                let Some(fields) = row.as_mapping() else {
                    return Some(format!(
                        "{label} row {} is not a plugin row (expected a map with a \"name\")",
                        nested_entries.len() + 1
                    ));
                };
                nested_entries.push(ProfileEntry::from_fields(fields.clone()));
            }
            if let Some(problem) = entry_list_problem(&nested_entries, &label) {
                return Some(problem);
            }
        }
    }
    None
}

fn absolute_root(path: &str) -> anyhow::Result<PathBuf> {
    let expanded = expand_home_path(path)?;
    if expanded.is_absolute() {
        Ok(expanded.clean())
    } else {
        Ok(std::env::current_dir()?.join(expanded).clean())
    }
}
