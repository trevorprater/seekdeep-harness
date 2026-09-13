//! Exact blob hashing, persistent snapshots, index bytes, absence, and invalid repository failures.

use std::process::Command;

use seekdeep_repository_tools::translation_pairing_git::{
    git_blob_hash, read_git_index_blob, store_git_blob,
};

fn init_repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(root.path())
            .env("GIT_DEFAULT_HASH", "sha1")
            .status()
            .unwrap()
            .success()
    );
    root
}

#[test]
fn exact_uncommitted_bytes_are_stored_pinned_and_recoverable() {
    let root = init_repository();
    let content = b"uncommitted\n\xff";
    let object_id = store_git_blob(root.path(), content).unwrap();
    assert_eq!(object_id, git_blob_hash(content));
    let reference = format!("refs/seekdeep/translation-pairing/snapshots/{object_id}");
    let resolved = Command::new("git")
        .arg("-C")
        .arg(root.path())
        .args(["rev-parse", &reference])
        .output()
        .unwrap();
    assert!(resolved.status.success());
    assert_eq!(String::from_utf8_lossy(&resolved.stdout).trim(), object_id);
    let recovered = Command::new("git")
        .arg("-C")
        .arg(root.path())
        .args(["cat-file", "-p", &object_id])
        .output()
        .unwrap();
    assert_eq!(recovered.stdout, content);
}

#[test]
fn staged_bytes_are_independent_of_working_tree_bytes() {
    let root = init_repository();
    let owner = root.path().join("owner.md");
    std::fs::write(&owner, "staged").unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(["add", "owner.md"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(owner, "unstaged").unwrap();
    let indexed = read_git_index_blob(root.path(), "owner.md")
        .unwrap()
        .unwrap();
    assert_eq!(indexed.content, b"staged");
    assert_eq!(indexed.object_id, git_blob_hash(b"staged"));
    assert_eq!(read_git_index_blob(root.path(), "absent.md").unwrap(), None);
}

#[test]
fn storing_outside_a_repository_fails_before_a_record_can_reference_it() {
    let root = tempfile::tempdir().unwrap();
    let error = store_git_blob(root.path(), b"snapshot")
        .unwrap_err()
        .to_string();
    assert!(error.contains("git hash-object -w --stdin failed"));
}

#[test]
fn git_cannot_start_failure_is_clear() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "git_cannot_start_helper"])
        .env("SEEKDEEP_TEST_GIT_CANNOT_START", "1")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn git_cannot_start_helper() {
    if std::env::var_os("SEEKDEEP_TEST_GIT_CANNOT_START").is_none() {
        return;
    }
    let error = store_git_blob(std::path::Path::new("."), b"snapshot")
        .unwrap_err()
        .to_string();
    assert!(error.contains("git hash-object -w --stdin failed"));
}

#[test]
fn non_sha1_object_format_is_rejected_when_supported() {
    let root = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet", "--object-format=sha256"])
        .arg(root.path())
        .status()
        .unwrap();
    if !status.success() {
        return;
    }
    let error = store_git_blob(root.path(), b"snapshot")
        .unwrap_err()
        .to_string();
    assert!(error.contains("returned unexpected object ID"));
}
