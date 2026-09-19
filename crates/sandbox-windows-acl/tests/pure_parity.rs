//! Cross-platform pure contract parity for the Windows backend.

use std::path::PathBuf;

use seekdeep_sandbox_windows_acl::{
    AclSandboxMode, AclSandboxOptions, Win32Error, assert_private_temp_disjoint,
    assert_temp_root_outside_workspace, build_command_line, parse_runner_args, quote_arg,
    temp_write_sid, validate_runner_args, workspace_write_sid,
};

#[test]
fn win32_error_name_fields_and_exact_message_are_preserved() {
    let error = Win32Error::new("CreateRestrictedToken", 5, Some("denied".into()));
    assert_eq!(error.api, "CreateRestrictedToken");
    assert_eq!(error.win32_code, 5);
    assert_eq!(error.detail(), Some("denied"));
    assert_eq!(
        error.to_string(),
        "CreateRestrictedToken failed (Win32 5): denied"
    );
    assert_eq!(
        Win32Error::new("LocalFree", 87, None).to_string(),
        "LocalFree failed (Win32 87)"
    );
}

#[test]
fn workspace_and_temp_sids_are_stable_distinct_byte_sensitive_and_capability_shaped() {
    let first = workspace_write_sid("C:\\Users\\agent\\repo");
    assert_eq!(first, workspace_write_sid("C:\\Users\\agent\\repo"));
    assert!(first.starts_with("S-1-4-"));
    assert_eq!(first.split('-').count(), 5);
    assert_ne!(
        workspace_write_sid("C:\\Users\\agent\\repo-a"),
        workspace_write_sid("C:\\Users\\agent\\repo-b")
    );
    assert_ne!(
        workspace_write_sid("C:\\Repo"),
        workspace_write_sid("c:\\repo")
    );
    assert_ne!(
        workspace_write_sid("C:\\Repo\\"),
        workspace_write_sid("C:\\Repo")
    );
    let temp = temp_write_sid("C:\\Temp\\seekdeep-a");
    assert_eq!(temp, temp_write_sid("C:\\Temp\\seekdeep-a"));
    assert!(temp.ends_with("-1"));
    assert_ne!(temp, workspace_write_sid("C:\\Temp\\seekdeep-a"));
    assert_ne!(temp, temp_write_sid("C:\\Temp\\seekdeep-b"));
}

#[test]
fn canonical_temp_boundaries_reject_containment_in_either_direction() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let nested = workspace.join("temp");
    let sibling = root.path().join("sibling");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&nested).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    assert!(assert_temp_root_outside_workspace(&workspace, &workspace).is_err());
    assert!(assert_temp_root_outside_workspace(&workspace, &nested).is_err());
    assert_temp_root_outside_workspace(&workspace, root.path()).unwrap();
    assert!(assert_private_temp_disjoint(std::slice::from_ref(&workspace), &nested).is_err());
    assert!(assert_private_temp_disjoint(std::slice::from_ref(&nested), &workspace).is_err());
    assert_private_temp_disjoint(&[workspace], &sibling).unwrap();
}

#[test]
fn quote_table_and_command_line_match_command_line_to_argv_rules() {
    let cases = [
        ("", "\"\""),
        ("a", "a"),
        ("a b", "\"a b\""),
        ("a\"b", "\"a\\\"b\""),
        ("a\\b", "a\\b"),
        ("a b\\", "\"a b\\\\\""),
        ("a b\\\\", "\"a b\\\\\\\\\""),
        ("a b\\\\\\", "\"a b\\\\\\\\\\\\\""),
        ("a\\\\\"b", "\"a\\\\\\\\\\\"b\""),
    ];
    for (input, expected) in cases {
        assert_eq!(quote_arg(input), expected);
    }
    assert_eq!(
        build_command_line("prog.exe", &["a b".into(), "plain".into()]),
        "prog.exe \"a b\" plain"
    );
}

#[test]
fn runner_parser_preserves_exact_grammar_and_diagnostics() {
    let parsed = parse_runner_args(
        &[
            "--workspace",
            "/workspace",
            "--temp",
            "/tmp",
            "--mode",
            "read-only",
            "--",
            "pwsh",
            "/Command",
            "x",
        ]
        .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(parsed.mode, AclSandboxMode::ReadOnly);
    assert_eq!(parsed.command, ["pwsh", "/Command", "x"]);
    let cases = [
        (vec!["--workspace"], "missing value after --workspace"),
        (vec!["--bogus", "x"], "unknown argument: --bogus"),
        (vec![], "missing --workspace"),
    ];
    for (args, expected) in cases {
        let error = parse_runner_args(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.diagnostic(), format!("windows-acl-run: {expected}"));
    }
}

#[test]
fn runner_validation_checks_directories_pairs_boundaries_and_derived_sids() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let temp_root = tempfile::tempdir().unwrap();
    let temp = temp_root.path().join("private");
    std::fs::create_dir(&temp).unwrap();
    let workspace_text = workspace.to_str().unwrap();
    let temp_text = temp.to_str().unwrap();
    let mut parsed = parse_runner_args(&[
        "--workspace".into(),
        workspace_text.into(),
        "--temp".into(),
        temp_text.into(),
        "--mode".into(),
        "workspace-write".into(),
        "--write-sid".into(),
        workspace_write_sid(workspace_text),
        "--temp-write-sid".into(),
        temp_write_sid(temp_text),
        "--".into(),
        "true".into(),
    ])
    .unwrap();
    validate_runner_args(&parsed).unwrap();
    parsed.write_sid = Some("S-1-4-1-1".into());
    assert_eq!(
        validate_runner_args(&parsed).unwrap_err().to_string(),
        "--write-sid does not match --workspace"
    );
}

#[test]
fn constructor_shape_rejects_missing_mismatched_and_overlapping_capabilities() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let base = AclSandboxOptions {
        writable_dirs: vec![workspace.path().to_owned()],
        temp_dir: Some(outside.path().to_owned()),
        temp_was_explicit: true,
        write_sid: Some("S-1-4-1-2".into()),
        temp_write_sid: Some("S-1-4-3-4-1".into()),
        mode: AclSandboxMode::WorkspaceWrite,
        manage_dacls: true,
    };
    assert!(base.resolve().is_ok());
    let mut missing_write = base.clone();
    missing_write.write_sid = None;
    assert!(
        missing_write
            .resolve()
            .unwrap_err()
            .to_string()
            .contains("requires a write SID")
    );
    let mut equal = base.clone();
    equal.temp_write_sid = equal.write_sid.clone();
    assert!(
        equal
            .resolve()
            .unwrap_err()
            .to_string()
            .contains("must be distinct")
    );
    let read_only = AclSandboxOptions {
        writable_dirs: Vec::new(),
        temp_dir: None,
        temp_was_explicit: false,
        write_sid: None,
        temp_write_sid: None,
        mode: AclSandboxMode::ReadOnly,
        manage_dacls: true,
    };
    assert!(read_only.resolve().is_ok());
    let mut invalid_read_only = read_only;
    invalid_read_only.temp_dir = Some(PathBuf::from(outside.path()));
    assert!(invalid_read_only.resolve().is_err());
}
