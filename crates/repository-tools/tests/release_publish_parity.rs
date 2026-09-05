//! Integrity skip/refusal, successful publish, prerelease tag, transient retry, and landed-failure fixtures.

use std::process::Command;

use base64::Engine as _;
use seekdeep_repository_tools::{
    release_families::ReleaseFamily, release_process::ReleaseCommandResult,
    release_publish::publish_release_with, release_tarball::PUBLISH_ORDER_FILE,
};
use sha2::{Digest as _, Sha512};

fn packed() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let stage = tempfile::tempdir().unwrap();
    let package = stage.path().join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        "{\"name\":\"@seekdeep-ai/probe\",\"version\":\"1.2.3\"}\n",
    )
    .unwrap();
    assert!(
        Command::new("tar")
            .args(["-czf"])
            .arg(root.path().join("probe.tgz"))
            .args(["-C"])
            .arg(stage.path())
            .arg("package")
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(root.path().join(PUBLISH_ORDER_FILE), "probe.tgz\n").unwrap();
    root
}

fn result(status: i32, stdout: &str, stderr: &str) -> ReleaseCommandResult {
    ReleaseCommandResult {
        status: Some(status),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    }
}

#[test]
fn absent_version_publishes_once() {
    let root = packed();
    let mut calls = 0;
    let outcome = publish_release_with(
        ReleaseFamily::Vendor,
        root.path(),
        |_command, args| {
            calls += 1;
            Ok(if args[0] == "view" {
                result(1, "", "E404")
            } else {
                result(0, "published", "")
            })
        },
        |_delay| {},
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(outcome.published, 1);
}

#[test]
fn different_published_integrity_is_refused() {
    let root = packed();
    let error = publish_release_with(
        ReleaseFamily::Vendor,
        root.path(),
        |_command, _args| Ok(result(0, "\"sha512-different\"", "")),
        |_delay| {},
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("already published with different content"));
}

#[test]
fn transient_failure_retries_with_backoff() {
    let root = packed();
    let mut publish_calls = 0;
    let mut delays = Vec::new();
    let outcome = publish_release_with(
        ReleaseFamily::Vendor,
        root.path(),
        |_command, args| {
            if args[0] == "view" {
                Ok(result(1, "", "E404"))
            } else {
                publish_calls += 1;
                Ok(if publish_calls == 1 {
                    result(1, "", "npm ERR! code E409")
                } else {
                    result(0, "", "")
                })
            }
        },
        |delay| delays.push(delay),
    )
    .unwrap();
    assert_eq!(publish_calls, 2);
    assert_eq!(delays, [std::time::Duration::from_millis(2_000)]);
    assert_eq!(outcome.published, 1);
}

#[test]
fn identical_published_integrity_is_skipped_without_publish() {
    let root = packed();
    let bytes = std::fs::read(root.path().join("probe.tgz")).unwrap();
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
    );
    let mut calls = 0;
    let outcome = publish_release_with(
        ReleaseFamily::Vendor,
        root.path(),
        |_command, args| {
            calls += 1;
            assert_eq!(args[0], "view");
            Ok(result(0, &serde_json::to_string(&integrity).unwrap(), ""))
        },
        |_delay| {},
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(outcome.skipped, 1);
    assert_eq!(outcome.published, 0);
}
