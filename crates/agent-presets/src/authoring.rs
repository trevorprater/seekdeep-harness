//! Constrained reading, copying, and deletion of locally authored presets.

use std::path::{Path, PathBuf};

use path_clean::PathClean as _;
use seekdeep_util::{
    atomic_write::{WriteFileAtomicOptions, write_file_atomic},
    home_paths::expand_home_path,
};
use thiserror::Error;

use crate::{
    metadata::{METADATA_FILE, PresetMetadata, render_preset_metadata},
    preset::{AgentPreset, PresetRoot, PresetTrust, valid_preset_id},
};

/// A preset id cannot be used as a contained directory name.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "agent-presets: preset id {quoted} must match /^[a-z0-9][a-z0-9-]*$/ — the id is a directory name, so anything else could escape the preset root"
)]
pub struct InvalidPresetIdError {
    /// Rejected identity.
    pub preset_id: String,
    quoted: String,
}

impl InvalidPresetIdError {
    fn new(preset_id: impl Into<String>) -> Self {
        let preset_id = preset_id.into();
        let quoted = serde_json::to_string(&preset_id).unwrap_or_else(|_| "\"?\"".to_owned());
        Self { preset_id, quoted }
    }
}

/// A copy target is already occupied.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "agent-presets: preset \"{preset_id}\" already exists — a copy never overwrites; delete the existing preset first or choose another id"
)]
pub struct PresetExistsError {
    /// Occupied identity.
    pub preset_id: String,
}

/// No writable root owns the requested authoring action.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("agent-presets: preset \"{preset_id}\" cannot be written: {reason}")]
pub struct PresetNotWritableError {
    /// Identity the caller tried to change.
    pub preset_id: String,
    /// Stable refusal reason.
    pub reason: String,
}

impl PresetNotWritableError {
    /// Creates one stable authoring refusal.
    #[must_use]
    pub fn new(preset_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            preset_id: preset_id.into(),
            reason: reason.into(),
        }
    }
}

/// Resolves the first user-trust root used by authoring.
///
/// # Errors
///
/// Returns when no writable root exists or the path cannot be expanded.
pub fn writable_root(roots: &[PresetRoot]) -> anyhow::Result<PathBuf> {
    let root = roots
        .iter()
        .find(|root| root.trust == PresetTrust::User)
        .ok_or_else(|| {
            PresetNotWritableError::new(
                "",
                "this deployment configures no user-writable preset root",
            )
        })?;
    let path = expand_home_path(&root.path)?;
    Ok(if path.is_absolute() {
        path.clean()
    } else {
        std::env::current_dir()?.join(path).clean()
    })
}

/// Reads one preset's composition exactly as stored.
///
/// # Errors
///
/// Returns the filesystem read failure.
pub async fn read_composition(preset: &AgentPreset) -> anyhow::Result<String> {
    Ok(tokio::fs::read_to_string(&preset.path).await?)
}

/// Copies a complete preset directory into the writable root without overwriting.
///
/// # Errors
///
/// Returns invalid-id, unavailable-root, occupied-target, copy, permission,
/// metadata, or rollback failures. A failed operation leaves no target directory.
pub async fn copy_composition(
    roots: &[PresetRoot],
    source: &AgentPreset,
    id: &str,
    name: Option<&str>,
) -> anyhow::Result<PathBuf> {
    if !valid_preset_id(id) {
        return Err(InvalidPresetIdError::new(id).into());
    }
    let root = writable_root(roots)?;
    let destination = root.join(id);
    if occupied(&destination).await {
        return Err(PresetExistsError {
            preset_id: id.to_owned(),
        }
        .into());
    }
    let source_directory = source
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("preset composition has no parent directory"))?;
    let operation = async {
        tokio::fs::create_dir_all(&root).await?;
        set_mode(&root, 0o700).await?;
        copy_tree(source_directory, &destination).await?;
        tighten_modes(&destination).await?;
        let rendered = render_preset_metadata(&PresetMetadata {
            name: name.map(ToOwned::to_owned),
            description: source.description.clone(),
            order: None,
        });
        let metadata = destination.join(METADATA_FILE);
        if let Some(rendered) = rendered {
            write_file_atomic(
                metadata,
                rendered.as_bytes(),
                WriteFileAtomicOptions {
                    mode: 0o600,
                    dir_mode: Some(0o700),
                },
            )
            .await?;
        } else if let Err(error) = tokio::fs::remove_file(metadata).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = operation {
        match tokio::fs::remove_dir_all(&destination).await {
            Ok(()) => return Err(error),
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => return Err(error),
            Err(cleanup) => return Err(cleanup.into()),
        }
    }
    Ok(destination)
}

/// Deletes one preset only when a user root owns its exact directory.
///
/// # Errors
///
/// Returns trust, root ownership, or filesystem failures.
pub async fn delete_composition(roots: &[PresetRoot], preset: &AgentPreset) -> anyhow::Result<()> {
    if preset.trust != PresetTrust::User {
        return Err(PresetNotWritableError::new(&preset.id, "it ships with the deployment").into());
    }
    let directory = writable_root(roots)?.join(&preset.id);
    if !preset.path.is_absolute() || !preset.path.starts_with(&directory) {
        return Err(PresetNotWritableError::new(
            &preset.id,
            "it does not live under the writable preset root",
        )
        .into());
    }
    match tokio::fs::remove_dir_all(directory).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn occupied(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

fn copy_tree<'a>(
    source: &'a Path,
    destination: &'a Path,
) -> futures::future::BoxFuture<'a, anyhow::Result<()>> {
    Box::pin(async move {
        let metadata = tokio::fs::metadata(source).await?;
        if metadata.is_dir() {
            tokio::fs::create_dir(destination).await?;
            let mut entries = tokio::fs::read_dir(source).await?;
            while let Some(entry) = entries.next_entry().await? {
                copy_tree(&entry.path(), &destination.join(entry.file_name())).await?;
            }
        } else if metadata.is_file() {
            tokio::fs::copy(source, destination).await?;
        } else {
            anyhow::bail!("unsupported preset entry type at {}", source.display());
        }
        Ok(())
    })
}

fn tighten_modes(path: &Path) -> futures::future::BoxFuture<'_, std::io::Result<()>> {
    Box::pin(async move {
        let metadata = tokio::fs::metadata(path).await?;
        if metadata.is_dir() {
            set_mode(path, 0o700).await?;
            let mut entries = tokio::fs::read_dir(path).await?;
            while let Some(entry) = entries.next_entry().await? {
                tighten_modes(&entry.path()).await?;
            }
        } else {
            let executable = owner_executable(&metadata);
            set_mode(path, if executable { 0o700 } else { 0o600 }).await?;
        }
        Ok(())
    })
}

#[cfg(unix)]
fn owner_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn owner_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}
