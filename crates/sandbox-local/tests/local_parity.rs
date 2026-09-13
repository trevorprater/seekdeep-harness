//! Profile, configuration, runner ladder, caching, and fail-closed parity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[cfg(not(windows))]
use seekdeep_core::session::SessionId;
use seekdeep_landlock_run::{LAUNCHER_FAILURE_EXIT, LandlockEnforcement};
use seekdeep_sandbox::{
    ConfinedSandboxMode, SandboxEnforcement, SandboxPolicy, SandboxProvider,
    SandboxUnavailableError,
};
use seekdeep_sandbox_local::{
    LocalSandboxConfig, LocalSandboxProvider, LocalSandboxRunner, SandboxInternals,
    bwrap_profile_args, landlock_profile_args, seatbelt_profile_args,
};
#[cfg(not(windows))]
use std::path::PathBuf;

fn policy(mode: ConfinedSandboxMode, root: &str) -> SandboxPolicy {
    SandboxPolicy {
        mode,
        workspace_root: root.into(),
        session_id: None,
    }
}

#[test]
fn profile_dialects_are_exact_canonical_and_deduplicated() {
    let read_only = policy(ConfinedSandboxMode::ReadOnly, "/ws");
    let workspace = policy(ConfinedSandboxMode::WorkspaceWrite, "/ws");
    assert_eq!(
        bwrap_profile_args(&read_only).unwrap(),
        [
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--die-with-parent"
        ]
    );
    assert_eq!(
        bwrap_profile_args(&workspace).unwrap(),
        [
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--die-with-parent",
            "--tmpfs",
            "/tmp",
            "--bind",
            "/ws",
            "/ws"
        ]
    );
    assert_eq!(
        landlock_profile_args(&read_only).unwrap(),
        ["--ro", "/", "--rw", "/dev/null"]
    );
    assert_eq!(
        landlock_profile_args(&workspace).unwrap(),
        [
            "--ro",
            "/",
            "--rw",
            "/dev/null",
            "--rw",
            "/tmp",
            "--rw",
            "/ws"
        ]
    );
    assert_eq!(
        seatbelt_profile_args(&read_only).unwrap(),
        [
            "-p",
            "(version 1) (allow default) (deny file-write*) (allow file-write* (literal \"/dev/null\"))"
        ]
    );
    let temp = tempfile::tempdir().unwrap();
    let temp_policy = SandboxPolicy {
        mode: ConfinedSandboxMode::WorkspaceWrite,
        workspace_root: temp.path().to_owned(),
        session_id: None,
    };
    let profile = &seatbelt_profile_args(&temp_policy).unwrap()[1];
    let grant = format!(
        "(subpath \"{}\")",
        temp.path().canonicalize().unwrap().display()
    );
    assert_eq!(profile.matches(&grant).count(), 1);
}

#[test]
fn config_defaults_dependencies_signatures_and_probe_bounds_fail_early() {
    assert!((LocalSandboxConfig::default().probe_timeout_ms - 5_000.0).abs() < f64::EPSILON);
    let error = LocalSandboxProvider::new(&LocalSandboxConfig {
        runner_failure_signatures: vec!["profile rejected".into()],
        ..LocalSandboxConfig::default()
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires runnerCommand"));
    let error = LocalSandboxProvider::new(&LocalSandboxConfig {
        runner_command: vec!["runner".into()],
        ..LocalSandboxConfig::default()
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires at least one"));
    for signature in ["  ", "fatal\ncontinued", "fatal\rcontinued"] {
        let error = LocalSandboxProvider::new(&LocalSandboxConfig {
            runner_command: vec!["runner".into()],
            runner_failure_signatures: vec![signature.into()],
            ..LocalSandboxConfig::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("non-empty single-line"));
    }
    for timeout in [0.0, -1.0, f64::INFINITY, f64::NAN, 1.5] {
        let error = LocalSandboxProvider::new(&LocalSandboxConfig {
            probe_timeout_ms: timeout,
            ..LocalSandboxConfig::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("positive finite number"));
    }
}

#[test]
fn configured_runner_skips_the_chain_and_carries_operator_failure_dialect() {
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig {
        runner_command: vec!["fake-runner".into(), "--flag".into()],
        runner_failure_signatures: vec!["fake-runner: profile rejected".into()],
        ..LocalSandboxConfig::default()
    })
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    provider.set_internals(SandboxInternals {
        probe_bwrap: Some({
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::AcqRel);
                false
            })
        }),
        ..SandboxInternals::default()
    });
    let confined = provider
        .confine(
            &["bash".into(), "-c".into(), "echo hi".into()],
            &policy(ConfinedSandboxMode::WorkspaceWrite, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.argv[0..2], ["fake-runner", "--flag"]);
    assert_eq!(confined.enforcement, SandboxEnforcement::Full);
    assert_eq!(
        confined.denial_signatures,
        ["read-only file system", "permission denied"]
    );
    assert_eq!(
        confined.runner_failure_rules[0].fatal_signatures,
        ["fake-runner: profile rejected"]
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[test]
fn platform_ladder_order_dialects_partial_report_and_verdict_cache_match_source() {
    let bwrap_calls = Arc::new(AtomicUsize::new(0));
    let landlock_calls = Arc::new(AtomicUsize::new(0));
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    provider.set_internals(SandboxInternals {
        platform: Some("linux".into()),
        probe_bwrap: Some({
            let calls = bwrap_calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::AcqRel);
                false
            })
        }),
        probe_landlock: Some({
            let calls = landlock_calls.clone();
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::AcqRel);
                LandlockEnforcement::Partial
            })
        }),
        landlock_launcher: Some("/fake/landlock-run".into()),
        ..SandboxInternals::default()
    });
    let confined = provider
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.argv[0], "/fake/landlock-run");
    assert_eq!(confined.enforcement, SandboxEnforcement::Partial);
    assert_eq!(confined.denial_signatures, ["permission denied"]);
    assert_eq!(
        confined.runner_failure_rules[0].allowed_exit_codes,
        Some(vec![LAUNCHER_FAILURE_EXIT])
    );
    provider
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::WorkspaceWrite, "/ws"),
        )
        .unwrap();
    assert_eq!(bwrap_calls.load(Ordering::Acquire), 1);
    assert_eq!(landlock_calls.load(Ordering::Acquire), 1);

    let darwin = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    let seatbelt_calls = Arc::new(AtomicUsize::new(0));
    darwin.set_internals(SandboxInternals {
        platform: Some("darwin".into()),
        probe_seatbelt: Some({
            let calls = seatbelt_calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::AcqRel);
                false
            })
        }),
        ..SandboxInternals::default()
    });
    let confined = darwin
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.argv[0], "sandbox-exec");
    assert_eq!(confined.denial_signatures, ["operation not permitted"]);
    assert_eq!(seatbelt_calls.load(Ordering::Acquire), 0);
}

