//! Real tarball ordering, payload, order file, cleanup, and missing-output fixtures.

use std::process::Command;

use seekdeep_repository_tools::{
    release_families::ReleaseFamily,
    release_pack::pack_release_with,
    release_tarball::{PUBLISH_ORDER_FILE, read_publish_order},
};
use tempfile::TempDir;

fn package(root: &TempDir, leaf: &str, dependency: Option<&str>) {
    let directory = root.path().join("packages/core").join(leaf);
    std::fs::create_dir_all(&directory).unwrap();
    let dependencies = dependency.map_or_else(serde_json::Map::new, |name| {
        serde_json::Map::from_iter([(name.to_owned(), serde_json::json!("workspace:^"))])
    });
    std::fs::write(
        directory.join("package.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": format!("@seekdeep-ai/seekdeep-{leaf}"),
                "version": "1.2.3",
                "dependencies": dependencies,
            }))
            .unwrap()
        ),
    )
    .unwrap();
}

fn write_tarball(
    member: &seekdeep_repository_tools::release_families::ReleaseMember,
    output: &std::path::Path,
) -> anyhow::Result<()> {
    let staging = tempfile::tempdir()?;
    let package = staging.path().join("package");
    std::fs::create_dir_all(package.join("lib"))?;
    std::fs::write(
        package.join("package.json"),
        serde_json::to_vec(&member.manifest)?,
    )?;
    std::fs::write(package.join("lib/index.js"), "export {}\n")?;
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(
            output.join(seekdeep_repository_tools::release_families::tarball_name(
                member,
            )),
        )
        .args(["-C"])
        .arg(staging.path())
        .arg("package")
        .status()?;
    anyhow::ensure!(status.success(), "tar failed");
    Ok(())
}

#[test]
fn packs_dependency_order_and_records_only_validated_tarballs() {
    let root = tempfile::tempdir().unwrap();
    package(&root, "library", None);
    package(&root, "consumer", Some("@seekdeep-ai/seekdeep-library"));
    let output = root.path().join("dist/npm");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("stale"), "stale").unwrap();
    let result =
        pack_release_with(root.path(), ReleaseFamily::SeekDeep, &output, write_tarball).unwrap();
    assert_eq!(
        result.order,
        [
            "seekdeep-ai-seekdeep-library-1.2.3.tgz",
            "seekdeep-ai-seekdeep-consumer-1.2.3.tgz",
        ]
    );
    assert!(!output.join("stale").exists());
    assert_eq!(read_publish_order(&output).unwrap(), result.order);
    assert!(output.join(PUBLISH_ORDER_FILE).is_file());
}

#[test]
fn missing_expected_tarball_fails_loud() {
    let root = tempfile::tempdir().unwrap();
    package(&root, "probe", None);
    let error = pack_release_with(
        root.path(),
        ReleaseFamily::SeekDeep,
        &root.path().join("out"),
        |_member, _output| Ok(()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("produced no tarball"));
}
