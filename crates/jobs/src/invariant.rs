//! Package-owned background-job snapshot invariants.

use std::sync::Arc;

use seekdeep_agent::Agent;
use seekdeep_cordis::Context;
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};

use crate::{index::JOBS, types::JobSnapshot};

const PACKAGE_NAME: &str = "seekdeep-jobs";

/// Registers the job-snapshot invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["jobs"], |context, failure| {
            Box::pin(async move {
                install(&context, &failure)?;
                Ok(())
            })
        }),
    )
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let jobs = context
        .get(JOBS)
        .ok_or_else(|| failure.fail("seekdeep-jobs invariant requires jobs"))?;

    for snapshot in jobs.list(None) {
        validate_snapshot(&snapshot, None, failure)?;
    }

    let failure = failure.clone();
    jobs.on_job_done(Arc::new(move |snapshot, owner| {
        let _ = validate_snapshot(snapshot, owner, &failure);
    }));

    Ok(())
}

/// Validate the cross-field relationships in one registry snapshot.
fn validate_snapshot(
    snapshot: &JobSnapshot,
    owner: Option<&Arc<Agent>>,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let id = snapshot.id.as_str();
    let prefix = format!("{}-", snapshot.kind);
    let Some(ordinal) = id.strip_prefix(&prefix) else {
        return Err(failure
            .fail(format!(
                "job snapshot id {id:?} must be {prefix:?} followed by a positive ordinal"
            ))
            .into());
    };
    let ordinal = ordinal.parse::<u64>().ok();
    if snapshot.kind.is_empty() || ordinal.is_none_or(|value| value < 1) {
        return Err(failure
            .fail(format!(
                "job snapshot id {id:?} must be {prefix:?} followed by a positive ordinal"
            ))
            .into());
    }
    if snapshot.label.is_empty() {
        return Err(failure
            .fail(format!("job {id:?} label must be non-empty"))
            .into());
    }
    if snapshot.started_at > 9_007_199_254_740_991 {
        return Err(failure
            .fail(format!(
                "job {id:?} startedAt must be a non-negative epoch integer"
            ))
            .into());
    }

    let terminal = snapshot.status.is_terminal();
    if terminal != snapshot.finished_at.is_some() {
        return Err(failure
            .fail(format!(
                "job {id:?} finishedAt must be present exactly for a terminal status"
            ))
            .into());
    }
    if let Some(finished_at) = snapshot.finished_at
        && (finished_at > 9_007_199_254_740_991 || finished_at < snapshot.started_at)
    {
        return Err(failure
            .fail(format!(
                "job {id:?} finishedAt must be an epoch integer no earlier than startedAt"
            ))
            .into());
    }

    let expected_owner = owner.map(|owner| owner.id().clone());
    if snapshot.owner_session != expected_owner {
        return Err(failure
            .fail(format!(
                "job {id:?} ownerSession does not match its completion owner"
            ))
            .into());
    }
    Ok(())
}
