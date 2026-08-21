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

/// Resolves a workspace-relative spelling for the Host path-opening boundary.
///
/// Absolute POSIX, drive-rooted Windows, and UNC paths pass through verbatim.
/// Without a non-empty workspace root, relative paths also pass through.
#[must_use]
pub fn resolve_workspace_path(cwd: Option<&str>, path: &str) -> String {
    if is_host_absolute(path) {
        return path.to_owned();
    }
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return path.to_owned();
    };
    let base = cwd.trim_end_matches(['/', '\\']);
    let relative = path.trim_start_matches(['/', '\\']);
    format!("{base}/{relative}")
}

fn is_host_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with(r"\\")
        || matches!(
            path.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic() && matches!(separator, b'/' | b'\\')
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_workspace_resolution_is_lexical_and_cross_platform() {
        for absolute in [
            "/tmp/file.rs",
            r"C:\file.rs",
            "z:/file.rs",
            r"\\server\share\file.rs",
        ] {
            assert_eq!(
                resolve_workspace_path(Some("/workspace"), absolute),
                absolute
            );
        }
        assert_eq!(resolve_workspace_path(None, "src/lib.rs"), "src/lib.rs");
        assert_eq!(resolve_workspace_path(Some(""), "src/lib.rs"), "src/lib.rs");
        assert_eq!(
            resolve_workspace_path(Some("/workspace///"), "src/lib.rs"),
            "/workspace/src/lib.rs"
        );
        assert_eq!(
            resolve_workspace_path(Some(r"C:\workspace\\"), r"\src\lib.rs"),
            "C:\\workspace/src\\lib.rs"
        );
        assert_eq!(resolve_workspace_path(Some("///"), "x"), "/x");
    }
}
