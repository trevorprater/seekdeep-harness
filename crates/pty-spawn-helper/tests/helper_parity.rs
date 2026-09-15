//! Child-process cwd, argv, environment, and failure behavior of the native helper.

#![cfg(unix)]

use std::process::Command;

const HELPER: &str = env!("CARGO_BIN_EXE_seekdeep-pty-spawn-helper");

#[test]
fn helper_preserves_arguments_environment_and_requested_cwd() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let output = Command::new(HELPER)
        .arg(&root)
        .args([
            "/bin/sh",
            "-c",
            "printf '%s|%s|%s' \"$PWD\" \"$HELPER_PROOF\" \"$1\"",
            "helper",
            "two words",
        ])
        .env("HELPER_PROOF", "inherited")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}|inherited|two words", root.display())
    );
}

#[test]
fn empty_cwd_and_path_search_preserve_the_callers_working_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let output = Command::new(HELPER)
        .current_dir(&root)
        .env("PATH", "/bin:/usr/bin")
        .args(["", "sh", "-c", "printf '%s' \"$PWD\""])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        root.to_string_lossy()
    );
}

#[test]
fn helper_returns_one_without_output_when_chdir_or_exec_fails() {
    for args in [
        vec!["/nonexistent-seekdeep-helper-cwd", "/bin/echo", "never"],
        vec!["", "/nonexistent-seekdeep-helper-program"],
        vec![],
    ] {
        let output = Command::new(HELPER).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}
