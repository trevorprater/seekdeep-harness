//! Built WASM callbacks against the pinned source and a real Chromium document.

#![cfg(not(target_arch = "wasm32"))]

use std::{path::PathBuf, process::Command};

#[test]
#[ignore = "Run after docs:prepare; needs the pinned oracle's Playwright installation."]
fn built_callbacks_match_source_and_sidebar_ownership_unwinds() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let snapshot = std::fs::read_to_string(root.join("SOURCE_SNAPSHOT")).unwrap();
    let source = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("repository="))
        .unwrap();
    let output = Command::new("node")
        .arg(root.join("crates/docs-site-runtime/tests/built-parity.mjs"))
        .arg(&root)
        .arg(source)
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
