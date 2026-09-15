//! Plain-Node self-reference, default export, broken map, and unstaged chunk fixtures.

use seekdeep_repository_tools::built_package_invariants::verify_built_package_invariants;

fn fixture(
    source: &str,
    invariant_export: &str,
    include_chunk: bool,
) -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("packages/core/probe");
    std::fs::create_dir_all(package.join("lib")).unwrap();
    std::fs::write(
        package.join("package.json"),
        format!(
            "{{\"name\":\"@seekdeep-ai/seekdeep-probe\",\"type\":\"module\",\"files\":[\"lib/invariant.js\"],\"exports\":{{\"./invariant\":{{\"default\":\"{invariant_export}\"}}}}}}\n"
        ),
    )
    .unwrap();
    std::fs::write(package.join("lib/invariant.js"), source).unwrap();
    if include_chunk {
        std::fs::write(
            package.join("lib/chunk.js"),
            "export const name='probe'; export const inject=['invariants']; export const apply=()=>{};\n",
        )
        .unwrap();
    }
    let loader = root.path().join("loader.mjs");
    std::fs::write(
        &loader,
        "export default class Loader { unwrapExports(value) { return value } }\n",
    )
    .unwrap();
    (root, url::Url::from_file_path(loader).unwrap().to_string())
}

#[test]
fn staged_companion_loads_through_plain_node() {
    let (root, loader) = fixture(
        "export const name='probe'; export const inject=['invariants']; export const apply=()=>{};\n",
        "./lib/invariant.js",
        false,
    );
    let report = verify_built_package_invariants(root.path(), Some(&loader)).unwrap();
    assert_eq!(report.checked, 1);
    assert!(report.failures.is_empty());
}

#[test]
fn default_export_and_broken_export_map_are_rejected() {
    let (root, loader) = fixture(
        "export default {}; export const name='probe'; export const inject=['invariants']; export const apply=()=>{};\n",
        "./lib/invariant.js",
        false,
    );
    assert!(
        verify_built_package_invariants(root.path(), Some(&loader))
            .unwrap()
            .failures[0]
            .contains("default export")
    );
    let (root, loader) = fixture("export const name='probe';\n", "./lib/missing.js", false);
    assert!(
        verify_built_package_invariants(root.path(), Some(&loader))
            .unwrap()
            .failures[0]
            .contains("manifest does not publish")
    );
}

#[test]
fn undeclared_runtime_chunk_is_not_staged() {
    let (root, loader) = fixture("export * from './chunk.js';\n", "./lib/invariant.js", true);
    assert!(
        verify_built_package_invariants(root.path(), Some(&loader))
            .unwrap()
            .failures[0]
            .contains("chunk.js")
    );
}
