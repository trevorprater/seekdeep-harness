//! Existing, moved, hypothetical, group, lib, token, and line fixtures.

use std::collections::HashSet;

use seekdeep_repository_tools::package_paths::find_package_path_violations;
use tempfile::TempDir;

fn fixture(source: &str) -> (TempDir, std::path::PathBuf, HashSet<String>) {
    let root = tempfile::tempdir().unwrap();
    for path in [
        "packages/current/actual",
        "packages/sdk/client",
        "packages/client/visible",
        "packages/old/other",
    ] {
        std::fs::create_dir_all(root.path().join(path)).unwrap();
    }
    let authored = root.path().join("README.md");
    std::fs::write(&authored, source).unwrap();
    let names = ["actual", "client", "visible", "other"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    (root, authored, names)
}

#[test]
fn existing_hypothetical_group_explained_and_unbuilt_lib_paths_are_accepted() {
    let (root, authored, names) = fixture(
        "packages/current/actual\npackages/future/not-real\npackages/client/missing\npackages/current/actual/lib/index.js\npackages/*/actual\npackages/<group>/actual\n",
    );
    assert!(
        find_package_path_violations(root.path(), &authored, &names)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn moved_live_leaf_and_stale_group_less_paths_are_reported_in_line_order() {
    let (root, authored, names) =
        fixture("first packages/old/actual/src/index.ts.\nsecond packages/actual/lib/index.js/\n");
    let violations = find_package_path_violations(root.path(), &authored, &names).unwrap();
    assert_eq!(violations.len(), 2);
    assert_eq!(violations[0].line, 1);
    assert_eq!(violations[0].reference, "packages/old/actual/src/index.ts");
    assert_eq!(violations[1].line, 2);
    assert_eq!(violations[1].reference, "packages/actual/lib/index.js");
}

#[test]
fn every_reference_on_one_line_is_examined() {
    let (root, authored, names) = fixture(
        "packages/old/actual/a.ts and packages/old/client/b.ts and packages/old/unknown/c.ts\n",
    );
    let violations = find_package_path_violations(root.path(), &authored, &names).unwrap();
    assert_eq!(
        violations
            .into_iter()
            .map(|violation| violation.reference)
            .collect::<Vec<_>>(),
        ["packages/old/actual/a.ts", "packages/old/client/b.ts"]
    );
}
