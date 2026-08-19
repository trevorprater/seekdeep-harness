//! The sandbox-escalation API shared by the write and edit tools.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_fs::{FS, FsError, FsErrorCode};
use seekdeep_sandbox::{
    ESCALATION_TARGETS, SandboxExecutionPolicy, SandboxMode, escalation_hint_marker,
    sandbox_denial_marker,
};
use seekdeep_sandbox_policy::{SANDBOX_POLICY, SandboxPolicyService};
use serde_json::{Value, json};

/// The two escalation arguments a mutating tool may carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FsEscalationArgs {
    /// The wider sandbox mode this file operation needs.
    pub sandbox_permissions: Option<String>,
    /// One-sentence justification for the wider access.
    pub justification: Option<String>,
}

/// The schema fields for the escalation arguments, spread into a tool's
/// parameters when a confining backend is mounted.
#[derive(Clone, Debug, PartialEq)]
pub struct EscalationSchemaFields {
    /// The closed-target escalation enum field.
    pub sandbox_permissions: Value,
    /// The justification field.
    pub justification: Value,
}

/// The filesystem escalation API: advertisement gating, per-call policy
/// resolution, and denial-marker mapping.
pub struct FsSandboxController {
    /// The escalation targets this composition advertises.
    pub escalation_modes: Vec<SandboxMode>,
    policy: Option<Arc<SandboxPolicyService>>,
}

impl std::fmt::Debug for FsSandboxController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FsSandboxController")
            .field("escalation_modes", &self.escalation_modes)
            .field("policy", &self.policy.as_ref().map(|_| ".."))
            .finish_non_exhaustive()
    }
}

impl FsSandboxController {
    /// Builds the controller from the mounted filesystem's confinement fact.
    ///
    /// # Errors
    ///
    /// Returns when a confining filesystem is mounted without a sandbox policy.
    pub fn new(context: &Context) -> anyhow::Result<Self> {
        let fs = context
            .get(FS)
            .ok_or_else(|| anyhow::anyhow!("tool-fs requires fs"))?;
        let default_mode = fs.filesystem().sandbox_mode();
        let (escalation_modes, policy) = if default_mode.is_none() {
            (Vec::new(), None)
        } else {
            let policy = context.get(SANDBOX_POLICY).ok_or_else(|| {
                anyhow::anyhow!(
                    "tool-fs: the mounted filesystem confines but ctx.sandboxPolicy is missing"
                )
            })?;
            (ESCALATION_TARGETS.to_vec(), Some(policy))
        };
        Ok(Self {
            escalation_modes,
            policy,
        })
    }

    /// The escalation schema fields for a mutating tool's parameters.
    #[must_use]
    pub fn schema_fields(&self) -> EscalationSchemaFields {
        EscalationSchemaFields {
            sandbox_permissions: json!({
                "type": "string",
                "enum": self.escalation_modes.iter().map(|mode| mode.as_str()).collect::<Vec<_>>(),
                "description": "The wider sandbox mode this file operation needs. Only valid as a one-shot retry of an operation the sandbox just denied; requires justification and user approval.",
            }),
            justification: json!({
                "type": "string",
                "description": "Required with sandbox_permissions: one sentence for the user explaining why this exact file operation needs the wider access.",
            }),
        }
    }

    /// Maps a thrown provider error for the model: a sandbox denial becomes a
    /// marker `FsError`, anything else passes through unchanged.
    ///
    /// # Panics
    ///
    /// Panics when a sandbox denial is mapped without a resolved policy, which
    /// cannot happen under a confining backend.
    #[must_use]
    pub fn map_error(
        &self,
        error: anyhow::Error,
        policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Error {
        let Some(fs_error) = error.downcast_ref::<FsError>() else {
            return error;
        };
        if fs_error.code != FsErrorCode::FsSandboxDenied {
            return error;
        }
        let mode = policy
            .expect("confining backend always resolves a policy")
            .mode;
        anyhow::Error::new(FsError::new(
            format!(
                "{}
{}",
                sandbox_denial_marker(mode),
                escalation_hint_marker("operation")
            ),
            FsErrorCode::FsSandboxDenied,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_error_rewrites_only_sandbox_denials() {
        let controller = FsSandboxController {
            escalation_modes: vec![],
            policy: None,
        };
        let policy = SandboxExecutionPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: "/ws".into(),
            session_id: None,
        };
        let denial = anyhow::Error::new(FsError::new("denied", FsErrorCode::FsSandboxDenied));
        let mapped = controller.map_error(denial, Some(&policy));
        let fs_error = mapped.downcast_ref::<FsError>().expect("FsError");
        assert_eq!(fs_error.code, FsErrorCode::FsSandboxDenied);
        assert!(
            fs_error
                .message
                .contains("[sandbox: file access denied under workspace-write mode]")
        );
        assert!(fs_error.message.contains("[sandbox: escalation available"));

        let other = anyhow::Error::new(FsError::new("io", FsErrorCode::FsIoError));
        let passed = controller.map_error(other, Some(&policy));
        let fs_error = passed.downcast_ref::<FsError>().expect("FsError");
        assert_eq!(fs_error.message, "io");
    }
}
