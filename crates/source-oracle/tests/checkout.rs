//! Oracle relocation, revision admission, and immutable source-object access.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_source_oracle::{SourceOracle, SourceRevision, source_location};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Oracle fixture",
            "-c",
            "user.email=oracle@example.invalid",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn source(root: &Path) -> String {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(
        root.join("nested/source 中文.ts"),
        "export const pinned = true;\n",
    )
    .unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "Pinned source"]);
    git(root, &["rev-parse", "HEAD"])
}

fn snapshot(root: &Path, source: &Path, revision: &str) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("SOURCE_SNAPSHOT"),
        format!("repository={}\ncommit={revision}\n", source.display()),
    )
    .unwrap();
}

#[test]
fn relocated_adjacent_checkout_precedes_the_recorded_path_and_explicit_override_wins() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("seekdeep-harness");
    let adjacent = temporary.path().join("deepseek-harness");
    let revision = source(&adjacent);
    let recorded = temporary.path().join("unavailable-original-location");
    snapshot(&root, &recorded, &revision);
    let oracle = SourceOracle::open_with_root(&root, None).unwrap();
    assert_eq!(oracle.root(), dunce::canonicalize(&adjacent).unwrap());

    let relocated = temporary.path().join("explicit source");
    std::fs::rename(&adjacent, &relocated).unwrap();
    assert_eq!(source_location(&root, None).unwrap(), recorded);
    assert!(SourceOracle::open_with_root(&root, None).is_err());
    let oracle = SourceOracle::open_with_root(&root, Some(&relocated)).unwrap();
    assert_eq!(oracle.root(), dunce::canonicalize(&relocated).unwrap());
}

#[test]
fn canonical_oracle_paths_support_plain_node_modules_and_entrypoints() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("target");
    let original = temporary.path().join("source with spaces 中文");
    let revision = source(&original);
    snapshot(&root, &original, &revision);
    std::fs::write(original.join("value.cjs"), "module.exports = 73;\n").unwrap();
    std::fs::write(
        original.join("main.cjs"),
        "process.stdout.write(String(require('./value.cjs')));\n",
    )
    .unwrap();
    let oracle = SourceOracle::open_with_root(&root, Some(&original)).unwrap();
    let output = Command::new("node")
        .arg(oracle.root().join("main.cjs"))
        .current_dir(oracle.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"73");
}

#[test]
fn original_git_objects_ignore_worktree_changes_and_untracked_files() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("target");
    let original = temporary.path().join("original");
    let revision = source(&original);
    snapshot(&root, &original, &revision);
    std::fs::write(
        original.join("nested/source 中文.ts"),
        "wrong worktree text\n",
    )
    .unwrap();
    std::fs::write(original.join("untracked.ts"), "export {};\n").unwrap();
    let oracle = SourceOracle::open_with_root(&root, Some(&original)).unwrap();
    assert_eq!(
        oracle.read("nested/source 中文.ts").unwrap(),
        "export const pinned = true;\n"
    );
    assert_eq!(oracle.files().unwrap(), ["nested/source 中文.ts"]);
    assert!(oracle.paths().unwrap().contains("nested"));
    for path in [
        "",
        "../outside.ts",
        "nested/../../outside.ts",
        "/absolute.ts",
        "untracked.ts",
    ] {
        assert!(oracle.read(path).is_err(), "{path}");
    }
}

#[test]
fn mismatched_or_malformed_revisions_fail_before_source_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("target");
    let original = temporary.path().join("original");
    let revision = source(&original);
    snapshot(&root, &original, &revision);
    assert_eq!(SourceRevision::read(&root).unwrap().as_str(), revision);
    git(
        &original,
        &["commit", "--quiet", "--allow-empty", "-m", "Source drift"],
    );
    let error = SourceOracle::open_with_root(&root, Some(&original))
        .err()
        .unwrap();
    assert!(error.to_string().contains(&revision));
    for revision in [
        "",
        "short",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        snapshot(&root, &original, revision);
        assert!(SourceRevision::read(&root).is_err());
    }
    for duplicate in [&revision, &"a".repeat(40)] {
        snapshot(&root, &original, &format!("{revision}\ncommit={duplicate}"));
        let error = SourceRevision::read(&root).unwrap_err();
        assert!(error.to_string().contains("exactly one commit"));
    }
    assert_eq!(
        source_location(&PathBuf::from("absent-target"), Some(&original)).unwrap(),
        original
    );
}