#[test]
fn unavailable_and_unknown_platform_verdicts_are_structured_and_cached() {
    let bwrap_calls = Arc::new(AtomicUsize::new(0));
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    provider.set_internals(SandboxInternals {
        platform: Some("linux".into()),
        probe_bwrap: Some({
            let calls = bwrap_calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::AcqRel);
                false
            })
        }),
        probe_landlock: Some(Arc::new(|_| LandlockEnforcement::Unusable)),
        ..SandboxInternals::default()
    });
    for _ in 0..2 {
        let error = provider
            .confine(
                &["true".into()],
                &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
            )
            .unwrap_err();
        let error = error.downcast_ref::<SandboxUnavailableError>().unwrap();
        assert_eq!(error.code(), "SANDBOX_UNAVAILABLE");
    }
    assert_eq!(bwrap_calls.load(Ordering::Acquire), 1);

    let unknown = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    unknown.set_internals(SandboxInternals {
        platform: Some("freebsd".into()),
        ..SandboxInternals::default()
    });
    assert!(
        unknown
            .confine(
                &["true".into()],
                &policy(ConfinedSandboxMode::ReadOnly, "/ws")
            )
            .unwrap_err()
            .is::<SandboxUnavailableError>()
    );
}

#[test]
fn multi_rung_injected_seatbelt_and_windows_acl_probe_paths_are_exhaustive() {
    let seatbelt = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    seatbelt.set_internals(SandboxInternals {
        chain: Some(vec![
            LocalSandboxRunner::Bwrap,
            LocalSandboxRunner::Seatbelt,
        ]),
        probe_bwrap: Some(Arc::new(|| false)),
        probe_seatbelt: Some(Arc::new(|| true)),
        seatbelt_exec: Some("/fake/sandbox-exec".into()),
        ..SandboxInternals::default()
    });
    let confined = seatbelt
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.argv[0], "/fake/sandbox-exec");

    let windows = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    windows.set_internals(SandboxInternals {
        chain: Some(vec![
            LocalSandboxRunner::WindowsAcl,
            LocalSandboxRunner::Bwrap,
        ]),
        probe_windows_acl: Some(Arc::new(|| true)),
        windows_acl_runner_args: Some(vec!["windows-acl-runner".into()]),
        ..SandboxInternals::default()
    });
    let confined = windows
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.enforcement, SandboxEnforcement::Partial);
    assert_eq!(confined.argv[0], "windows-acl-runner");
    assert_eq!(confined.argv[confined.argv.len() - 2..], ["--", "true"]);
}

