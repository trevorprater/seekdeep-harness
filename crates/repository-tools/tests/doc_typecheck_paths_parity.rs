//! Source-oracle coverage for declaration-build target mapping.

use seekdeep_repository_tools::doc_typecheck_paths::built_declaration_path;

#[test]
fn maps_package_sources_wildcards_files_and_subdirectories() {
    for (source, expected) in [
        ("./packages/*/*/src", "./packages/*/*/lib/types"),
        ("./packages/*/*/src/*", "./packages/*/*/lib/types/*"),
        (
            "./packages/runtime-diagnostics/invariants/src/index.ts",
            "./packages/runtime-diagnostics/invariants/lib/types/index.d.ts",
        ),
        (
            "./packages/core/session/src/invariant.ts",
            "./packages/core/session/lib/types/invariant.d.ts",
        ),
        (
            "./packages/host/apiproxy/src/client/store",
            "./packages/host/apiproxy/lib/types/client/store",
        ),
    ] {
        assert_eq!(built_declaration_path(source).unwrap(), expected);
    }
}

#[test]
fn rejects_aliases_without_a_supported_source_target() {
    let candidate = "./packages/runtime-diagnostics/invariants/source/index.ts";
    let error = built_declaration_path(candidate).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "doc-typecheck: cannot map workspace source path to built declarations: {candidate}"
        )
    );
}
