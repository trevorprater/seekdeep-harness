//! Pure entry API, resolution, probe, and CLI grammar parity.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use seekdeep_landlock_run::{
    LAUNCHER_BIN, LAUNCHER_FAILURE_EXIT, LandlockEnforcement, LauncherError, LauncherGrants,
    LauncherRequest, grant_args, launcher_path_from, parse_args, probe,
};

#[test]
fn constants_grants_and_sibling_resolution_are_exact_and_environment_independent() {
    assert_eq!(LAUNCHER_BIN, "landlock-run");
    assert_eq!(LAUNCHER_FAILURE_EXIT, 125);
    assert!(grant_args(&LauncherGrants::default()).is_empty());
    assert_eq!(
        grant_args(&LauncherGrants {
            read_only: vec!["/".into(), "/opt".into()],
            read_write: vec!["/tmp/work".into()],
        }),
        ["--ro", "/", "--ro", "/opt", "--rw", "/tmp/work"]
            .map(OsString::from)
            .to_vec()
    );
    assert_eq!(
        launcher_path_from(Path::new("/opt/seekdeep/bin/seekdeep")).unwrap(),
        PathBuf::from("/opt/seekdeep/bin/landlock-run")
    );
}

#[test]
fn parser_preserves_exact_grammar_order_and_usage_diagnostics() {
    assert_eq!(parse_args(["--probe"]).unwrap(), LauncherRequest::Probe);
    assert_eq!(
        parse_args(["--ro", "/", "--rw", "/tmp", "--", "sh", "-c", "true"]).unwrap(),
        LauncherRequest::Run {
            grants: LauncherGrants {
                read_only: vec!["/".into()],
                read_write: vec!["/tmp".into()],
            },
            command: ["sh", "-c", "true"].map(OsString::from).to_vec(),
        }
    );
    let cases = [
        (
            Vec::<&str>::new(),
            "usage error: missing `-- <argv>...` command",
        ),
        (
            vec!["--bogus", "--", "true"],
            "usage error: unknown argument: --bogus",
        ),
        (vec!["--ro"], "usage error: --ro requires a path"),
        (
            vec!["--probe", "--ro", "/"],
            "usage error: --probe takes no other arguments",
        ),
        (
            vec!["--probe", "--"],
            "usage error: --probe takes no other arguments",
        ),
        (
            vec!["--probe", "--probe"],
            "usage error: --probe takes no other arguments",
        ),
    ];
    for (args, expected) in cases {
        let error = parse_args(args).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.diagnostic(), format!("landlock-run: {expected}"));
        assert!(matches!(error, LauncherError::Usage(_)));
    }
}

#[cfg(unix)]
#[test]
fn probe_classifies_missing_full_partial_failure_and_timeout() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let fake = |name: &str, script: &str| {
        let path = directory.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    assert_eq!(
        probe(&directory.path().join("missing"), Duration::from_secs(1)),
        LandlockEnforcement::Unusable
    );
    assert_eq!(
        probe(
            &fake("full", "echo 'landlock: fully enforced'; exit 0"),
            Duration::from_secs(1)
        ),
        LandlockEnforcement::Full
    );
    assert_eq!(
        probe(
            &fake(
                "partial",
                "echo 'landlock: partially enforced (older ABI)'; exit 0"
            ),
            Duration::from_secs(1)
        ),
        LandlockEnforcement::Partial
    );
    assert_eq!(
        probe(&fake("failure", "exit 125"), Duration::from_secs(1)),
        LandlockEnforcement::Unusable
    );
    assert_eq!(
        probe(&fake("timeout", "sleep 10"), Duration::from_millis(50)),
        LandlockEnforcement::Unusable
    );
}
