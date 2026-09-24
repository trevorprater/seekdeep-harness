//! Closed, missing-peer chain, optional, external, optional-dependency, and cycle fixtures.

use seekdeep_repository_tools::runtime_closure::inspect_runtime_closure;
use tempfile::TempDir;

fn manifest(root: &TempDir, relative: &str, value: serde_json::Value) {
    let path = root.path().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
    drop(value);
    std::fs::write(path, source).unwrap();
}

#[test]
fn runtime_supplied_required_peers_close_the_reachable_graph() {
    let root = tempfile::tempdir().unwrap();
    manifest(
        &root,
        "runtime.json",
        serde_json::json!({
            "name": "runtime",
            "dependencies": { "a": "workspace:^", "b": "workspace:^" },
        }),
    );
    manifest(
        &root,
        "packages/core/a/package.json",
        serde_json::json!({
            "name": "a",
            "dependencies": { "c": "workspace:^" },
            "peerDependencies": { "b": "workspace:^" },
        }),
    );
    manifest(
        &root,
        "packages/core/b/package.json",
        serde_json::json!({ "name": "b" }),
    );
    manifest(
        &root,
        "packages/core/c/package.json",
        serde_json::json!({ "name": "c" }),
    );
    let report = inspect_runtime_closure(root.path(), &root.path().join("runtime.json")).unwrap();
    assert_eq!(report.packages, 3);
    assert!(report.failures.is_empty());
}

#[test]
fn missing_peer_reports_the_shortest_breadth_first_parent_chain() {
    let root = tempfile::tempdir().unwrap();
    manifest(
        &root,
        "runtime.json",
        serde_json::json!({ "name": "runtime", "dependencies": { "a": "workspace:^" } }),
    );
    manifest(
        &root,
        "packages/core/a/package.json",
        serde_json::json!({ "name": "a", "dependencies": { "c": "workspace:^" } }),
    );
    manifest(
        &root,
        "packages/core/c/package.json",
        serde_json::json!({ "name": "c", "peerDependencies": { "b": "workspace:^" } }),
    );
    manifest(
        &root,
        "packages/core/b/package.json",
        serde_json::json!({ "name": "b" }),
    );
    let report = inspect_runtime_closure(root.path(), &root.path().join("runtime.json")).unwrap();
    assert_eq!(report.failures, ["runtime -> a -> c -> b"]);
}

#[test]
fn optional_and_external_peers_are_ignored() {
    let root = tempfile::tempdir().unwrap();
    manifest(
        &root,
        "runtime.json",
        serde_json::json!({ "dependencies": { "a": "workspace:^" } }),
    );
    manifest(
        &root,
        "packages/core/a/package.json",
        serde_json::json!({
            "name": "a",
            "peerDependencies": { "optional": "workspace:^", "external": "1" },
            "peerDependenciesMeta": { "optional": { "optional": true } },
        }),
    );
    manifest(
        &root,
        "packages/core/optional/package.json",
        serde_json::json!({ "name": "optional" }),
    );
    let report = inspect_runtime_closure(root.path(), &root.path().join("runtime.json")).unwrap();
    assert!(report.failures.is_empty());
}

#[test]
fn optional_dependencies_are_traversed_and_cycles_are_deduplicated() {
    let root = tempfile::tempdir().unwrap();
    manifest(
        &root,
        "runtime.json",
        serde_json::json!({ "dependencies": { "a": "workspace:^" } }),
    );
    manifest(
        &root,
        "packages/core/a/package.json",
        serde_json::json!({ "name": "a", "optionalDependencies": { "b": "workspace:^" } }),
    );
    manifest(
        &root,
        "packages/core/b/package.json",
        serde_json::json!({ "name": "b", "dependencies": { "a": "workspace:^" } }),
    );
    let report = inspect_runtime_closure(root.path(), &root.path().join("runtime.json")).unwrap();
    assert_eq!(report.packages, 2);
    assert!(report.failures.is_empty());
}
