//! Strict-widening and approval choreography parity.

use std::sync::Mutex;

use async_trait::async_trait;
use seekdeep_sandbox::{
    ESCALATION_TARGETS, EscalationApproval, EscalationApprover, EscalationAsk, EscalationOutcome,
    EscalationRequest, SandboxMode, approve_escalation, escalation_hint_marker,
    sandbox_denial_marker, validate_escalation_args,
};

#[derive(Debug)]
struct Approver {
    outcome: EscalationOutcome,
    seen: Mutex<Vec<EscalationAsk<(), String>>>,
}

#[async_trait]
impl EscalationApprover<(), String> for Approver {
    async fn request(&self, request: EscalationAsk<(), String>) -> EscalationOutcome {
        self.seen.lock().unwrap().push(request);
        self.outcome
    }
}

fn request() -> EscalationRequest {
    EscalationRequest {
        requested_mode: "workspace-write".into(),
        justification: "the user asked to write in the workspace".into(),
        effective_mode: SandboxMode::ReadOnly,
        subject: "command".into(),
    }
}

fn approval(approver: Option<&Approver>) -> EscalationApproval<'_, (), String> {
    EscalationApproval {
        approver: approver.map(|value| value as &dyn EscalationApprover<(), String>),
        agent: Some(()),
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        signal: None,
    }
}

#[test]
fn wider_vocabulary_validation_and_markers_are_exact() {
    assert_eq!(
        ESCALATION_TARGETS,
        [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess]
    );
    validate_escalation_args(None, None).unwrap();
    validate_escalation_args(Some("workspace-write"), Some("because it is needed")).unwrap();
    assert!(
        validate_escalation_args(Some("workspace-write"), None)
            .unwrap_err()
            .to_string()
            .contains("requires a justification")
    );
    assert!(
        validate_escalation_args(None, Some("reason"))
            .unwrap_err()
            .to_string()
            .contains("only valid together")
    );
    assert!(
        validate_escalation_args(Some("workspace-write"), Some("   "))
            .unwrap_err()
            .to_string()
            .contains("non-empty sentence")
    );
    assert_eq!(
        sandbox_denial_marker(SandboxMode::ReadOnly),
        "[sandbox: file access denied under read-only mode]"
    );
    assert!(
        escalation_hint_marker("command")
            .contains("retry this exact command once with sandbox_permissions")
    );
}

#[tokio::test]
async fn grant_is_audited_and_fail_closed_paths_are_distinct() {
    let allowed = Approver {
        outcome: EscalationOutcome::AllowedOnce,
        seen: Mutex::new(Vec::new()),
    };
    assert_eq!(
        approve_escalation(request(), approval(Some(&allowed)))
            .await
            .unwrap(),
        SandboxMode::WorkspaceWrite
    );
    assert_eq!(
        allowed.seen.lock().unwrap()[0].reason,
        "escalate sandbox to workspace-write: the user asked to write in the workspace"
    );

    let mut bad = request();
    bad.requested_mode = "read-only".into();
    let error = approve_escalation(bad, approval(Some(&allowed)))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not strictly wider"));
    assert_eq!(allowed.seen.lock().unwrap().len(), 1);

    let error = approve_escalation(request(), approval(None))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no approval service is composed")
    );

    let mut no_agent = approval(Some(&allowed));
    no_agent.agent = None;
    let error = approve_escalation(request(), no_agent).await.unwrap_err();
    assert!(error.to_string().contains("no agent to route it through"));
}

#[tokio::test]
async fn every_non_grant_outcome_maps_to_its_exact_failure() {
    for (outcome, expected) in [
        (
            EscalationOutcome::Rejected,
            "the user rejected escalating this command to \"workspace-write\"",
        ),
        (
            EscalationOutcome::Cancelled,
            "approval for escalating to \"workspace-write\" was cancelled",
        ),
        (
            EscalationOutcome::Unavailable,
            "no approval channel is available",
        ),
    ] {
        let approver = Approver {
            outcome,
            seen: Mutex::new(Vec::new()),
        };
        let error = approve_escalation(request(), approval(Some(&approver)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected));
    }
}
