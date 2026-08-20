//! Meta validation: re-checks the typed workflow meta against the
//! `WorkflowMeta` contract and rejects every violation by name.

use seekdeep_workflow::{WorkflowError, WorkflowErrorCode, WorkflowMeta};

/// Validate a caller-provided meta block against the `WorkflowMeta` contract.
///
/// The typed `WorkflowMeta` already enforces field presence, types, and the
/// closed field set through serde; this re-check enforces the non-emptiness
/// invariants the wire decoder cannot, returning a normalized copy so the
/// engine never aliases the caller's value.
///
/// # Errors
///
/// Returns a meta-invalid failure naming every violation.
pub fn validate_meta(meta: &WorkflowMeta) -> anyhow::Result<WorkflowMeta> {
    let mut violations = Vec::new();
    if meta.name.is_empty() {
        violations.push("meta.name must be a non-empty string".to_owned());
    }
    if meta.description.is_empty() {
        violations.push("meta.description must be a non-empty string".to_owned());
    }
    if let Some(phases) = &meta.phases {
        for (index, phase) in phases.iter().enumerate() {
            if phase.title.is_empty() {
                violations.push(format!(
                    "meta.phases[{index}].title must be a non-empty string"
                ));
            }
        }
    }
    if !violations.is_empty() {
        return Err(WorkflowError::new(
            format!("invalid meta: {}", violations.join("; ")),
            WorkflowErrorCode::MetaInvalid,
        )
        .into());
    }
    Ok(meta.clone())
}
