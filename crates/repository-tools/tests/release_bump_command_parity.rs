//! Shared dry-run, write workflow, and vendor tag-baseline fixtures.

use seekdeep_repository_tools::{
    release_bump_command::{ReleaseBumpOptions, bump_release_with},
    release_families::ReleaseFamily,
};

fn shared_repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("package.json"),
        "{\n  \"version\": \"1.2.3\"\n}\n",
    )
    .unwrap();
    let package = root.path().join("packages/core/probe");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        "{\n  \"name\": \"@seekdeep-ai/seekdeep-probe\",\n  \"version\": \"1.2.3\"\n}\n",
    )
    .unwrap();
    root
}

#[test]
fn seekdeep_dry_run_reports_without_writing_or_commands() {
    let root = shared_repository();
    let before = std::fs::read_to_string(root.path().join("package.json")).unwrap();
    let output = bump_release_with(
        root.path(),
        &ReleaseBumpOptions {
            family: ReleaseFamily::SeekDeep,
            version: Some("patch".to_owned()),
            prerelease: None,
            dry_run: true,
        },
        |_command, _args| unreachable!(),
    )
    .unwrap();
    assert!(output.contains("family seekdeep -> 1.2.4"));
    assert!(output.contains("dry run, nothing written"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("package.json")).unwrap(),
        before
    );
}

#[test]
fn seekdeep_write_updates_manifests_and_requests_lockfile_commit() {
    let root = shared_repository();
    let mut commands = Vec::new();
    bump_release_with(
        root.path(),
        &ReleaseBumpOptions {
            family: ReleaseFamily::SeekDeep,
            version: Some("minor".to_owned()),
            prerelease: None,
            dry_run: false,
        },
        |command, args| {
            commands.push((command.to_owned(), args.to_vec()));
            Ok(String::new())
        },
    )
    .unwrap();
    assert!(
        std::fs::read_to_string(root.path().join("package.json"))
            .unwrap()
            .contains("1.3.0")
    );
    assert!(
        std::fs::read_to_string(root.path().join("packages/core/probe/package.json"))
            .unwrap()
            .contains("1.3.0")
    );
    assert_eq!(commands[0].0, "pnpm");
    assert_eq!(commands[1].1[0], "add");
    assert_eq!(commands[2].1[0], "commit");
}

#[test]
fn vendor_dry_run_uses_newest_tag_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("vendor/cordis");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        "{\n  \"name\": \"@seekdeep-ai/cordis\",\n  \"version\": \"4.0.0-rc.8\"\n}\n",
    )
    .unwrap();
    let output = bump_release_with(
        root.path(),
        &ReleaseBumpOptions {
            family: ReleaseFamily::Vendor,
            version: None,
            prerelease: None,
            dry_run: true,
        },
        |_command, args| {
            assert_eq!(args[0], "tag");
            Ok("vendor-cordis-v4.0.1\n".to_owned())
        },
    )
    .unwrap();
    assert!(output.contains("cordis 4.0.2"));
}
