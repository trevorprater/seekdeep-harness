//! Replay-aware config paths, required overlays, and provenance config dumps.

use std::{
    fs,
    path::{Path, PathBuf},
};

use path_clean::PathClean as _;
use seekdeep_loader::profile_patch::{
    ProfileEntry, ProfilePatch, apply_entry_patches_with_warning_sink, parse_entry_list_yaml,
    parse_patch_list_yaml, render_entry_list_yaml,
};

/// One ordered overlay with its dump-comment label.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigDumpLayer {
    /// Source name printed in provenance comments.
    pub label: String,
    /// Parsed patch entries.
    pub patches: Vec<ProfilePatch>,
}

/// Resolves a requested config path and swaps `cordis.yml`/`cordis.yaml` in replay mode.
///
/// # Errors
///
/// Returns current-directory lookup failures when `cwd` is relative.
pub fn resolve_config_path(
    config_path: &Path,
    snapshot_mode: Option<&str>,
    cwd: &Path,
) -> anyhow::Result<PathBuf> {
    let cwd = if cwd.is_absolute() {
        cwd.to_owned()
    } else {
        std::env::current_dir()?.join(cwd)
    };
    let absolute = if config_path.is_absolute() {
        config_path.to_owned()
    } else {
        cwd.join(config_path)
    }
    .clean();
    if snapshot_mode != Some("replay") {
        return Ok(absolute);
    }
    let Some(name) = absolute.file_name().and_then(std::ffi::OsStr::to_str) else {
        return Ok(absolute);
    };
    let replay_name = if let Some(prefix) = name.strip_suffix("cordis.yml") {
        format!("{prefix}cordis.snapshot.yml")
    } else if let Some(prefix) = name.strip_suffix("cordis.yaml") {
        format!("{prefix}cordis.snapshot.yml")
    } else {
        name.to_owned()
    };
    Ok(absolute.with_file_name(replay_name).clean())
}

fn parse_patch_list(
    bin_name: &str,
    path: &Path,
    source: &str,
    label: &str,
) -> anyhow::Result<Vec<ProfilePatch>> {
    parse_patch_list_yaml(source).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to parse {label} {}: {error}",
            path.display()
        )
    })
}

/// Loads one required overlay patch file.
///
/// # Errors
///
/// A missing, unreadable, unparsable, or malformed overlay fails loudly.
pub fn load_overlay_patches(bin_name: &str, path: &Path) -> anyhow::Result<Vec<ProfilePatch>> {
    let source = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to read overlay {}: {error}",
            path.display()
        )
    })?;
    parse_patch_list(bin_name, path, &source, "overlay")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Provenance {
    origin: String,
    patched_by: Vec<String>,
}

fn render_group(entries: &[ProfileEntry]) -> anyhow::Result<String> {
    let rendered = render_entry_list_yaml(entries)?;
    Ok(rendered
        .strip_prefix("---\n")
        .unwrap_or(&rendered)
        .trim_end()
        .to_owned())
}

fn grouped_dump(entries: &[ProfileEntry], provenance: &[Provenance]) -> anyhow::Result<String> {
    let mut lines = Vec::new();
    let mut current_label: Option<String> = None;
    let mut group = Vec::new();
    for (entry, provenance) in entries.iter().zip(provenance) {
        let label = if provenance.patched_by.is_empty() {
            provenance.origin.clone()
        } else {
            format!(
                "{}, patched by {}",
                provenance.origin,
                provenance.patched_by.join(", ")
            )
        };
        if current_label
            .as_deref()
            .is_some_and(|current| current != label)
        {
            let current = current_label.take().expect("label exists");
            lines.push(format!("# == {current}"));
            lines.push(render_group(&group)?);
            group.clear();
        }
        current_label = Some(label);
        group.push(entry.clone());
    }
    if let Some(current) = current_label
        && !group.is_empty()
    {
        lines.push(format!("# == {current}"));
        lines.push(render_group(&group)?);
    }
    Ok(format!("{}\n", lines.join("\n")))
}

/// Renders the effective config with contiguous provenance comment groups.
///
/// # Errors
///
/// Returns base read/parse/shape, patch composition, or YAML rendering failures.
pub fn render_config_dump(
    bin_name: &str,
    absolute_config_path: &Path,
    layers: &[ConfigDumpLayer],
    mut warn: impl FnMut(String),
) -> anyhow::Result<String> {
    let source = fs::read_to_string(absolute_config_path).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to read config {}: {error}",
            absolute_config_path.display()
        )
    })?;
    let base = parse_entry_list_yaml(&source).map_err(|error| {
        anyhow::anyhow!(
            "{bin_name}: failed to parse config {}: {error}",
            absolute_config_path.display()
        )
    })?;
    let base_label = absolute_config_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let mut previous = base.clone();
    let mut previous_warnings = Vec::new();
    let mut provenance = base
        .iter()
        .map(|_| Provenance {
            origin: base_label.clone(),
            patched_by: Vec::new(),
        })
        .collect::<Vec<_>>();
    let mut composed = base.clone();
    for count in 1..=layers.len() {
        let layer = &layers[count - 1];
        let flattened = layers[..count]
            .iter()
            .flat_map(|layer| layer.patches.iter().cloned())
            .collect::<Vec<_>>();
        let mut warnings = Vec::new();
        composed = apply_entry_patches_with_warning_sink(&base, &flattened, |warning| {
            warnings.push(warning.to_string());
        })?;
        for warning in &warnings[previous_warnings.len()..] {
            warn(format!("{bin_name}: [{}] {warning}", layer.label));
        }
        for index in 0..composed.len() {
            if index >= previous.len() {
                provenance.push(Provenance {
                    origin: layer.label.clone(),
                    patched_by: Vec::new(),
                });
            } else if composed[index] != previous[index]
                && let Some(record) = provenance.get_mut(index)
            {
                record.patched_by.push(layer.label.clone());
            }
        }
        previous.clone_from(&composed);
        previous_warnings = warnings;
    }
    grouped_dump(&composed, &provenance)
}

/// Renders a config dump and prints skipped-patch warnings to standard error.
///
/// # Errors
///
/// Returns the same failures as [`render_config_dump`].
pub fn render_config_dump_stderr(
    bin_name: &str,
    absolute_config_path: &Path,
    layers: &[ConfigDumpLayer],
) -> anyhow::Result<String> {
    render_config_dump(bin_name, absolute_config_path, layers, |line| {
        eprintln!("{line}");
    })
}
