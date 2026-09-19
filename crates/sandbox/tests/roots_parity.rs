//! Canonical writable-root derivation parity.

use std::path::PathBuf;

use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode, canonical_path, writable_roots};
use tempfile::tempdir;

fn policy(mode: SandboxMode, root: PathBuf) -> SandboxExecutionPolicy {
    SandboxExecutionPolicy {
        mode,
        workspace_root: root,
        session_id: None,
    }
}

#[test]
fn canonical_path_resolves_existing_and_retains_missing_spelling() {
    let directory = tempdir().expect("temporary directory");
    assert_eq!(
        canonical_path(directory.path()),
        std::fs::canonicalize(directory.path()).unwrap()
    );
    assert_eq!(
        canonical_path("/does/not/exist/anywhere-xyz"),
        PathBuf::from("/does/not/exist/anywhere-xyz")
    );
}

#[test]
fn writable_roots_match_mode_canonicalization_order_and_deduplication() {
    let workspace = tempdir().expect("workspace");
    assert!(writable_roots(&policy(SandboxMode::ReadOnly, workspace.path().into())).is_empty());
    let roots = writable_roots(&policy(
        SandboxMode::WorkspaceWrite,
        workspace.path().into(),
    ));
    assert!(roots.contains(&std::fs::canonicalize(workspace.path()).unwrap()));
    assert!(roots.contains(&canonical_path("/tmp")));
    assert!(roots.contains(&canonical_path(std::env::temp_dir())));
    let unique = roots.iter().collect::<std::collections::HashSet<_>>();
    assert_eq!(unique.len(), roots.len());
}
