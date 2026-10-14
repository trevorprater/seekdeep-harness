//! Pure entry API, resolution, probe, and CLI grammar parity.

use std::{
    ffi::OsString,
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
    let executable = std::env::current_exe().unwrap();
    let directory = tempfile::tempdir_in(executable.parent().unwrap()).unwrap();
    let fixture = directory.path().join("probe-fixture");
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(compiler)
        .arg("--edition=2024")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe_fixture.rs"))
        .arg("-o")
        .arg(&fixture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Match the immutable native launcher. Building in a child process avoids
    // sharing a writable executable fd with concurrently forked test children.
    let fake = |name: &str| {
        let path = directory.path().join(name);
        std::os::unix::fs::symlink(&fixture, &path).unwrap();
        path
    };
    assert_eq!(
        probe(&directory.path().join("missing"), Duration::from_secs(1)),
        LandlockEnforcement::Unusable
    );
    assert_eq!(
        probe(&fake("full"), Duration::from_secs(1)),
        LandlockEnforcement::Full
    );
    assert_eq!(
        probe(&fake("partial"), Duration::from_secs(1)),
        LandlockEnforcement::Partial
    );
    assert_eq!(
        probe(&fake("failure"), Duration::from_secs(1)),
        LandlockEnforcement::Unusable
    );
    assert_eq!(
        probe(&fake("timeout"), Duration::from_millis(50)),
        LandlockEnforcement::Unusable
    );
}
