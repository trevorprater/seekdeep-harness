//! Tag, version, publish-order, cycle, payload, entry, identity, and tarball fixtures.

use seekdeep_repository_tools::release_families::{ReleaseFamily, ReleaseMember, tarball_name};

fn member(directory: &str, name: &str, manifest: serde_json::Value) -> ReleaseMember {
    ReleaseMember {
        directory: directory.to_owned(),
        name: name.to_owned(),
        version: "0.0.1".to_owned(),
        manifest,
    }
}

#[test]
fn tags_are_shared_for_seekdeep_and_per_vendor_package() {
    let cli = member("apps/cli", "@seekdeep-ai/seekdeep", serde_json::json!({}));
    let mut cordis = member(
        "vendor/cordis",
        "@seekdeep-ai/cordis",
        serde_json::json!({}),
    );
    cordis.version = "4.0.1".to_owned();
    assert_eq!(ReleaseFamily::SeekDeep.tag_for(&cli), "seekdeep-v0.0.1");
    assert_eq!(
        ReleaseFamily::Vendor.tag_for(&cordis),
        "vendor-cordis-v4.0.1"
    );
    cordis.version = "4.0.0-rc.7".to_owned();
    assert_eq!(
        ReleaseFamily::Vendor.tag_prefix_for(&cordis),
        "vendor-cordis-v"
    );
}

#[test]
fn version_baselines_match_each_family() {
    let one = member("apps/cli", "@seekdeep-ai/seekdeep", serde_json::json!({}));
    let mut two = member(
        "apps/web",
        "@seekdeep-ai/seekdeep-web",
        serde_json::json!({}),
    );
    two.version = "0.0.2".to_owned();
    assert!(
        ReleaseFamily::SeekDeep
            .verify_versions(std::slice::from_ref(&one))
            .is_ok()
    );
    assert!(
        ReleaseFamily::SeekDeep
            .verify_versions(&[one, two])
            .is_err()
    );
    let mut vendor = member(
        "vendor/cordis",
        "@seekdeep-ai/cordis",
        serde_json::json!({}),
    );
    vendor.version = "4.0.1-rc.2".to_owned();
    assert!(
        ReleaseFamily::Vendor
            .verify_versions(std::slice::from_ref(&vendor))
            .is_ok()
    );
    vendor.version = "latest".to_owned();
    assert!(ReleaseFamily::Vendor.verify_versions(&[vendor]).is_err());
}

#[test]
fn dependencies_publish_before_consumers_and_ties_sort_by_name() {
    let consumer = member(
        "packages/a/consumer",
        "@seekdeep-ai/seekdeep-consumer",
        serde_json::json!({ "dependencies": { "@seekdeep-ai/seekdeep-library": "workspace:^" } }),
    );
    let library = member(
        "packages/a/library",
        "@seekdeep-ai/seekdeep-library",
        serde_json::json!({}),
    );
    let zebra = member(
        "packages/a/zebra",
        "@seekdeep-ai/seekdeep-zebra",
        serde_json::json!({}),
    );
    assert_eq!(
        ReleaseFamily::SeekDeep
            .publish_order(&[consumer, library, zebra])
            .unwrap()
            .into_iter()
            .map(|member| member.name)
            .collect::<Vec<_>>(),
        [
            "@seekdeep-ai/seekdeep-library",
            "@seekdeep-ai/seekdeep-consumer",
            "@seekdeep-ai/seekdeep-zebra",
        ]
    );
}

#[test]
fn dependency_cycle_fails_loud() {
    let left = member(
        "packages/a/left",
        "@seekdeep-ai/seekdeep-left",
        serde_json::json!({ "dependencies": { "@seekdeep-ai/seekdeep-right": "workspace:^" } }),
    );
    let right = member(
        "packages/a/right",
        "@seekdeep-ai/seekdeep-right",
        serde_json::json!({ "dependencies": { "@seekdeep-ai/seekdeep-left": "workspace:^" } }),
    );
    assert!(
        ReleaseFamily::SeekDeep
            .publish_order(&[left, right])
            .unwrap_err()
            .to_string()
            .contains("dependency cycle")
    );
}

#[test]
fn payload_policy_and_installed_entry_are_family_specific() {
    let seekdeep = member(
        "packages/a/lib",
        "@seekdeep-ai/seekdeep-lib",
        serde_json::json!({}),
    );
    let vendor = member(
        "vendor/cordis",
        "@seekdeep-ai/cordis",
        serde_json::json!({}),
    );
    assert!(
        ReleaseFamily::SeekDeep
            .validate_payload(&seekdeep, &["package/src/index.ts".to_owned()])
            .is_err()
    );
    assert!(
        ReleaseFamily::Vendor
            .validate_payload(&vendor, &["package/src/index.ts".to_owned()])
            .is_ok()
    );
    assert!(
        ReleaseFamily::Vendor
            .validate_payload(&vendor, &[])
            .is_err()
    );
    assert_eq!(
        ReleaseFamily::SeekDeep
            .installed_entry()
            .unwrap()
            .package_name,
        "@seekdeep-ai/seekdeep"
    );
    assert_eq!(ReleaseFamily::Vendor.installed_entry(), None);
}

#[test]
fn closed_family_ids_and_tarball_names_are_product_renamed() {
    assert_eq!(
        ReleaseFamily::resolve("seekdeep").unwrap(),
        ReleaseFamily::SeekDeep
    );
    assert!(ReleaseFamily::resolve("native").is_err());
    let package = member("apps/cli", "@seekdeep-ai/seekdeep", serde_json::json!({}));
    assert_eq!(tarball_name(&package), "seekdeep-ai-seekdeep-0.0.1.tgz");
}

#[test]
fn live_release_families_discover_and_order_cleanly() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for family in [ReleaseFamily::SeekDeep, ReleaseFamily::Vendor] {
        let members = family.members(&root).unwrap();
        assert!(!members.is_empty());
        family.verify_versions(&members).unwrap();
        assert_eq!(family.publish_order(&members).unwrap().len(), members.len());
    }
}
