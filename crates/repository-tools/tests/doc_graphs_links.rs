//! Package documentation links resolve against the repository the graph is written to.

use std::{fs, path::Path};

use indexmap::{IndexMap, IndexSet};
use seekdeep_repository_tools::{
    doc_graphs::{PackageLinks, render_event_relations, target_identity},
    package_graph::PackageGraphNode,
};
use seekdeep_typert_generator::{
    analyzer::repository_graphs::{EventRelation, EventRelations},
    catalog::{EventEntry, Mode},
};

fn package(short: &str, relative: &str) -> PackageGraphNode {
    PackageGraphNode {
        short: short.to_owned(),
        name: format!("@seekdeep-ai/seekdeep-{short}"),
        group: relative.split('/').nth(1).unwrap_or_default().to_owned(),
        relative: relative.to_owned(),
        dependencies: Vec::new(),
    }
}

fn write_readme(root: &Path, directory: &str) {
    let path = root.join(directory).join("README.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "# package").unwrap();
}

fn matrix(root: &Path, packages: &[PackageGraphNode]) -> String {
    let links = PackageLinks::resolve(root, packages);
    let mut dispatchers = IndexMap::new();
    for package in packages {
        dispatchers.insert(package.short.clone(), IndexSet::from(["emit".to_owned()]));
    }
    let mut relations = EventRelations::new();
    relations.insert(
        "g/host".to_owned(),
        EventRelation {
            dispatchers,
            listeners: IndexSet::new(),
        },
    );
    let events = [EventEntry {
        name: "g/host".to_owned(),
        scope: String::new(),
        signature: String::new(),
        js_doc: String::new(),
        mode: Mode::Emit,
        doc: String::new(),
        source: "packages/g/ported/src/index.ts:1".to_owned(),
    }];
    // The writer rewrites source identities into target ones across the whole document, so
    // the resolved path must be the rewritten one.
    target_identity(&render_event_relations(packages, &links, &events, &relations).unwrap())
}

#[test]
fn a_package_links_to_whichever_directory_owns_its_readme() {
    let root = tempfile::tempdir().unwrap();
    write_readme(root.path(), "crates/ported");
    write_readme(root.path(), "packages/g/legacy");
    fs::create_dir_all(root.path().join("packages/g/bareless")).unwrap();
    write_readme(root.path(), "packages/subagent/subagent-seekdeep-sdk");
    let packages = [
        package("ported", "packages/g/ported"),
        package("legacy", "packages/g/legacy"),
        package("bareless", "packages/g/bareless"),
        package("subagent-dsh-sdk", "packages/subagent/subagent-dsh-sdk"),
    ];
    let matrix = matrix(root.path(), &packages);
    assert!(matrix.contains("[`ported`](../crates/ported)"), "{matrix}");
    assert!(
        matrix.contains("[`legacy`](../packages/g/legacy)"),
        "{matrix}"
    );
    assert!(matrix.contains("`bareless`"), "{matrix}");
    assert!(!matrix.contains("](../packages/g/bareless)"), "{matrix}");
    assert!(
        matrix.contains("[`subagent-seekdeep-sdk`](../packages/subagent/subagent-seekdeep-sdk)"),
        "{matrix}"
    );
}
