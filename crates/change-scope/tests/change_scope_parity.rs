//! Source-oracle parity over isolated Git repositories and raw path bytes.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Report {
    format_version: u32,
    repository_root: String,
    input: ReportInput,
    resolved: Resolved,
    paths: Paths,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ReportInput {
    base: String,
    head: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Resolved {
    #[serde(rename = "baseSha")]
    base: String,
    #[serde(rename = "headSha")]
    head: String,
    #[serde(rename = "mergeBaseSha")]
    merge_base: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Paths {
    committed: Vec<String>,
    staged: Vec<String>,
    unstaged: Vec<String>,
    untracked: Vec<String>,
}

struct Fixture {
    container: tempfile::TempDir,
    root: PathBuf,
}

fn git_bytes(cwd: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git(cwd: &Path, args: &[&str]) -> String {
    String::from_utf8(git_bytes(cwd, args, None))
        .unwrap()
        .trim()
        .to_owned()
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    write(path, content);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn fixture(worktree_name: &str) -> Fixture {
    let container = tempfile::tempdir().unwrap();
    let origin = container.path().join("origin.git");
    let root = container.path().join(worktree_name);
    let hooks = container.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    git(
        container.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=master",
            origin.to_str().unwrap(),
        ],
    );
    git(
        container.path(),
        &["init", "--initial-branch=master", root.to_str().unwrap()],
    );
    git(&root, &["config", "user.email", "change-scope@example.com"]);
    git(&root, &["config", "user.name", "Change Scope Tests"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    git(
        &root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    write(&root.join("README.md"), "# Fixture\n");
    git(&root, &["add", "README.md"]);
    git(&root, &["commit", "-m", "initial"]);
    git(
        &root,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&root, &["push", "--set-upstream", "origin", "master"]);
    Fixture { container, root }
}

fn commit(root: &Path, path: &str, content: &str) -> String {
    write(&root.join(path), content);
    git(root, &["add", "--", path]);
    git(root, &["commit", "-m", &format!("add {path}")]);
    git(root, &["rev-parse", "HEAD"])
}

fn render(root: &Path, base: &str, head: Option<&str>) -> String {
    let mut args = vec!["--base".to_owned(), base.to_owned()];
    if let Some(head) = head {
        args.extend(["--head".to_owned(), head.to_owned()]);
    }
    seekdeep_change_scope::render_change_scope(&args, root).unwrap()
}

fn report(root: &Path, base: &str, head: Option<&str>) -> Report {
    serde_json::from_str(&render(root, base, head)).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct RepositoryState {
    status: Vec<u8>,
    head: String,
    refs: String,
    index: Vec<u8>,
    config: Vec<u8>,
}

fn repository_state(root: &Path) -> RepositoryState {
    RepositoryState {
        status: git_bytes(root, &["status", "--porcelain=v2", "--branch", "-z"], None),
        head: git(root, &["rev-parse", "HEAD"]),
        refs: git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        index: fs::read(root.join(".git/index")).unwrap(),
        config: fs::read(root.join(".git/config")).unwrap(),
    }
}

#[test]
fn explicit_base_works_before_and_after_the_first_same_name_push() {
    let fixture = fixture("worktree");
    git(&fixture.root, &["switch", "-c", "feature"]);
    git(
        &fixture.root,
        &["branch", "--set-upstream-to=origin/master"],
    );
    let head_sha = commit(&fixture.root, "feature.txt", "feature\n");

    let fresh = report(&fixture.root, "origin/master", None);
    assert_eq!(
        fs::canonicalize(&fresh.repository_root).unwrap(),
        fs::canonicalize(&fixture.root).unwrap()
    );
    assert_eq!(
        fresh.resolved,
        Resolved {
            base: git(&fixture.root, &["rev-parse", "origin/master"]),
            head: head_sha,
            merge_base: git(&fixture.root, &["rev-parse", "origin/master"]),
        }
    );
    assert_eq!(
        fresh.paths,
        Paths {
            committed: vec!["feature.txt".into()],
            staged: vec![],
            unstaged: vec![],
            untracked: vec![],
        }
    );
    assert!(
        git(
            &fixture.root,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/remotes/origin/feature"
            ]
        )
        .is_empty()
    );

    git(
        &fixture.root,
        &["push", "--set-upstream", "origin", "feature"],
    );
    assert_eq!(
        report(&fixture.root, "origin/master", None).paths.committed,
        ["feature.txt"]
    );
}

#[cfg(not(windows))]
#[test]
fn repository_root_preserves_legal_trailing_spaces() {
    let fixture = fixture("worktree ");
    let report = report(&fixture.root, "HEAD", None);
    assert_eq!(
        fs::canonicalize(&report.repository_root).unwrap(),
        fs::canonicalize(&fixture.root).unwrap()
    );
    assert_eq!(
        report.paths,
        Paths {
            committed: vec![],
            staged: vec![],
            unstaged: vec![],
            untracked: vec![],
        }
    );
}

#[test]
fn exact_head_above_a_stacked_base_keeps_dirty_paths_worktree_local() {
    let fixture = fixture("worktree");
    git(&fixture.root, &["switch", "-c", "foundation"]);
    let base_sha = commit(&fixture.root, "foundation.txt", "foundation\n");
    git(&fixture.root, &["switch", "-c", "topic"]);
    let head_sha = commit(&fixture.root, "topic.txt", "topic\n");
    commit(&fixture.root, "later.txt", "later\n");
    write(
        &fixture.root.join("current-worktree.txt"),
        "current worktree\n",
    );

    let report = report(&fixture.root, "foundation", Some(&head_sha));
    assert_eq!(
        report.input,
        ReportInput {
            base: "foundation".into(),
            head: head_sha.clone(),
        }
    );
    assert_eq!(
        report.resolved,
        Resolved {
            base: base_sha.clone(),
            head: head_sha,
            merge_base: base_sha,
        }
    );
    assert_eq!(report.paths.committed, ["topic.txt"]);
    assert_eq!(report.paths.untracked, ["current-worktree.txt"]);
}

#[test]
fn dirty_layers_are_independent_and_reporting_does_not_mutate_repository_state() {
    let fixture = fixture("worktree");
    commit(&fixture.root, "unstaged.txt", "before\n");
    let base_sha = git(&fixture.root, &["rev-parse", "HEAD"]);
    commit(&fixture.root, "committed.txt", "committed\n");
    write(&fixture.root.join("staged.txt"), "staged\n");
    write(&fixture.root.join("mixed.txt"), "staged part\n");
    git(&fixture.root, &["add", "staged.txt", "mixed.txt"]);
    write(
        &fixture.root.join("mixed.txt"),
        "staged part\nunstaged part\n",
    );
    write(&fixture.root.join("unstaged.txt"), "unstaged\n");
    write(&fixture.root.join("untracked.txt"), "untracked\n");
    let before = repository_state(&fixture.root);

    let report = report(&fixture.root, &base_sha, None);
    assert_eq!(
        report.paths,
        Paths {
            committed: vec!["committed.txt".into()],
            staged: vec!["mixed.txt".into(), "staged.txt".into()],
            unstaged: vec!["mixed.txt".into(), "unstaged.txt".into()],
            untracked: vec!["untracked.txt".into()],
        }
    );
    assert_eq!(repository_state(&fixture.root), before);
}

#[cfg(unix)]
#[test]
fn configured_filesystem_monitor_is_never_executed() {
    let fixture = fixture("worktree");
    let monitor = fixture.container.path().join("fsmonitor.sh");
    let side_effect = fixture.container.path().join("fsmonitor.sh.ran");
    write_executable(&monitor, "#!/bin/sh\ntouch \"$0.ran\"\n");
    git(
        &fixture.root,
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );

    let report = report(&fixture.root, "HEAD", None);
    assert_eq!(
        report.paths,
        Paths {
            committed: vec![],
            staged: vec![],
            unstaged: vec![],
            untracked: vec![],
        }
    );
    assert!(!side_effect.exists());
}

#[cfg(unix)]
#[test]
fn distinct_non_utf8_git_paths_fail_instead_of_collapsing_lossily() {
    let fixture = fixture("worktree");
    let blob_sha = String::from_utf8(git_bytes(
        &fixture.root,
        &["hash-object", "-w", "--stdin"],
        Some(b"content"),
    ))
    .unwrap()
    .trim()
    .to_owned();
    let entry = format!("100644 {blob_sha}\t").into_bytes();
    let mut index = Vec::new();
    index.extend(&entry);
    index.extend([0x80, 0]);
    index.extend(&entry);
    index.extend([0x81, 0]);
    git_bytes(
        &fixture.root,
        &["update-index", "-z", "--index-info"],
        Some(&index),
    );

    let error = seekdeep_change_scope::render_change_scope(
        &["--base".into(), "HEAD".into()],
        &fixture.root,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot inspect staged paths: Git path 1 is not valid UTF-8")
    );
}

#[test]
fn missing_ambiguous_and_non_commit_refs_fail_loudly() {
    let fixture = fixture("worktree");
    git(&fixture.root, &["branch", "collision"]);
    git(&fixture.root, &["tag", "collision"]);
    write(&fixture.root.join("blob.txt"), "blob\n");
    let blob_sha = git(&fixture.root, &["hash-object", "-w", "blob.txt"]);
    git(&fixture.root, &["tag", "blob-ref", &blob_sha]);

    for (args, expected) in [
        (
            vec!["--base".into(), "missing".into()],
            "base ref \"missing\" does not resolve to a commit",
        ),
        (
            vec!["--base".into(), "collision".into()],
            "base ref \"collision\" is ambiguous",
        ),
        (
            vec!["--base".into(), "blob-ref".into()],
            "base ref \"blob-ref\" does not resolve to a commit",
        ),
        (
            vec![
                "--base".into(),
                "HEAD".into(),
                "--head".into(),
                "missing".into(),
            ],
            "head ref \"missing\" does not resolve to a commit",
        ),
    ] {
        let error = seekdeep_change_scope::render_change_scope(&args, &fixture.root).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn report_json_is_deterministic_and_versioned() {
    let fixture = fixture("worktree");
    git(&fixture.root, &["switch", "-c", "format"]);
    commit(&fixture.root, "zeta.txt", "zeta\n");
    commit(&fixture.root, "alpha.txt", "alpha\n");

    let json = render(&fixture.root, "origin/master", None);
    assert_eq!(json, render(&fixture.root, "origin/master", None));
    let report: Report = serde_json::from_str(&json).unwrap();
    assert_eq!(report.format_version, 1);
    assert_eq!(report.paths.committed, ["alpha.txt", "zeta.txt"]);
}
