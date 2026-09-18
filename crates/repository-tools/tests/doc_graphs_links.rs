//! Package documentation links resolve against the repository the graph is written to.

use std::{fs, path::Path};

use indexmap::{IndexMap, IndexSet};
use seekdeep_repository_tools::{
    doc_graphs::{DeclarationLinks, PackageLinks, render_event_relations, target_identity},
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
    let declarations = DeclarationLinks::resolve(root);
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
    target_identity(
        &render_event_relations(packages, &links, &declarations, &events, &relations).unwrap(),
    )
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

#[test]
fn a_declaration_links_to_the_rust_file_the_parity_manifest_names_or_stays_plain() {
    let root = tempfile::tempdir().unwrap();
    write_readme(root.path(), "crates/ported");
    let packages = [package("ported", "packages/g/ported")];
    // Without a manifest the repository is a source checkout rather than a port, and the cell
    // keeps the source generator's own form so the document reproduces the pinned one.
    let plain = matrix(root.path(), &packages);
    assert!(
        plain.contains(
            "| [`packages/g/ported/src/index.ts:1`](../packages/g/ported/src/index.ts) |"
        ),
        "{plain}"
    );
    // A source file that is still present keeps that same link.
    fs::create_dir_all(root.path().join("packages/g/ported/src")).unwrap();
    fs::write(root.path().join("packages/g/ported/src/index.ts"), "").unwrap();
    let present = matrix(root.path(), &packages);
    assert!(
        present.contains(
            "| [`packages/g/ported/src/index.ts:1`](../packages/g/ported/src/index.ts) |"
        ),
        "{present}"
    );

    fs::create_dir_all(root.path().join("porting")).unwrap();
    fs::write(
        root.path().join("porting/parity.json"),
        serde_json::json!({
            "surfaces": [
                {"source": "packages/g/ported/src/index.ts", "status": "verified", "targets": ["crates/ported/src/lib.rs"]},
                {"source": "packages/g/pending/src/index.ts", "status": "pending"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let linked = matrix(root.path(), &packages);
    assert!(
        linked.contains(
            "| [`crates/ported/src/lib.rs`](../crates/ported/src/lib.rs) from `packages/g/ported/src/index.ts:1` |"
        ),
        "{linked}"
    );

    // Under a manifest the repository is a port: a declaration whose source file the port has
    // retired, and which no verified row realizes, stays plain so the link gate has no dead
    // link to reject.
    fs::remove_file(root.path().join("packages/g/ported/src/index.ts")).unwrap();
    fs::write(
        root.path().join("porting/parity.json"),
        serde_json::json!({
            "surfaces": [
                {"source": "packages/g/ported/src/index.ts", "status": "pending"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let retired = matrix(root.path(), &packages);
    assert!(
        retired.contains("| `packages/g/ported/src/index.ts:1` |"),
        "{retired}"
    );
}
