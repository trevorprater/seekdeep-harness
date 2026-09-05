//! Local, private, ref, wrong-family, unknown-version, and valid-publish fixtures.

use seekdeep_repository_tools::{release_families::ReleaseFamily, release_verify::verify_release};
use tempfile::TempDir;

fn repository(private: bool) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("packages/core/probe");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        format!(
            "{{\"name\":\"@seekdeep-ai/seekdeep-probe\",\"version\":\"1.2.3\",\"private\":{private}}}\n"
        ),
    )
    .unwrap();
    root
}

#[test]
fn local_verification_checks_versions_without_publish_gates() {
    let root = repository(true);
    assert_eq!(
        verify_release(root.path(), ReleaseFamily::SeekDeep, false, "").unwrap(),
        "release verify: family seekdeep, 1 member(s), 1.2.3\n"
    );
}

#[test]
fn publishing_rejects_private_members_before_tag_checks() {
    let root = repository(true);
    assert!(
        verify_release(
            root.path(),
            ReleaseFamily::SeekDeep,
            true,
            "refs/tags/seekdeep-v1.2.3",
        )
        .unwrap_err()
        .to_string()
        .contains("removing \"private\": true")
    );
}

#[test]
fn publishing_rejects_missing_wrong_family_and_unknown_version_tags() {
    let root = repository(false);
    for (reference, expected) in [
        ("", "requires running from"),
        ("refs/tags/vendor-probe-v1.2.3", "does not belong"),
        ("refs/tags/seekdeep-v9.9.9", "names no version"),
    ] {
        assert!(
            verify_release(root.path(), ReleaseFamily::SeekDeep, true, reference,)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[test]
fn matching_publish_tag_passes() {
    let root = repository(false);
    assert!(
        verify_release(
            root.path(),
            ReleaseFamily::SeekDeep,
            true,
            "refs/tags/seekdeep-v1.2.3",
        )
        .unwrap()
        .contains("publish gates passed")
    );
}
