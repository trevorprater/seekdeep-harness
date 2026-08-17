//! Canonical workspace path identity.

use std::path::{Path, PathBuf};

/// Resolves symlinks, trailing separators, and parent segments using the host filesystem.
///
/// # Errors
///
/// Preserves the underlying filesystem failure for absent or inaccessible paths.
pub async fn realpath_normalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    tokio::fs::canonicalize(path).await
}
