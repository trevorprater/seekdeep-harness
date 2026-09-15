//! Published Node bootstrap against the exact pinned source installation script.

use std::{path::PathBuf, process::Command};

#[test]
fn bundled_rust_installer_preserves_source_paths_permissions_errors_and_exports() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/source.json")).unwrap();
    let snapshot = include_str!("../../../SOURCE_SNAPSHOT");
    assert_eq!(
        snapshot
            .lines()
            .find_map(|line| line.strip_prefix("commit="))
            .unwrap(),
        fixture["sourceCommit"].as_str().unwrap()
    );
    let blob = Command::new("git")
        .arg("hash-object")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/source.mjs"))
        .output()
        .unwrap();
    assert!(blob.status.success());
    assert_eq!(
        String::from_utf8_lossy(&blob.stdout).trim(),
        fixture["sourceBlob"].as_str().unwrap()
    );
    let directory = tempfile::tempdir().unwrap();
    let runner = directory.path().join("parity.mjs");
    std::fs::write(&runner, include_str!("postinstall-parity.mjs")).unwrap();
    let output = Command::new("node")
        .arg(&runner)
        .arg(root.join(fixture["sourcePath"].as_str().unwrap()))
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/source.mjs"))
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
