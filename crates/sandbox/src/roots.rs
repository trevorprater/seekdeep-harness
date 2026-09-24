//! Canonical writable-root derivation shared by enforcement dialects.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::{SandboxExecutionPolicy, SandboxMode};

/// Resolves a root to filesystem identity, conservatively retaining a missing spelling.
#[must_use]
pub fn canonical_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Returns the canonical, ordered, deduplicated roots writable under a policy.
#[must_use]
pub fn writable_roots(policy: &SandboxExecutionPolicy) -> Vec<PathBuf> {
    if policy.mode != SandboxMode::WorkspaceWrite {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    [
        policy.workspace_root.clone(),
        PathBuf::from("/tmp"),
        std::env::temp_dir(),
    ]
    .into_iter()
    .map(canonical_path)
    .filter(|path| seen.insert(path.clone()))
    .collect()
}
