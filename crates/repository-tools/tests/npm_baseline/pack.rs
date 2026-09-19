use std::path::Path;

use seekdeep_repository_tools::npm_baseline::{
    BaselinePackOptions, ReleaseBundle, pack_baseline, plan_baseline,
};

use super::support::{
    NpmFixtureRunner, environment, git, initialize_git, now, source_call, workspace,
};

fn options(root: &Path) -> BaselinePackOptions {
    BaselinePackOptions {
        reference: "HEAD".to_owned(),
        registry: "http://127.0.0.1:9///".to_owned(),
        output_directory: root.to_owned(),
    }
}

#[test]
fn fixed_plan_matches_source_commit_timestamp_version_tag_and_output() {
    let root = workspace();
    initialize_git(root.path());
    let output = tempfile::tempdir().unwrap();
    let mut runner = NpmFixtureRunner::default();
    let plan = plan_baseline(root.path(), &options(output.path()), now(), &mut runner).unwrap();
    assert_eq!(plan.timestamp, "20260911010203");
    assert_eq!(plan.short_commit.len(), 10);
    assert_eq!(plan.dist_tag, "dev-1.2.3");
    assert_eq!(plan.registry, "http://127.0.0.1:9");
    let source = source_call(
        &serde_json::json!({"op":"plan","root":root.path(),"output":output.path(),"registry":"http://127.0.0.1:9///","now":"2026-09-11T01:02:03.456Z"}),
    );
    assert_eq!(
        source["value"],
        serde_json::json!({"commit":plan.commit,"shortCommit":plan.short_commit,"timestamp":plan.timestamp,"baseVersion":plan.base_version,"version":plan.version,"distTag":plan.dist_tag,"registry":plan.registry,"artifactDirectory":plan.artifact_directory})
    );
    std::fs::create_dir_all(&plan.artifact_directory).unwrap();
    std::fs::write(plan.artifact_directory.join("keep"), "keep").unwrap();
    let error = plan_baseline(root.path(), &options(output.path()), now(), &mut runner)
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("output already exists:"));
    assert!(
        pack_baseline(root.path(), &plan, &environment(), &mut runner)
            .unwrap_err()
            .to_string()
            .starts_with("output already exists:")
    );
    assert_eq!(
        std::fs::read_to_string(plan.artifact_directory.join("keep")).unwrap(),
        "keep"
    );
    assert!(
        !runner
            .calls
            .iter()
            .any(|call| call.arguments.first().map(String::as_str) == Some("worktree"))
    );
}

