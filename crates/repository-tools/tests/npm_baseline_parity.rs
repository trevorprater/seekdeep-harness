//! Commit-addressed baseline source comparisons, real tarballs, and isolated process lifecycles.

#[path = "npm_baseline/bundle.rs"]
mod bundle;
#[path = "npm_baseline/capture.rs"]
mod capture;
#[path = "npm_baseline/pack.rs"]
mod pack;
#[path = "npm_baseline/registry.rs"]
mod registry;
#[cfg(unix)]
#[path = "npm_baseline/release.rs"]
mod release;
#[path = "npm_baseline/smoke.rs"]
mod smoke;
#[path = "npm_baseline/support.rs"]
mod support;

use std::process::Command;

use seekdeep_repository_tools::npm_baseline::{
    BaselinePackOptions, WorkspacePackageSet, plan_baseline,
};

use support::{NpmFixtureRunner, now, source_call};

#[test]
fn pinned_checkout_rejects_its_prerelease_base_before_build_or_registry_access() {
    let root = support::repository_root();
    let output = tempfile::tempdir().unwrap();
    let mut runner = NpmFixtureRunner::default();
    let error = plan_baseline(
        &root,
        &BaselinePackOptions {
            reference: "HEAD".to_owned(),
            registry: "https://registry.npm.harnessment.com".to_owned(),
            output_directory: output.path().to_owned(),
        },
        now(),
        &mut runner,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("must have a stable X.Y.Z version")
    );
    assert!(runner.calls.iter().all(|call| call.command == "git"));
    assert_eq!(runner.calls.len(), 3);
    let actual = source_call(
        &serde_json::json!({"op":"plan", "root":root, "output":output.path(), "now":"2026-09-11T01:02:03.456Z"}),
    );
    assert_eq!(actual["error"].as_str(), Some(error.to_string().as_str()));
    let mut entries = std::fs::read_dir(output.path()).unwrap();
    assert!(entries.next().is_none());
}

#[test]
fn command_argument_failures_match_source_without_running_any_release_operation() {
    let root = support::repository_root();
    for arguments in [
        vec!["verify"],
        vec!["publish"],
        vec!["unknown"],
        vec!["verify", "--manifest", "missing", "--yes"],
        vec!["publish", "--manifest"],
        vec!["publish", "--yes=false"],
        vec!["pack", "--bad"],
        vec!["pack", "arg"],
        vec!["publish", "--manifest", "--yes"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_publish-npm-baseline"))
            .args(&arguments)
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8(output.stderr).unwrap();
        let source =
            source_call(&serde_json::json!({"op":"main", "arguments":arguments, "root":root}));
        assert_eq!(
            error.trim(),
            format!(
                "publish-npm-baseline: {}",
                source["error"].as_str().unwrap()
            )
        );
    }
}

#[test]
fn help_succeeds_outside_git_and_does_not_require_node_or_registry() {
    let root = tempfile::tempdir().unwrap();
    for arguments in [
        vec![],
        vec!["help"],
        vec!["--help"],
        vec!["-h"],
        vec!["unknown", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_publish-npm-baseline"))
            .args(arguments)
            .current_dir(root.path())
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.starts_with("Usage:\n"));
        assert!(output.contains("publish --manifest <path> [--yes]"));
        assert!(output.contains("pack/release without waiting for Enter"));
    }
}

#[test]
fn workspace_discovery_preserves_source_failures_and_ordering() {
    let fixture = support::workspace();
    let selected = WorkspacePackageSet::discover(fixture.path()).unwrap();
    let oracle = source_call(&serde_json::json!({"op":"discover", "root":fixture.path()}));
    assert_eq!(
        oracle["value"],
        serde_json::json!({"packages":selected.packages, "baseVersion":selected.base_version})
    );
    for (path, value) in [
        (
            "apps/cli/package.json",
            serde_json::json!({"name":"upstream", "version":"1.2.3"}),
        ),
        (
            "apps/cli/package.json",
            serde_json::json!({"name":"@seekdeep-ai/seekdeep-root", "version":"1.2.3"}),
        ),
        (
            "apps/cli/package.json",
            serde_json::json!({"name":"@seekdeep-ai/base", "version":"1.2.3"}),
        ),
        (
            "apps/cli/package.json",
            serde_json::json!({"name":"@seekdeep-ai/seekdeep", "version":"9.2.3"}),
        ),
        (
            "apps/cli/package.json",
            serde_json::json!({"name":"@seekdeep-ai/seekdeep", "version":null}),
        ),
        ("package.json", serde_json::json!({"version":"1.2.3-rc.1"})),
    ] {
        let fixture = support::workspace();
        support::write_json(&fixture.path().join(path), &value);
        let error = WorkspacePackageSet::discover(fixture.path())
            .unwrap_err()
            .to_string();
        assert_eq!(
            source_call(&serde_json::json!({"op":"discover", "root":fixture.path()}))["error"],
            error
        );
    }
    let empty = tempfile::tempdir().unwrap();
    let error = WorkspacePackageSet::discover(empty.path())
        .unwrap_err()
        .to_string();
    assert_eq!(
        source_call(&serde_json::json!({"op":"discover", "root":empty.path()}))["error"],
        error
    );
}
