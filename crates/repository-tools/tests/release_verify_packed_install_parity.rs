//! No-entry, empty directory, installed version, environment, and mismatch fixtures.

use std::process::Command;

use seekdeep_repository_tools::{
    release_families::ReleaseFamily,
    release_verify_packed_install::{consumer_environment, verify_packed_install_with},
};

fn tarball(directory: &std::path::Path, name: &str, version: &str) {
    let staging = tempfile::tempdir().unwrap();
    let package = staging.path().join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"{name}\",\"version\":\"{version}\"}}\n"),
    )
    .unwrap();
    assert!(
        Command::new("tar")
            .args(["-czf"])
            .arg(directory.join("probe.tgz"))
            .args(["-C"])
            .arg(staging.path())
            .arg("package")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn vendor_family_short_circuits_without_reading_directories() {
    assert!(
        verify_packed_install_with(
            ReleaseFamily::Vendor,
            &["/does/not/exist".into()],
            |_command, _args, _cwd, _env| unreachable!(),
        )
        .unwrap()
        .contains("publishes no executable")
    );
}

#[test]
fn empty_directory_and_missing_entry_fail_loud() {
    let empty = tempfile::tempdir().unwrap();
    assert!(
        verify_packed_install_with(
            ReleaseFamily::SeekDeep,
            &[empty.path().to_owned()],
            |_command, _args, _cwd, _env| Ok(String::new()),
        )
        .unwrap_err()
        .to_string()
        .contains("holds no packed tarball")
    );
    tarball(empty.path(), "@seekdeep-ai/other", "1.0.0");
    assert!(
        verify_packed_install_with(
            ReleaseFamily::SeekDeep,
            &[empty.path().to_owned()],
            |_command, _args, _cwd, _env| Ok(String::new()),
        )
        .unwrap_err()
        .to_string()
        .contains("not among the packed tarballs")
    );
}

#[test]
fn installed_version_uses_sanitized_environment() {
    let packed = tempfile::tempdir().unwrap();
    tarball(packed.path(), "@seekdeep-ai/seekdeep", "1.2.3");
    let mut calls = 0;
    let output = verify_packed_install_with(
        ReleaseFamily::SeekDeep,
        &[packed.path().to_owned()],
        |command, args, cwd, env| {
            calls += 1;
            assert!(!env.contains_key(std::ffi::OsStr::new("NODE_OPTIONS")));
            assert_eq!(
                env.get(std::ffi::OsStr::new("SEEKDEEP_HOME")),
                Some(&cwd.join(".seekdeep").into_os_string())
            );
            if command == "npm" {
                assert!(args.contains(&"--omit=optional".to_owned()));
                Ok(String::new())
            } else {
                Ok("1.2.3".to_owned())
            }
        },
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert!(output.contains("installed @seekdeep-ai/seekdeep reports 1.2.3"));
}

#[test]
fn installed_version_mismatch_fails() {
    let packed = tempfile::tempdir().unwrap();
    tarball(packed.path(), "@seekdeep-ai/seekdeep", "1.2.3");
    let error = verify_packed_install_with(
        ReleaseFamily::SeekDeep,
        &[packed.path().to_owned()],
        |command, _args, _cwd, _env| Ok(if command == "npm" { "" } else { "9.9.9" }.to_owned()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("reported \"9.9.9\", expected 1.2.3"));
}

#[test]
fn environment_removes_host_node_and_npm_hooks() {
    let root = std::path::Path::new("/consumer");
    let environment = consumer_environment(
        root,
        [
            ("NODE_OPTIONS".into(), "--import hook".into()),
            ("NODE_PATH".into(), "/host".into()),
            ("KEEP".into(), "yes".into()),
        ],
    );
    assert_eq!(
        environment.get(std::ffi::OsStr::new("KEEP")),
        Some(&"yes".into())
    );
    assert!(!environment.contains_key(std::ffi::OsStr::new("NODE_OPTIONS")));
}
