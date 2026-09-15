//! Source-oracle coverage for package publication payload policy.

use seekdeep_repository_tools::publication_payload::{
    has_typert_remote_navigation, is_forbidden_publication_file, validate_tarball_payload,
};
use serde_json::{Value, json};

fn files(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn accepts_publishable_static_paths() {
    for file in [
        "lib/index.js",
        "lib/types/index.d.ts",
        "lib/styles/base.css",
    ] {
        assert!(!is_forbidden_publication_file(file));
    }
}

#[test]
fn rejects_source_and_map_static_paths_after_source_normalization() {
    for file in [
        "src",
        "./src",
        "src/",
        "src/index.ts",
        "./src/index.ts",
        "src\\index.ts",
        "lib/types/index.d.ts.map",
        "./lib/types/index.d.ts.map",
        "lib/typert.remote-client.d.ts.map",
        "lib/client.js.map",
        "./lib/client.js.map",
    ] {
        assert!(is_forbidden_publication_file(file), "{file}");
    }
}

#[test]
fn packed_tarballs_name_the_first_forbidden_source_or_map() {
    let source = validate_tarball_payload(
        &files(&["package/package.json", "package/src/index.ts"]),
        "fixture.tgz",
    )
    .unwrap_err();
    assert_eq!(
        source.to_string(),
        "fixture.tgz publishes source file package/src/index.ts"
    );
    for file in [
        "package/lib/types/index.d.ts.map",
        "package/lib/typert.remote-client.d.ts.map",
        "package/lib/client.js.map",
    ] {
        let error =
            validate_tarball_payload(&files(&["package/package.json", file]), "fixture.tgz")
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("fixture.tgz publishes source map {file}")
        );
    }
}

#[test]
fn clean_packed_tarball_is_accepted() {
    validate_tarball_payload(
        &files(&[
            "package/package.json",
            "package/lib/index.js",
            "package/lib/types/index.d.ts",
            "package/lib/styles/base.css",
        ]),
        "fixture.tgz",
    )
    .unwrap();
}

#[test]
fn recognizes_only_the_canonical_host_for_client_export_pair() {
    assert!(has_typert_remote_navigation(&json!({
        "exports": {
            "./remote": {
                "types": "./lib/typert.remote-client.d.ts",
                "default": "./lib/typert.remote-client.js"
            }
        }
    })));
    assert!(!has_typert_remote_navigation(
        &json!({"exports":{"./remote":"./lib/remote.js"}})
    ));
    for manifest in [Value::Null, json!([]), json!({})] {
        assert!(!has_typert_remote_navigation(&manifest));
    }
}
