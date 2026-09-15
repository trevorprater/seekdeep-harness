//! Group order, Mermaid edges, links, and unknown dependency fixtures.

use seekdeep_repository_tools::{
    module_graph::render_module_graph, package_graph::PackageGraphNode,
};

#[test]
fn renders_grouped_mermaid_and_dependency_table() {
    let packages = vec![
        PackageGraphNode {
            short: "base".into(),
            name: "@seekdeep-ai/seekdeep-base".into(),
            group: "core".into(),
            relative: "packages/core/base".into(),
            dependencies: vec![],
        },
        PackageGraphNode {
            short: "tool".into(),
            name: "@seekdeep-ai/seekdeep-tool".into(),
            group: "web".into(),
            relative: "packages/web/tool".into(),
            dependencies: vec!["base".into(), "external".into()],
        },
    ];
    let output = render_module_graph(&packages);
    assert!(output.contains("subgraph group_core[\"packages/core\"]"));
    assert!(output.contains("pkg_tool --> pkg_base"));
    assert!(output.contains("[`base`](../packages/core/base)"));
    assert!(output.contains("`external`"));
    assert!(output.contains("@seekdeep-ai/seekdeep-*"));
    assert!(output.ends_with('\n'));
}
