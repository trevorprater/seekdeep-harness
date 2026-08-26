//! Vendor baselines, `SemVer` precedence, and payload reachability fixtures.

use seekdeep_repository_tools::{
    release_bump::{compare_versions, next_vendor_version, reaches_payload},
    release_families::ReleaseMember,
};

fn member(directory: &str, files: &[&str]) -> ReleaseMember {
    ReleaseMember {
        directory: directory.to_owned(),
        name: "@seekdeep-ai/probe".to_owned(),
        version: "0.0.1".to_owned(),
        manifest: serde_json::json!({ "files": files }),
    }
}

#[test]
fn vendor_versions_drop_upstream_prerelease_and_respect_newer_tags() {
    assert_eq!(
        next_vendor_version("4.0.0-rc.7", None, None).unwrap(),
        "4.0.1"
    );
    assert_eq!(
        next_vendor_version("4.0.0-rc.8", Some("4.0.1"), None).unwrap(),
        "4.0.2"
    );
    assert_eq!(
        next_vendor_version("4.1.0", Some("4.0.1"), None).unwrap(),
        "4.1.1"
    );
}

#[test]
fn rehearsal_prereleases_do_not_consume_release_numbers() {
    assert_eq!(
        next_vendor_version("4.0.0-rc.7", None, Some("rc.1")).unwrap(),
        "4.0.1-rc.1"
    );
    assert_eq!(
        next_vendor_version("4.0.0-rc.7", Some("4.0.1-rc.1"), Some("rc.2")).unwrap(),
        "4.0.1-rc.2"
    );
    assert_eq!(
        next_vendor_version("4.0.0-rc.7", Some("4.0.1-rc.1"), None).unwrap(),
        "4.0.1"
    );
}

#[test]
fn semver_precedence_matches_release_and_numeric_identifier_rules() {
    use std::cmp::Ordering::{Equal, Greater, Less};
    assert_eq!(compare_versions("4.0.1", "4.0.1-rc.1").unwrap(), Greater);
    assert_eq!(compare_versions("4.0.1-rc.1", "4.0.1").unwrap(), Less);
    assert_eq!(
        compare_versions("4.0.1-rc.10", "4.0.1-rc.2").unwrap(),
        Greater
    );
    assert_eq!(compare_versions("4.0.1-1", "4.0.1-alpha").unwrap(), Less);
    assert_eq!(compare_versions("4.0.1-rc", "4.0.1-rc.1").unwrap(), Less);
    assert_eq!(compare_versions("4.0.1-rc.1", "4.0.1-rc.1").unwrap(), Equal);
}

#[test]
fn payload_reachability_includes_always_published_and_build_inputs() {
    let source = member("vendor/cosmokit", &["lib/index.js", "src"]);
    assert!(reaches_payload(&source, "vendor/cosmokit/package.json"));
    assert!(reaches_payload(&source, "vendor/cosmokit/README.md"));
    assert!(reaches_payload(&source, "vendor/cosmokit/src/index.ts"));
    assert!(!reaches_payload(
        &source,
        "vendor/cosmokit/tests/unit.spec.ts"
    ));
    assert!(reaches_payload(&source, "vendor/cosmokit/README.i18n.yaml"));

    let built = member("vendor/cordis", &["lib/index.js"]);
    assert!(reaches_payload(&built, "vendor/cordis/src/context.ts"));
    assert!(reaches_payload(&built, "vendor/cordis/tsconfig.json"));
}
