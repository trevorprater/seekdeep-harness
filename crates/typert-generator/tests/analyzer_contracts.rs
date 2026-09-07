//! Compiler-independent workspace boundary and deterministic batch-merging contracts.

use std::path::Path;

use seekdeep_typert_generator::{
    analyzer::{
        client_export_subpaths, external_module_identity_for_file, host_export_subpaths,
        is_dual_face_package, is_remote_segment, is_standard_library_file, merge_workspace_models,
        module_identity, package_export_targets, source_path_for_export,
    },
    model::{TypeNodeModel, WorkspaceModel},
};
use serde_json::{Value, json};

#[test]
fn export_forms_preserve_condition_and_fallback_order() {
    for (manifest, expected) in [
        (
            json!({"exports":"./lib/index.js"}),
            json!([[".", "./lib/index.js"]]),
        ),
        (
            json!({"types":"./lib/index.d.ts"}),
            json!([[".", "./lib/index.d.ts"]]),
        ),
        (
            json!({"exports":[false, {}, {"default":"./default.js","import":"./import.js","types":"./types.d.ts"}]}),
            json!([[".", "./types.d.ts"]]),
        ),
        (
            json!({"exports":{"./z":null,"./b":{"default":"./b.js"},"ignored":"./no.js",".":{"types":"./index.d.ts"}}}),
            json!([[".", "./index.d.ts"], ["./b", "./b.js"]]),
        ),
        (
            serde_json::from_str(r#"{"exports":{"2":"two","1":"one"}}"#).unwrap(),
            json!([[".", "one"]]),
        ),
        (
            json!({"exports":false,"types":"fallback"}),
            json!([[".", "fallback"]]),
        ),
    ] {
        assert_eq!(
            serde_json::to_value(package_export_targets(&manifest)).unwrap(),
            expected
        );
    }
}

#[test]
fn dual_face_metadata_does_not_promote_remote_exports_to_host_source() {
    let manifest = json!({"seekdeep":{"client":{}},"exports":{".":"./lib/index.js","./remote":"./lib/remote.js","./client":"./lib/client.js","./client/typert":"./lib/typert.client.js","./types":"./lib/types.d.ts"}});
    assert!(is_dual_face_package(&manifest));
    assert_eq!(host_export_subpaths(&manifest), [".", "./types"]);
    assert_eq!(
        client_export_subpaths(&manifest),
        ["./client", "./client/typert"]
    );
    assert!(!is_dual_face_package(
        &json!({"seekdeep":{"client":{}},"exports":"index"})
    ));
    assert!(!is_dual_face_package(
        &json!({"seekdeep":{"client":false},"exports":{"./client":"client"}})
    ));
}

#[test]
fn source_paths_and_external_identities_preserve_the_authored_boundary() {
    let root = std::env::current_dir().unwrap().join("fixture-package");
    for (target, expected) in [
        ("./lib/types/nested/item.d.mts", "src/nested/item.ts"),
        ("./lib/types/item.d.cts", "src/item.ts"),
        ("./lib/item.mjs", "src/item.ts"),
        ("./lib/item.d.ts", "src/item.ts"),
        ("./lib/types/item.D.TS", "src/item.D.TS"),
        ("./src/../direct.ts", "direct.ts"),
        ("./lib/item.d.ts\n", "src/item.d.ts\n"),
    ] {
        assert_eq!(
            source_path_for_export(&root, target).unwrap(),
            root.join(expected)
        );
    }
    assert_eq!(
        source_path_for_export(&root, "/absolute.ts").unwrap(),
        Path::new("/absolute.ts")
    );
    assert_eq!(
        serde_json::to_value(module_identity("@scope/package/nested/type")).unwrap(),
        json!({"package":"@scope/package","subpath":"./nested/type"})
    );
    assert!(module_identity("../relative").is_none());
    assert_eq!(
        serde_json::to_value(external_module_identity_for_file(
            "C:\\ws\\node_modules\\.pnpm\\types@1\\node_modules\\@types\\node\\index.d.ts"
        ))
        .unwrap(),
        json!({"package":"@types/node","subpath":"."})
    );
    assert!(is_standard_library_file(
        "C:\\ws\\node_modules\\typescript\\lib\\lib.es2022.d.ts"
    ));
    assert!(!is_standard_library_file("/typescript/lib/lib.d.ts"));
    for value in ["a", "a.b", "$scope", "name-1"] {
        assert!(is_remote_segment(value));
    }
    for value in ["", ".", "..", "a/b", "a b", "é"] {
        assert!(!is_remote_segment(value));
    }
}

#[test]
fn batch_merge_keeps_last_packages_first_nodes_and_host_first_faces() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/source_type_model.json")).unwrap();
    let first: WorkspaceModel = serde_json::from_value(fixture["workspace"].clone()).unwrap();
    let mut second = first.clone();
    second.faces.reverse();
    second.faces[0].packages[0].root = "replacement".to_owned();
    second.faces[0].graph.declarations[0].text =
        "must not replace the first declaration".to_owned();
    if let TypeNodeModel::Defined(node) = &mut second.faces[0].graph.nodes[0] {
        node.kind = seekdeep_typert_generator::model::TypeNodeKind::This;
    }
    let merged = merge_workspace_models([first.clone(), second]);
    assert_eq!(merged.faces[0].face.as_str(), "host");
    assert_eq!(merged.faces[1].packages[0].root, "replacement");
    assert_eq!(merged.faces[1].graph, first.faces[1].graph);
    assert_eq!(merged.cross_face_links, first.cross_face_links);
    assert!(merge_workspace_models([]).faces.is_empty());
}
