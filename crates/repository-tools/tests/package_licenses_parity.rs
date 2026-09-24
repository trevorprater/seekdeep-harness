//! Product-renamed source-oracle coverage for first-party package licenses.

use seekdeep_repository_tools::package_licenses::inspect_seekdeep_package_licenses;
use serde_json::json;

fn write_manifest(root: &std::path::Path, file: &str, manifest: &serde_json::Value) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut content = serde_json::to_string_pretty(manifest).unwrap();
    content.push('\n');
    std::fs::write(path, content).unwrap();
}

fn workspace() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_manifest(
        root.path(),
        "package.json",
        &json!({
            "name":"@seekdeep-ai/seekdeep-root",
            "license":"MIT",
            "workspaces":["apps/*","packages/*/*","vendor/*"]
        }),
    );
    root
}

#[test]
fn checks_root_cli_and_prefixed_names_while_ignoring_other_families() {
    let root = workspace();
    write_manifest(
        root.path(),
        "apps/cli/package.json",
        &json!({"name":"@seekdeep-ai/seekdeep","license":"MIT"}),
    );
    write_manifest(
        root.path(),
        "packages/core/agent/package.json",
        &json!({"name":"@seekdeep-ai/seekdeep-agent","license":"BSD-3-Clause"}),
    );
    write_manifest(
        root.path(),
        "vendor/cordis/package.json",
        &json!({"name":"@seekdeep-ai/cordis","license":"BSD-3-Clause"}),
    );
    let report = inspect_seekdeep_package_licenses(root.path()).unwrap();
    assert_eq!(report.package_count, 3);
    assert_eq!(
        report.failures,
        [
            "packages/core/agent/package.json: @seekdeep-ai/seekdeep-agent must declare \"license\": \"MIT\"; found \"BSD-3-Clause\"."
        ]
    );
}

#[test]
fn rejects_a_missing_license_declaration() {
    let root = workspace();
    write_manifest(
        root.path(),
        "packages/core/agent/package.json",
        &json!({"name":"@seekdeep-ai/seekdeep-agent"}),
    );
    assert_eq!(
        inspect_seekdeep_package_licenses(root.path())
            .unwrap()
            .failures,
        [
            "packages/core/agent/package.json: @seekdeep-ai/seekdeep-agent must declare \"license\": \"MIT\"; found undefined."
        ]
    );
}