#[test]
fn detached_pack_real_local_npm_install_and_pty_smoke_preserve_dirty_caller() {
    let root = workspace();
    let path = root.path().join("apps/cli/package.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest
        .as_object_mut()
        .unwrap()
        .shift_remove("optionalDependencies");
    super::support::write_json(&path, &manifest);
    initialize_git(root.path());
    std::fs::write(
        root.path().join("unrelated-dirty"),
        "preserve this caller work\n",
    )
    .unwrap();
    let before = git(root.path(), &["status", "--porcelain=v1"]);
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let output = tempfile::tempdir().unwrap();
    let mut runner = NpmFixtureRunner {
        real_install: true,
        real_probe: true,
        ..NpmFixtureRunner::default()
    };
    let plan = plan_baseline(root.path(), &options(output.path()), now(), &mut runner).unwrap();
    plan.confirm(&mut runner, true).unwrap();
    let mut environment = environment();
    environment.insert("npm_config_fetch_retries".into(), "0".into());
    environment.insert("npm_config_offline".into(), "true".into());
    environment.insert(
        "npm_config_cache".into(),
        output.path().join("npm-cache").into_os_string(),
    );
    let bundle = pack_baseline(root.path(), &plan, &environment, &mut runner).unwrap();
    assert_eq!(bundle.manifest.packages.len(), 3);
    assert_eq!(git(root.path(), &["status", "--porcelain=v1"]), before);
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(root.path(), &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    assert!(!runner.worktree.as_ref().unwrap().exists());
    assert!(!runner.consumer.as_ref().unwrap().exists());
    assert_eq!(
        ReleaseBundle::load(
            &bundle.directory.join("manifest.json"),
            &mut NpmFixtureRunner::default()
        )
        .unwrap(),
        bundle
    );
    let pnpm = runner
        .calls
        .iter()
        .filter(|call| call.command == "pnpm")
        .map(|call| call.arguments.join(" "))
        .collect::<Vec<_>>();
    assert_eq!(
        &pnpm[..5],
        [
            "install --frozen-lockfile",
            "run constraints",
            "run build",
            "run publint",
            "run verify-built-package-invariants"
        ]
    );
    assert!(pnpm[5].starts_with("--filter ./vendor/** --filter ./packages/** --filter ./apps/** --recursive pack --pack-destination "));
    assert!(
        runner
            .calls
            .iter()
            .filter(|call| call.command == "pnpm")
            .all(|call| call.cwd != root.path())
    );
    assert!(
        runner
            .logs
            .iter()
            .any(|line| line.contains("installed seekdeep entry and Web startup probes passed"))
    );
    assert!(
        !runner
            .calls
            .iter()
            .any(|call| call.command == "npm" && call.arguments[0] == "publish")
    );
}

#[test]
fn every_failed_pack_stage_removes_only_its_worktree_and_partial_bundle() {
    for failure in [
        "pnpm install",
        "pnpm run constraints",
        "pnpm run build",
        "pnpm run publint",
        "pnpm run verify-built-package-invariants",
        "pnpm --filter",
        "npm install",
        "node",
        "web_probe",
    ] {
        let root = workspace();
        initialize_git(root.path());
        std::fs::write(root.path().join("dirty"), "caller remains").unwrap();
        let before = git(root.path(), &["status", "--porcelain=v1"]);
        let output = tempfile::tempdir().unwrap();
        std::fs::write(output.path().join("unrelated"), "keep").unwrap();
        let mut runner = NpmFixtureRunner {
            fail: Some(failure.to_owned()),
            ..NpmFixtureRunner::default()
        };
        let plan = plan_baseline(root.path(), &options(output.path()), now(), &mut runner).unwrap();
        let error = pack_baseline(root.path(), &plan, &environment(), &mut runner)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("fixture") || error.contains("exited with status 17"),
            "{failure}: {error}"
        );
        assert!(!plan.artifact_directory.exists(), "{failure}");
        assert!(!runner.worktree.as_ref().unwrap().exists(), "{failure}");
        if let Some(consumer) = &runner.consumer {
            assert!(!consumer.exists(), "{failure}");
        }
        assert_eq!(git(root.path(), &["status", "--porcelain=v1"]), before);
        assert_eq!(
            std::fs::read_to_string(output.path().join("unrelated")).unwrap(),
            "keep"
        );
        assert_eq!(
            git(root.path(), &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }
}

#[test]
fn cancellation_happens_after_concrete_read_only_plan_and_before_worktree_creation() {
    let root = workspace();
    initialize_git(root.path());
    let output = tempfile::tempdir().unwrap();
    let mut runner = NpmFixtureRunner {
        confirmation_error: Some("pack cancelled".to_owned()),
        ..NpmFixtureRunner::default()
    };
    let plan = plan_baseline(root.path(), &options(output.path()), now(), &mut runner).unwrap();
    assert_eq!(
        plan.confirm(&mut runner, false).unwrap_err().to_string(),
        "pack cancelled"
    );
    assert_eq!(runner.confirmation_count, 1);
    assert_eq!(runner.calls.len(), 3);
    assert!(
        runner
            .logs
            .iter()
            .any(|line| line.contains(plan.version.as_str()))
    );
    assert!(!plan.artifact_directory.exists());
}
