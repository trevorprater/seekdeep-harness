//! Discovery, layered topology, group order, cycles, IDs, and labels.

use seekdeep_repository_tools::package_graph::{
    collect_package_graph, escape_mermaid_label, graph_node_id,
};
use tempfile::TempDir;

fn package(root: &TempDir, group: &str, leaf: &str, name: &str, peers: &[&str]) {
    let directory = root.path().join("packages").join(group).join(leaf);
    std::fs::create_dir_all(&directory).unwrap();
    let peer_dependencies = peers
        .iter()
        .map(|peer| {
            (
                (*peer).to_owned(),
                serde_json::Value::String("workspace:^".to_owned()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        directory.join("package.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name,
                "peerDependencies": peer_dependencies,
            }))
            .unwrap()
        ),
    )
    .unwrap();
}

#[test]
fn collection_filters_foreign_packages_and_preserves_dependency_layers() {
    let root = tempfile::tempdir().unwrap();
    package(&root, "zeta", "base", "@seekdeep-ai/seekdeep-base", &[]);
    package(
        &root,
        "alpha",
        "second",
        "@seekdeep-ai/seekdeep-second",
        &["@seekdeep-ai/seekdeep-base", "external"],
    );
    package(
        &root,
        "alpha",
        "third",
        "@seekdeep-ai/seekdeep-third",
        &["@seekdeep-ai/seekdeep-second"],
    );
    package(&root, "alpha", "foreign", "foreign-package", &[]);
    let graph = collect_package_graph(root.path(), &["alpha".to_owned()], "probe").unwrap();
    assert_eq!(
        graph
            .iter()
            .map(|node| node.short.as_str())
            .collect::<Vec<_>>(),
        ["base", "second", "third"]
    );
    assert_eq!(graph[1].dependencies, ["base"]);
    assert_eq!(graph[1].relative, "packages/alpha/second");
}

#[test]
fn caller_group_order_breaks_ties_within_one_ready_layer() {
    let root = tempfile::tempdir().unwrap();
    package(&root, "later", "a", "@seekdeep-ai/seekdeep-a", &[]);
    package(&root, "first", "z", "@seekdeep-ai/seekdeep-z", &[]);
    package(&root, "unknown", "b", "@seekdeep-ai/seekdeep-b", &[]);
    let graph = collect_package_graph(
        root.path(),
        &["first".to_owned(), "later".to_owned()],
        "probe",
    )
    .unwrap();
    assert_eq!(
        graph
            .iter()
            .map(|node| node.short.as_str())
            .collect::<Vec<_>>(),
        ["z", "a", "b"]
    );
}

#[test]
fn missing_or_cyclic_dependencies_fail_with_remaining_key_order() {
    let root = tempfile::tempdir().unwrap();
    package(
        &root,
        "core",
        "a",
        "@seekdeep-ai/seekdeep-a",
        &["@seekdeep-ai/seekdeep-b"],
    );
    package(
        &root,
        "core",
        "b",
        "@seekdeep-ai/seekdeep-b",
        &["@seekdeep-ai/seekdeep-a"],
    );
    let error = collect_package_graph(root.path(), &[], "graph-gate")
        .unwrap_err()
        .to_string();
    assert_eq!(error, "graph-gate: dependency cycle among a, b");
}

#[test]
fn mermaid_helpers_match_javascript_utf16_and_quote_rules() {
    assert_eq!(graph_node_id("pkg", "a-b/c.d"), "pkg_a_b_c_d");
    assert_eq!(graph_node_id("pkg", "fish😀"), "pkg_fish__");
    assert_eq!(escape_mermaid_label("say \"hello\""), "say \\\"hello\\\"");
}