#[test]
#[cfg(not(windows))]
fn session_workspace_write_on_windows_fails_closed_until_native_acl_grants_exist() {
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    provider.set_internals(SandboxInternals {
        platform: Some("win32".into()),
        windows_acl_runner_args: Some(vec!["windows-acl-runner".into()]),
        ..SandboxInternals::default()
    });
    let workspace = tempfile::tempdir().unwrap();
    let error = provider
        .confine(
            &["true".into()],
            &SandboxPolicy {
                mode: ConfinedSandboxMode::WorkspaceWrite,
                workspace_root: workspace.path().to_owned(),
                session_id: Some(SessionId::new("session")),
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("session grants are unavailable"));
}

#[test]
fn real_default_probes_are_bounded_and_never_claim_unconfined_success() {
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig {
        probe_timeout_ms: 250.0,
        ..LocalSandboxConfig::default()
    })
    .unwrap();
    provider.set_internals(SandboxInternals {
        platform: Some("linux".into()),
        ..SandboxInternals::default()
    });
    let started = std::time::Instant::now();
    let result = provider.confine(
        &["true".into()],
        &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    if let Ok(confined) = result {
        assert!(confined.argv[0].contains("bwrap") || confined.argv[0].contains("landlock-run"));
    }
}

#[test]
fn passing_first_rung_short_circuits_and_empty_custom_runner_uses_the_ladder() {
    let landlock_calls = Arc::new(AtomicUsize::new(0));
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig {
        runner_command: Vec::new(),
        ..LocalSandboxConfig::default()
    })
    .unwrap();
    provider.set_internals(SandboxInternals {
        platform: Some("linux".into()),
        probe_bwrap: Some(Arc::new(|| true)),
        probe_landlock: Some({
            let calls = landlock_calls.clone();
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::AcqRel);
                LandlockEnforcement::Full
            })
        }),
        ..SandboxInternals::default()
    });
    let confined = provider
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.argv[0], "bwrap");
    assert_eq!(confined.denial_signatures, ["read-only file system"]);
    assert_eq!(landlock_calls.load(Ordering::Acquire), 0);
}

#[cfg(unix)]
fn fake_executable(directory: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    let path = directory.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn default_landlock_probe_parses_full_partial_failure_and_timeout_reports() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        (
            fake_executable(
                &directory,
                "full",
                "echo 'landlock: fully enforced'; exit 0",
            ),
            1_000.0,
            Some(SandboxEnforcement::Full),
        ),
        (
            fake_executable(
                &directory,
                "partial",
                "echo 'landlock: partially enforced (older ABI)'; exit 0",
            ),
            1_000.0,
            Some(SandboxEnforcement::Partial),
        ),
        (
            fake_executable(&directory, "failure", "exit 125"),
            1_000.0,
            None,
        ),
        (
            fake_executable(
                &directory,
                "timeout",
                "sleep 0.25; echo 'landlock: fully enforced'; exit 0",
            ),
            50.0,
            None,
        ),
    ];
    for (launcher, timeout, expected) in cases {
        let provider = LocalSandboxProvider::new(&LocalSandboxConfig {
            probe_timeout_ms: timeout,
            ..LocalSandboxConfig::default()
        })
        .unwrap();
        provider.set_internals(SandboxInternals {
            platform: Some("linux".into()),
            probe_bwrap: Some(Arc::new(|| false)),
            landlock_launcher: Some(launcher),
            ..SandboxInternals::default()
        });
        let result = provider.confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        );
        match expected {
            Some(expected) => assert_eq!(result.unwrap().enforcement, expected),
            None => assert!(result.unwrap_err().is::<SandboxUnavailableError>()),
        }
    }
}

#[cfg(unix)]
#[test]
fn default_seatbelt_and_windows_probe_paths_select_or_fall_through() {
    let directory = tempfile::tempdir().unwrap();
    let success = fake_executable(&directory, "success", "exit 0");
    let failure = fake_executable(&directory, "failure", "exit 1");

    let seatbelt = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    seatbelt.set_internals(SandboxInternals {
        chain: Some(vec![
            LocalSandboxRunner::Bwrap,
            LocalSandboxRunner::Seatbelt,
        ]),
        probe_bwrap: Some(Arc::new(|| false)),
        seatbelt_exec: Some(success.to_string_lossy().into_owned()),
        ..SandboxInternals::default()
    });
    let confined = seatbelt
        .confine(
            &["true".into()],
            &policy(ConfinedSandboxMode::ReadOnly, "/ws"),
        )
        .unwrap();
    assert_eq!(confined.argv[0], success.to_string_lossy());

    for invocation in [
        vec![failure.to_string_lossy().into_owned()],
        Vec::<String>::new(),
    ] {
        let windows = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
        windows.set_internals(SandboxInternals {
            chain: Some(vec![
                LocalSandboxRunner::WindowsAcl,
                LocalSandboxRunner::Bwrap,
            ]),
            windows_acl_runner_args: Some(invocation),
            probe_bwrap: Some(Arc::new(|| true)),
            ..SandboxInternals::default()
        });
        assert_eq!(
            windows
                .confine(
                    &["true".into()],
                    &policy(ConfinedSandboxMode::ReadOnly, "/ws")
                )
                .unwrap()
                .argv[0],
            "bwrap"
        );
    }
}
