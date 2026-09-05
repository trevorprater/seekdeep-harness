//! Source configuration defaults and validation parity.

use seekdeep_terminal_bash::TerminalBashConfig;

fn config() -> TerminalBashConfig {
    TerminalBashConfig {
        shell_args: Vec::new(),
        rows: 40.0,
        cols: 160.0,
        scrollback_lines: 100.0,
        scrollback_max_bytes: 1_024.0,
        max_read_bytes: 512.0,
        poll_interval_ms: 10.0,
        exact_probe_after_ms: 20.0,
        idle_silence_ms: 100.0,
        handoff_grace_ms: 50.0,
        timeout_ms: 1_000.0,
        dispose_grace_ms: 100.0,
        ..TerminalBashConfig::default()
    }
}

#[test]
fn accepts_resolved_positive_bounds_and_defaults() {
    let defaults = TerminalBashConfig::default();
    assert_eq!(defaults.backend_type, "shell");
    assert_eq!(defaults.shell_path, "/bin/bash");
    assert_eq!(defaults.shell_args, ["--noprofile", "--norc", "-i"]);
    config().resolve().expect("valid config");
}

#[test]
fn rejects_empty_names_invalid_numbers_and_composed_bounds() {
    for (mut invalid, field) in [
        (
            TerminalBashConfig {
                backend_type: String::new(),
                ..config()
            },
            "backendType",
        ),
        (
            TerminalBashConfig {
                shell_path: String::new(),
                ..config()
            },
            "shellPath",
        ),
        (
            TerminalBashConfig {
                rows: 0.0,
                ..config()
            },
            "rows",
        ),
        (
            TerminalBashConfig {
                rows: 1.5,
                ..config()
            },
            "rows",
        ),
    ] {
        assert!(
            invalid
                .resolve()
                .expect_err("invalid")
                .to_string()
                .contains(field)
        );
        invalid.rows = 40.0;
    }
    let oversized = TerminalBashConfig {
        max_read_bytes: 2_048.0,
        ..config()
    };
    assert!(
        oversized
            .resolve()
            .expect_err("read cap")
            .to_string()
            .contains("must not exceed")
    );
}

#[test]
fn rejects_handoff_grace_shorter_than_one_readiness_poll() {
    let too_short = TerminalBashConfig {
        handoff_grace_ms: 9.0,
        poll_interval_ms: 10.0,
        ..config()
    };
    assert!(
        too_short
            .resolve()
            .expect_err("handoff grace")
            .to_string()
            .contains("handoffGraceMs must be at least pollIntervalMs")
    );
    TerminalBashConfig {
        handoff_grace_ms: 10.0,
        poll_interval_ms: 10.0,
        ..config()
    }
    .resolve()
    .expect("equal bound");
}
