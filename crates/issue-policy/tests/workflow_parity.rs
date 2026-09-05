//! Trusted-checkout workflow subscription and Rust entry-point parity.

use std::{fs, path::Path, process::Command};

use serde_yml::Value;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

fn workflow(name: &str) -> (String, Value) {
    let source = fs::read_to_string(root().join(format!(".github/workflows/{name}.yml"))).unwrap();
    let value = serde_yml::from_str(&source).unwrap();
    (source, value)
}

fn sequence<'a>(value: &'a Value, pointer: &[&str]) -> Vec<&'a str> {
    let mut current = value;
    for key in pointer {
        current = current.get(*key).unwrap();
    }
    current
        .as_sequence()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

#[test]
fn policy_and_lifecycle_workflows_preserve_events_guards_and_trusted_rust_execution() {
    let (policy_source, policy) = workflow("issue-policy");
    let policy_events = sequence(&policy, &["on", "pull_request", "types"]);
    assert!(policy_events.contains(&"ready_for_review"));
    assert!(policy_events.contains(&"review_requested"));
    assert!(policy_source.contains("ref: ${{ github.event.repository.default_branch }}"));
    assert!(policy_source.contains("uses: dtolnay/rust-toolchain@1.93.1"));
    assert!(
        policy_source.contains(
            "CARGO_INCREMENTAL=0 cargo run --quiet --locked -p seekdeep-issue-policy -- pr"
        )
    );
    assert!(!policy_source.contains("policy.mjs"));

    let (lifecycle_source, lifecycle) = workflow("issue-lifecycle");
    let lifecycle_pr_events = sequence(&lifecycle, &["on", "pull_request", "types"]);
    assert!(!lifecycle_pr_events.contains(&"ready_for_review"));
    assert!(lifecycle_pr_events.contains(&"review_requested"));
    assert_eq!(
        lifecycle["jobs"]["lifecycle"]["if"].as_str(),
        Some(
            "${{ github.event_name != 'pull_request_review' || (github.event.action == 'submitted' && github.event.review.state == 'changes_requested') }}"
        )
    );
    assert!(lifecycle_source.contains("uses: dtolnay/rust-toolchain@1.93.1"));
    assert!(lifecycle_source.contains(
        "CARGO_INCREMENTAL=0 cargo run --quiet --locked -p seekdeep-issue-policy -- lifecycle"
    ));
    assert!(!lifecycle_source.contains("policy.mjs"));
}

#[test]
fn root_test_command_and_compiled_config_point_at_the_rust_owner() {
    let package: serde_json::Value =
        serde_json::from_slice(&fs::read(root().join("package.json")).unwrap()).unwrap();
    assert_eq!(
        package["scripts"]["test:issue-management"].as_str(),
        Some("cargo test -p seekdeep-issue-policy")
    );
    let config = seekdeep_issue_policy::IssuePolicyConfig::bundled().unwrap();
    assert_eq!(config.organization, "seekdeep-harness");
    assert_eq!(config.repository, "seekdeep-harness");
    assert_eq!(config.project_title, "SEEKDEEP Issue Management");
    assert_eq!(config.lifecycle_actor, "seekdeep-issue-management");
}

#[test]
fn built_command_preserves_usage_event_and_lazy_token_failure_order() {
    let binary = env!("CARGO_BIN_EXE_seekdeep-issue-policy");
    let usage = Command::new(binary)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_EVENT_PATH")
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(usage.stderr).unwrap().trim(),
        "用法：seekdeep-issue-policy pr|lifecycle"
    );

    let missing_event = Command::new(binary)
        .arg("pr")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_EVENT_PATH")
        .output()
        .unwrap();
    assert_eq!(missing_event.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(missing_event.stderr).unwrap().trim(),
        "GITHUB_EVENT_PATH 未设置"
    );

    let scratch = tempfile::tempdir().unwrap();
    let event = scratch.path().join("event.json");
    fs::write(&event, "{}").unwrap();
    let irrelevant = Command::new(binary)
        .arg("lifecycle")
        .env("GITHUB_EVENT_PATH", event)
        .env("GITHUB_EVENT_NAME", "workflow_dispatch")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(irrelevant.status.success());
    assert!(irrelevant.stdout.is_empty());
    assert!(irrelevant.stderr.is_empty());
}
