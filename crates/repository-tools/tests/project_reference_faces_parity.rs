//! Neutral, matching, opposite, solution-root, inherited-face, and JSONC fixtures.

use seekdeep_repository_tools::project_reference_faces::collect_project_reference_face_violations;
use tempfile::TempDir;

fn write_json(root: &TempDir, relative: &str, value: serde_json::Value) {
    let path = root.path().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
    drop(value);
    std::fs::write(path, source).unwrap();
}

fn workspace_fixture(host: &[&str], client: &[&str]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write_json(&root, "tsconfig.base.json", serde_json::json!({}));
    write_json(
        &root,
        "tsconfig.base.client.json",
        serde_json::json!({ "extends": "./tsconfig.base.json" }),
    );
    write_json(
        &root,
        "packages/core/shared/package.json",
        serde_json::json!({ "name": "@seekdeep-ai/seekdeep-shared" }),
    );
    write_json(
        &root,
        "packages/core/shared/tsconfig.json",
        serde_json::json!({ "extends": "../../../tsconfig.base.json", "references": [] }),
    );
    write_json(
        &root,
        "packages/api/split/package.json",
        serde_json::json!({ "name": "@seekdeep-ai/seekdeep-split" }),
    );
    write_json(
        &root,
        "packages/api/split/tsconfig.json",
        serde_json::json!({
            "files": [],
            "references": [
                { "path": "./tsconfig.host.json" },
                { "path": "./tsconfig.client.json" },
            ],
        }),
    );
    write_json(
        &root,
        "packages/api/split/tsconfig.host.json",
        serde_json::json!({ "references": [{ "path": "../../core/shared" }] }),
    );
    write_json(
        &root,
        "packages/api/split/tsconfig.client.json",
        serde_json::json!({ "references": [{ "path": "../../core/shared" }] }),
    );
    write_json(
        &root,
        "tsconfig.host.json",
        serde_json::json!({ "references": host.iter().map(|path| serde_json::json!({ "path": path })).collect::<Vec<_>>() }),
    );
    write_json(
        &root,
        "tsconfig.client.json",
        serde_json::json!({ "references": client.iter().map(|path| serde_json::json!({ "path": path })).collect::<Vec<_>>() }),
    );
    root
}

#[test]
fn neutral_projects_and_matching_split_leaves_are_allowed() {
    let root = workspace_fixture(
        &[
            "./packages/core/shared",
            "./packages/api/split/tsconfig.host.json",
        ],
        &[
            "./packages/core/shared",
            "./packages/api/split/tsconfig.client.json",
        ],
    );
    assert!(
        collect_project_reference_face_violations(root.path())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn opposite_leaf_and_solution_root_are_rejected_exactly() {
    let root = workspace_fixture(
        &[
            "./packages/api/split/tsconfig.host.json",
            "./packages/api/split/tsconfig.client.json",
        ],
        &["./packages/api/split"],
    );
    assert_eq!(
        collect_project_reference_face_violations(root.path()).unwrap(),
        [
            "tsconfig.client.json: Project Reference \"./packages/api/split\" enters split project packages/api/split from a Client config; reference \"packages/api/split/tsconfig.client.json\" instead",
            "tsconfig.host.json: Project Reference \"./packages/api/split/tsconfig.client.json\" enters split project packages/api/split from a Host config; reference \"packages/api/split/tsconfig.host.json\" instead",
        ]
    );
}

#[test]
fn inherited_referencing_face_flows_through_reachable_graph() {
    let root = workspace_fixture(
        &["./packages/core/host-consumer"],
        &["./packages/core/client-consumer"],
    );
    write_json(
        &root,
        "packages/core/host-consumer/package.json",
        serde_json::json!({ "name": "@seekdeep-ai/seekdeep-host-consumer" }),
    );
    write_json(
        &root,
        "packages/core/host-consumer/tsconfig.json",
        serde_json::json!({
            "extends": "../../../tsconfig.base.json",
            "references": [{ "path": "../../api/split/tsconfig.client.json" }],
        }),
    );
    write_json(
        &root,
        "packages/core/client-consumer/package.json",
        serde_json::json!({ "name": "@seekdeep-ai/seekdeep-client-consumer" }),
    );
    write_json(
        &root,
        "packages/core/client-consumer/tsconfig.json",
        serde_json::json!({
            "extends": "../../../tsconfig.base.client.json",
            "references": [{ "path": "../../api/split/tsconfig.host.json" }],
        }),
    );
    assert_eq!(
        collect_project_reference_face_violations(root.path()).unwrap(),
        [
            "packages/core/client-consumer/tsconfig.json: Project Reference \"../../api/split/tsconfig.host.json\" enters split project packages/api/split from a Client config; reference \"packages/api/split/tsconfig.client.json\" instead",
            "packages/core/host-consumer/tsconfig.json: Project Reference \"../../api/split/tsconfig.client.json\" enters split project packages/api/split from a Host config; reference \"packages/api/split/tsconfig.host.json\" instead",
        ]
    );
}

#[test]
fn jsonc_comments_and_trailing_commas_are_accepted() {
    let root = workspace_fixture(&["./packages/core/shared"], &[]);
    std::fs::write(
        root.path().join("tsconfig.host.json"),
        "{ // aggregate\n  \"references\": [{ \"path\": \"./packages/core/shared\", }],\n}\n",
    )
    .unwrap();
    assert!(
        collect_project_reference_face_violations(root.path())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn live_compatibility_project_graph_preserves_faces() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_eq!(
        collect_project_reference_face_violations(&root).unwrap(),
        Vec::<String>::new()
    );
}
