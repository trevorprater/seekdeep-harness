//! Sandbox diagnostic-classification parity.

use std::{io, path::Path};

use seekdeep_bash_local::BashSpawnFailure;
use seekdeep_bash_sandbox::{classify_runner_failure, is_runner_spawn_failure, matches_signature};
use seekdeep_sandbox::RunnerFailureRule;

fn rule(
    allowed_exit_codes: Option<Vec<i32>>,
    fatal_signatures: &[&str],
    informational_lines: Option<Vec<String>>,
) -> RunnerFailureRule {
    RunnerFailureRule {
        allowed_exit_codes,
        fatal_signatures: fatal_signatures
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        informational_lines,
    }
}

#[test]
fn spawn_attribution_requires_runner_provenance_usable_cwd_and_known_error_kind() {
    let directory = tempfile::tempdir().expect("workdir");
    let runner = Some("/missing/sandbox-runner");
    for detail in ["runner not found", "runner permission denied"] {
        let error = anyhow::Error::new(BashSpawnFailure::new(detail));
        assert!(is_runner_spawn_failure(&error, runner, directory.path()));
    }

    let foreign = anyhow::Error::new(io::Error::other("broken pipe"));
    assert!(!is_runner_spawn_failure(&foreign, runner, directory.path()));
    let missing = anyhow::Error::new(BashSpawnFailure::new("missing"));
    assert!(!is_runner_spawn_failure(&missing, None, directory.path()));
    assert!(!is_runner_spawn_failure(
        &missing,
        runner,
        Path::new("/nonexistent-seekdeep-bash-sandbox-cwd")
    ));
}

#[test]
fn runner_failure_rules_honor_exit_gates_informational_lines_and_case() {
    let rules = vec![rule(
        Some(vec![127]),
        &["runner fatal", "   "],
        Some(vec!["runner partial".to_owned()]),
    )];
    assert_eq!(
        classify_runner_failure(
            Some(127),
            "RUNNER PARTIAL\nRunner Fatal: backend unavailable\n",
            &rules
        )
        .expect("fatal match")
        .detail,
        "Runner Fatal: backend unavailable"
    );
    assert!(classify_runner_failure(Some(0), "runner fatal", &rules).is_none());
    assert!(classify_runner_failure(None, "runner fatal", &rules).is_none());
    assert!(classify_runner_failure(Some(126), "runner fatal", &rules).is_none());
    assert!(classify_runner_failure(Some(127), "runner partial", &rules).is_none());
    assert!(classify_runner_failure(Some(127), "anything", &[rule(None, &["  "], None)]).is_none());
}

#[test]
fn denial_signatures_require_a_nonzero_ordinary_exit() {
    let signatures = vec!["access denied".to_owned()];
    assert!(matches_signature(
        Some(1),
        "ACCESS DENIED by policy",
        &signatures
    ));
    assert!(!matches_signature(Some(0), "access denied", &signatures));
    assert!(!matches_signature(None, "access denied", &signatures));
    assert!(!matches_signature(Some(1), "other failure", &signatures));
}
