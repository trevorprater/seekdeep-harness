//! Shared sandbox-escalation vocabulary and ordered fail-closed choreography.

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;

use crate::SandboxMode;

/// Modes each effective mode may strictly widen to, in source order.
pub const WIDER_MODES: [(SandboxMode, &[SandboxMode]); 2] = [
    (
        SandboxMode::ReadOnly,
        &[SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess],
    ),
    (
        SandboxMode::WorkspaceWrite,
        &[SandboxMode::DangerFullAccess],
    ),
];

/// Every mode an execution can escalate to (`read-only` is the floor).
pub const ESCALATION_TARGETS: [SandboxMode; 2] =
    [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess];

/// Returns the strictly wider targets for an effective mode.
#[must_use]
pub fn wider_modes(mode: SandboxMode) -> &'static [SandboxMode] {
    match mode {
        SandboxMode::ReadOnly => WIDER_MODES[0].1,
        SandboxMode::WorkspaceWrite => WIDER_MODES[1].1,
        SandboxMode::DangerFullAccess => &[],
    }
}

/// Validates the schema-inexpressible argument pairing.
///
/// # Errors
///
/// Returns the source diagnostic when only one field is present or the reason is blank.
pub fn validate_escalation_args(
    sandbox_permissions: Option<&str>,
    justification: Option<&str>,
) -> anyhow::Result<()> {
    if sandbox_permissions.is_some() && justification.is_none() {
        anyhow::bail!("invalid escalation: sandbox_permissions requires a justification");
    }
    if justification.is_some() && sandbox_permissions.is_none() {
        anyhow::bail!(
            "invalid escalation: justification is only valid together with sandbox_permissions"
        );
    }
    if justification.is_some_and(|value| value.trim().is_empty()) {
        anyhow::bail!("invalid justification: expected a non-empty sentence");
    }
    Ok(())
}

/// Exact model-facing denial marker.
#[must_use]
pub fn sandbox_denial_marker(mode: SandboxMode) -> String {
    format!("[sandbox: file access denied under {mode} mode]")
}

/// Exact same-turn model-facing escalation hint.
#[must_use]
pub fn escalation_hint_marker(subject: &str) -> String {
    format!(
        "[sandbox: escalation available — retry this exact {subject} once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    )
}

/// Closed result of one approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// Grant applies to this call only.
    AllowedOnce,
    /// Human rejected the request.
    Rejected,
    /// The pending request was cancelled.
    Cancelled,
    /// No channel could answer it.
    Unavailable,
}

/// Audit-self-contained approval request passed to the approval seam.
#[derive(Clone, Debug)]
pub struct EscalationAsk<A, C> {
    /// Exact calling agent identity.
    pub agent: A,
    /// Tool name recorded in the audit event.
    pub tool_name: String,
    /// Exact tool-call identity.
    pub call_id: C,
    /// Target-bearing audit reason.
    pub reason: String,
    /// Tool-execution cancellation carried to the approval channel.
    pub signal: Option<AbortSignal>,
}

/// Minimal structural approval seam.
#[async_trait]
pub trait EscalationApprover<A, C>: Send + Sync {
    /// Requests a one-call decision.
    async fn request(&self, request: EscalationAsk<A, C>) -> EscalationOutcome;
}

/// Ingredients held by an escalating tool.
pub struct EscalationApproval<'a, A, C> {
    /// Composed approval seam, absent when unavailable.
    pub approver: Option<&'a dyn EscalationApprover<A, C>>,
    /// Calling agent, absent for agentless execution.
    pub agent: Option<A>,
    /// Tool-call identity.
    pub call_id: C,
    /// Tool name.
    pub tool_name: String,
    /// Tool-execution cancellation carried to the approval channel.
    pub signal: Option<AbortSignal>,
}

/// One escalation request to judge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscalationRequest {
    /// Untrusted requested target spelling.
    pub requested_mode: String,
    /// Verbatim model reason.
    pub justification: String,
    /// Effective policy for this call.
    pub effective_mode: SandboxMode,
    /// Family noun (`command` or `operation`).
    pub subject: String,
}

/// Resolves strict widening and human approval before any execution.
///
/// # Errors
///
/// Returns distinct source-compatible diagnostics for every fail-closed path.
pub async fn approve_escalation<A, C>(
    request: EscalationRequest,
    approval: EscalationApproval<'_, A, C>,
) -> anyhow::Result<SandboxMode>
where
    A: Send,
    C: Send,
{
    let Some(requested) = SandboxMode::parse(&request.requested_mode)
        .filter(|mode| wider_modes(request.effective_mode).contains(mode))
    else {
        anyhow::bail!(
            "sandbox escalation to \"{}\" is not strictly wider than this call's current \"{}\" mode",
            request.requested_mode,
            request.effective_mode
        );
    };
    let Some(approver) = approval.approver else {
        anyhow::bail!(
            "sandbox escalation to \"{}\" requires approval, but no approval service is composed",
            request.requested_mode
        );
    };
    let Some(agent) = approval.agent else {
        anyhow::bail!(
            "sandbox escalation to \"{}\" requires approval, but the call has no agent to route it through",
            request.requested_mode
        );
    };
    let outcome = approver
        .request(EscalationAsk {
            agent,
            tool_name: approval.tool_name,
            call_id: approval.call_id,
            reason: format!(
                "escalate sandbox to {}: {}",
                request.requested_mode, request.justification
            ),
            signal: approval.signal,
        })
        .await;
    match outcome {
        EscalationOutcome::AllowedOnce => Ok(requested),
        EscalationOutcome::Rejected => anyhow::bail!(
            "the user rejected escalating this {} to \"{}\"",
            request.subject,
            request.requested_mode
        ),
        EscalationOutcome::Cancelled => anyhow::bail!(
            "approval for escalating to \"{}\" was cancelled",
            request.requested_mode
        ),
        EscalationOutcome::Unavailable => anyhow::bail!(
            "sandbox escalation to \"{}\" requires approval, but no approval channel is available",
            request.requested_mode
        ),
    }
}
