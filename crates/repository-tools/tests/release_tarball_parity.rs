//! Real gzip tar member, manifest identity, shape failure, and order fixtures.

use std::process::Command;

use seekdeep_repository_tools::release_tarball::{
    PUBLISH_ORDER_FILE, PackedIdentity, packed_identity, read_publish_order, tarball_files,
};

#[test]
fn real_tarball_members_and_identity_are_read_from_archive_bytes() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("package");
    std::fs::create_dir_all(package.join("lib")).unwrap();
    std::fs::write(
        package.join("package.json"),
        "{\"name\":\"@seekdeep-ai/probe\",\"version\":\"1.2.3\"}\n",
    )
    .unwrap();
    std::fs::write(package.join("lib/index.js"), "export {}\n").unwrap();
    let tarball = root.path().join("probe.tgz");
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&tarball)
        .args(["-C"])
        .arg(root.path())
        .arg("package")
        .status()
        .unwrap();
    assert!(status.success());

    let files = tarball_files(&tarball).unwrap();
    assert!(files.iter().any(|file| file == "package/package.json"));
    assert!(files.iter().any(|file| file == "package/lib/index.js"));
    assert_eq!(
        packed_identity(&tarball).unwrap(),
        PackedIdentity {
            name: "@seekdeep-ai/probe".to_owned(),
            version: "1.2.3".to_owned(),
        }
    );
}

#[test]
fn manifest_without_string_identity_fields_fails_loud() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), "{\"name\":7}\n").unwrap();
    let tarball = root.path().join("invalid.tgz");
    assert!(
        Command::new("tar")
            .args(["-czf"])
            .arg(&tarball)
            .args(["-C"])
            .arg(root.path())
            .arg("package")
            .status()
            .unwrap()
            .success()
    );
    assert!(
        packed_identity(&tarball)
            .unwrap_err()
            .to_string()
            .ends_with("manifest lacks name/version")
    );
}

#[test]
fn publish_order_filters_only_empty_lines_and_preserves_carriage_returns() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join(PUBLISH_ORDER_FILE),
        b"first.tgz\n\nsecond.tgz\r\nthird\xff.tgz\n",
    )
    .unwrap();
    assert_eq!(
        read_publish_order(root.path()).unwrap(),
        ["first.tgz", "second.tgz\r", "third�.tgz"]
    );
}
