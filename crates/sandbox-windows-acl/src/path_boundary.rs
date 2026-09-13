//! Canonical directory-boundary checks separating workspace and temp capabilities.

use std::path::{Path, PathBuf};

fn canonical(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(std::fs::canonicalize(path)?)
}

fn contains_directory(root: &Path, candidate: &Path) -> anyhow::Result<bool> {
    let root = canonical(root)?;
    let candidate = canonical(candidate)?;
    Ok(candidate == root || candidate.starts_with(root))
}

/// Rejects a temp parent equal to or inside the workspace.
///
/// # Errors
///
/// Returns canonicalization failures or the exact boundary diagnostic.
pub fn assert_temp_root_outside_workspace(
    workspace_root: &Path,
    temp_root: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !contains_directory(workspace_root, temp_root)?,
        "Windows ACL temp root must be outside the workspace: workspace={}; temp={}",
        workspace_root.display(),
        temp_root.display()
    );
    Ok(())
}

/// Rejects overlap in either direction between private temp and any writable tree.
///
/// # Errors
///
/// Returns canonicalization failures or the exact boundary diagnostic.
pub fn assert_private_temp_disjoint(
    writable_dirs: &[PathBuf],
    temp_dir: &Path,
) -> anyhow::Result<()> {
    for writable_dir in writable_dirs {
        anyhow::ensure!(
            !contains_directory(writable_dir, temp_dir)?
                && !contains_directory(temp_dir, writable_dir)?,
            "AclSandbox private temp directory must be disjoint from writable directories: writable={}; temp={}",
            writable_dir.display(),
            temp_dir.display()
        );
    }
    Ok(())
}
