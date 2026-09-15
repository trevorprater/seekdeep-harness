//! Real launcher binary CLI checks; enforcement world proofs run on Linux only.

use std::process::Command;

use seekdeep_landlock_run::LAUNCHER_FAILURE_EXIT;

fn launcher() -> &'static str {
    env!("CARGO_BIN_EXE_landlock-run")
}

#[test]
fn usage_failures_exit_125_with_launcher_owned_diagnostics_before_restriction() {
    let cases: &[(&[&str], &str)] = &[
        (&[], "usage error: missing `-- <argv>...` command"),
        (
            &["--bogus", "--", "true"],
            "usage error: unknown argument: --bogus",
        ),
        (&["--ro"], "usage error: --ro requires a path"),
        (
            &["--probe", "--ro", "/"],
            "--probe takes no other arguments",
        ),
        (&["--probe", "--"], "--probe takes no other arguments"),
        (&["--probe", "--probe"], "--probe takes no other arguments"),
    ];
    for (args, expected) in cases {
        let output = Command::new(launcher()).args(*args).output().unwrap();
        assert_eq!(output.status.code(), Some(LAUNCHER_FAILURE_EXIT));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with("landlock-run: "));
        assert!(stderr.contains(expected), "{stderr:?}");
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_host_probe_fails_closed_without_executing_any_command() {
    let output = Command::new(launcher()).arg("--probe").output().unwrap();
    assert_eq!(output.status.code(), Some(LAUNCHER_FAILURE_EXIT));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "landlock-run: landlock is not enforced by this kernel (ABI unsupported or disabled)\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn real_kernel_exec_exit_grants_denials_inheritance_and_fail_closed_behavior() {
    use std::{fs, time::Duration};

    use seekdeep_landlock_run::{
        LandlockEnforcement, LauncherGrants, PARTIAL_NOTICE, grant_args, probe,
    };

    let enforcement = probe(launcher().as_ref(), Duration::from_secs(2));
    if enforcement == LandlockEnforcement::Unusable {
        assert_ne!(
            std::env::var("SEEKDEEP_REQUIRE_LANDLOCK").as_deref(),
            Ok("1"),
            "SEEKDEEP_REQUIRE_LANDLOCK=1 but the functional probe is unusable"
        );
        return;
    }
    let expected_notice = if enforcement == LandlockEnforcement::Partial {
        format!("{PARTIAL_NOTICE}\n")
    } else {
        String::new()
    };
    let probe_output = Command::new(launcher()).arg("--probe").output().unwrap();
    assert!(probe_output.status.success());
    let probe_stdout = String::from_utf8(probe_output.stdout).unwrap();
    assert!(
        probe_stdout == "landlock: fully enforced\n"
            || probe_stdout == "landlock: partially enforced (older ABI)\n"
    );

    let run = |grants: LauncherGrants, script: &str| {
        Command::new(launcher())
            .args(grant_args(&grants))
            .args(["--", "/bin/sh", "-c", script])
            .output()
            .unwrap()
    };
    let readable = LauncherGrants {
        read_only: vec!["/".into()],
        read_write: Vec::new(),
    };
    let output = run(readable.clone(), "echo confined-ok");
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, b"confined-ok\n");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), expected_notice);
    let output = run(readable.clone(), "exit 7");
    assert_eq!(output.status.code(), Some(7));
    let output = run(readable.clone(), "exit 125");
    assert_eq!(output.status.code(), Some(LAUNCHER_FAILURE_EXIT));
    assert_eq!(String::from_utf8(output.stderr).unwrap(), expected_notice);

    let directory = tempfile::tempdir().unwrap();
    let denied = directory.path().join("denied.txt");
    let output = run(readable.clone(), &format!("echo x > {}", denied.display()));
    assert!(!output.status.success());
    assert!(!denied.exists());

    let granted = directory.path().join("granted.txt");
    let output = run(
        LauncherGrants {
            read_only: vec!["/".into()],
            read_write: vec![directory.path().to_owned()],
        },
        &format!("echo ok > {}", granted.display()),
    );
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(fs::read_to_string(granted).unwrap(), "ok\n");

    let nested = directory.path().join("nested.txt");
    let output = run(
        readable,
        &format!("/bin/sh -c 'echo x > {}'; true", nested.display()),
    );
    assert!(output.status.success());
    assert!(!nested.exists());

    let marker = directory.path().join("must-not-run.txt");
    let output = Command::new(launcher())
        .args(["--ro", "/no/such/grant/root", "--", "/bin/sh", "-c"])
        .arg(format!("echo x > {}", marker.display()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(LAUNCHER_FAILURE_EXIT));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("landlock-run: "));
    assert!(stderr.contains("cannot open rule path"));
    assert!(!marker.exists());
}
