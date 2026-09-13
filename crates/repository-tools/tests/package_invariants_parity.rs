//! Publication metadata, parity ownership, generated marker, and live audit fixtures.

use seekdeep_repository_tools::package_invariants::collect_package_invariant_violations;

fn fixture(manifest: serde_json::Value, status: &str, generated: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("packages/core/probe");
    let target = root.path().join("crates/probe/src/invariant.rs");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.path().join("porting")).unwrap();
    std::fs::create_dir_all(root.path().join("crates/invariants/src/noop")).unwrap();
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    drop(manifest);
    std::fs::write(package.join("package.json"), manifest_bytes).unwrap();
    std::fs::write(
        &target,
        if generated {
            "// @generated\n"
        } else {
            "// hand-owned\n"
        },
    )
    .unwrap();
    std::fs::write(
        root.path().join("crates/invariants/src/noop/catalog.rs"),
        "",
    )
    .unwrap();
    std::fs::write(
        root.path().join("porting/parity.json"),
        serde_json::to_vec(&serde_json::json!({ "surfaces": [{
            "source": "packages/core/probe/src/invariant.ts",
            "status": status,
            "targets": ["crates/probe/src/invariant.rs"]
        }]}))
        .unwrap(),
    )
    .unwrap();
    root
}

fn valid_manifest() -> serde_json::Value {
    serde_json::json!({
        "name": "@seekdeep-ai/seekdeep-probe",
        "exports": { "./invariant": {
            "types": "./lib/types/invariant.d.ts",
            "default": "./lib/invariant.js"
        }},
        "files": ["lib/invariant.js"],
        "peerDependencies": { "@seekdeep-ai/seekdeep-invariants": "workspace:^" },
        "devDependencies": { "@seekdeep-ai/seekdeep-invariants": "workspace:^" }
    })
}

#[test]
fn valid_rust_owned_companion_conforms() {
    let root = fixture(valid_manifest(), "verified", false);
    assert!(
        collect_package_invariant_violations(root.path())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn publication_unverified_and_generated_failures_are_named() {
    let root = fixture(
        serde_json::json!({ "name": "@seekdeep-ai/seekdeep-probe" }),
        "pending",
        true,
    );
    let messages = collect_package_invariant_violations(root.path())
        .unwrap()
        .into_iter()
        .map(|violation| violation.message)
        .collect::<Vec<_>>();
    assert!(messages.iter().any(|message| message.contains("exports")));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("peerDependency"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("not verified"))
    );
}

#[test]
fn generated_verified_rust_target_is_rejected() {
    let root = fixture(valid_manifest(), "verified", true);
    assert!(
        collect_package_invariant_violations(root.path())
            .unwrap()
            .iter()
            .any(|violation| violation.message.contains("hand-owned"))
    );
}

#[test]
fn live_target_package_invariant_ownership_is_complete() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        collect_package_invariant_violations(&root)
            .unwrap()
            .is_empty()
    );
}
