//! Closed sandbox vocabulary and structured error parity.

use seekdeep_sandbox::{
    ConfinedArgv, ConfinedSandboxMode, RunnerFailureRule, SANDBOX_UNAVAILABLE, SandboxEnforcement,
    SandboxMode, SandboxUnavailableError,
};

#[test]
fn mode_wire_vocabulary_is_closed_and_exact() {
    assert_eq!(SandboxMode::parse("read-only"), Some(SandboxMode::ReadOnly));
    assert_eq!(
        SandboxMode::parse("workspace-write"),
        Some(SandboxMode::WorkspaceWrite)
    );
    assert_eq!(
        SandboxMode::parse("danger-full-access"),
        Some(SandboxMode::DangerFullAccess)
    );
    assert_eq!(SandboxMode::parse("host-root"), None);
    assert!(ConfinedSandboxMode::try_from(SandboxMode::DangerFullAccess).is_err());
}

#[test]
fn confined_result_wire_fields_and_optional_rule_fields_are_exact() {
    let value = ConfinedArgv {
        argv: vec!["sandbox".into(), "--".into(), "bash".into()],
        enforcement: SandboxEnforcement::Partial,
        denial_signatures: vec!["operation not permitted".into()],
        runner_failure_rules: vec![RunnerFailureRule {
            allowed_exit_codes: Some(vec![1, 125]),
            fatal_signatures: vec!["runner failed".into()],
            informational_lines: None,
        }],
    };
    assert_eq!(
        serde_json::to_value(&value).unwrap(),
        serde_json::json!({
            "argv": ["sandbox", "--", "bash"],
            "enforcement": "partial",
            "denialSignatures": ["operation not permitted"],
            "runnerFailureRules": [{
                "allowedExitCodes": [1, 125],
                "fatalSignatures": ["runner failed"]
            }]
        })
    );
    assert_eq!(
        serde_json::from_value::<ConfinedArgv>(serde_json::to_value(&value).unwrap()).unwrap(),
        value
    );
}

#[test]
fn unavailable_error_preserves_name_code_message_and_optional_runner_detail() {
    let error = SandboxUnavailableError::new(ConfinedSandboxMode::ReadOnly, None);
    assert_eq!(error.name(), "SandboxUnavailableError");
    assert_eq!(error.code(), SANDBOX_UNAVAILABLE);
    assert!(error.message().contains("\"read-only\""));
    assert!(error.message().contains("danger-full-access"));
    assert!(!error.message().contains("Runner failure"));

    let error = SandboxUnavailableError::new(
        ConfinedSandboxMode::ReadOnly,
        Some("landlock-run: landlock is not enforced by this kernel"),
    );
    assert!(
        error
            .message()
            .contains("Runner failure: landlock-run: landlock is not enforced by this kernel")
    );
}
