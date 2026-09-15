//! Declaration scan, manifest discovery, public export, and failure-order fixtures.

use seekdeep_repository_tools::node_next_types::{
    NodeNextTypesReport, node_next_workspace_packages, relative_specifiers_missing_extensions,
    verify_node_next_types,
};
use tempfile::TempDir;

fn write(root: &TempDir, relative: &str, content: &str) {
    let path = root.path().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn declaration_scan_covers_static_dynamic_side_effect_and_module_specifiers() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root,
        "packages/core/demo/lib/types/index.d.ts",
        "export { x } from './static';\nexport type Y = import(\"../dynamic\").Y;\nimport './effect';\ndeclare module \"./ambient\" {}\nimport { z } from './with.js';\nimport type { Q } from '@scope/pkg';\n",
    );
    assert_eq!(
        relative_specifiers_missing_extensions(root.path()).unwrap(),
        [
            "packages/core/demo/lib/types/index.d.ts: ./static",
            "packages/core/demo/lib/types/index.d.ts: ../dynamic",
            "packages/core/demo/lib/types/index.d.ts: ./effect",
            "packages/core/demo/lib/types/index.d.ts: ./ambient",
        ]
    );
}

#[test]
fn public_specifiers_follow_types_and_explicit_typed_exports_only() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root,
        "packages/core/demo/package.json",
        r#"{
  "name": "@seekdeep-ai/demo",
  "types": "./lib/types/index.d.ts",
  "exports": {
    ".": { "types": "./lib/types/index.d.ts" },
    "./feature": { "types": "./lib/types/feature.d.ts" },
    "./wild/*": { "types": "./lib/types/*.d.ts" },
    "./package.json": "./package.json",
    "./runtime": "./lib/index.js",
    "./empty": null
  }
}
"#,
    );
    write(
        &root,
        "packages/core/unnamed/package.json",
        "{\"types\":\"./index.d.ts\"}\n",
    );
    let packages = node_next_workspace_packages(root.path()).unwrap();
    assert_eq!(packages.len(), 1);
    assert_eq!(
        packages[0].public_specifiers(),
        ["@seekdeep-ai/demo", "@seekdeep-ai/demo/feature"]
    );
}

#[test]
fn extension_errors_precede_missing_output_errors() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root,
        "packages/core/demo/package.json",
        "{\"name\":\"@seekdeep-ai/demo\",\"types\":\"./lib/types/missing.d.ts\"}\n",
    );
    write(
        &root,
        "packages/core/demo/lib/types/index.d.ts",
        "export * from './missing';\n",
    );
    assert!(matches!(
        verify_node_next_types(root.path()).unwrap(),
        NodeNextTypesReport::MissingSpecifierExtensions(errors)
            if errors == ["packages/core/demo/lib/types/index.d.ts: ./missing"]
    ));
}

#[test]
fn missing_top_level_types_outputs_are_named_in_package_order() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root,
        "vendor/zeta/package.json",
        "{\"name\":\"zeta\",\"types\":\"./lib/types/index.d.ts\"}\n",
    );
    write(
        &root,
        "packages/core/alpha/package.json",
        "{\"name\":\"alpha\",\"types\":\"./types.d.ts\"}\n",
    );
    assert_eq!(
        verify_node_next_types(root.path()).unwrap(),
        NodeNextTypesReport::MissingOutputs(vec![
            "alpha: missing ./types.d.ts".to_owned(),
            "zeta: missing ./lib/types/index.d.ts".to_owned(),
        ])
    );
}
