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
        if let Err(error) = validate_snapshot(snapshot, owner, &failure) {
            panic!("{error}");
        }
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

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Mutex},
    };

    use futures::future::BoxFuture;
    use seekdeep_agent::{Agent, AgentOptions, Inbox, InboxNotifications, NoopInboxNotifications};
    use seekdeep_cordis::{Context, fiber::EffectHandle};
    use seekdeep_core::session::{Session, SessionId};
    use seekdeep_invariants::InvariantConfig;
    use seekdeep_llm::AbortSignal;
    use seekdeep_scope::ScopeKey;

    use super::*;
    use crate::{
        JobDoneListener, JobId, JobKillOutcome, JobRead, JobRegistry, JobRegistryService,
        JobSnapshot, JobStart, JobStatus, JobsChangedListener,
    };

    fn base() -> JobSnapshot {
        JobSnapshot {
            id: JobId::new("bash-1"),
            kind: "bash".to_owned(),
            label: "compile".to_owned(),
            output_limit_bytes: None,
            owner_session: None,
            status: JobStatus::Completed,
            detail: None,
            started_at: 10,
            finished_at: Some(20),
            reported: false,
        }
    }

    fn running() -> JobSnapshot {
        JobSnapshot {
            status: JobStatus::Running,
            finished_at: None,
            ..base()
        }
    }

    fn terminal_without_finish() -> JobSnapshot {
        JobSnapshot {
            finished_at: None,
            ..base()
        }
    }

    fn stub_agent(context: &Context, id: &str) -> Arc<Agent> {
        let session_id = SessionId::new(id);
        let session = Session::create(&session_id, None, None).expect("session");
        let notifications: Arc<dyn InboxNotifications> = Arc::new(NoopInboxNotifications);
        let inbox = Arc::new(Inbox::new(session.clone(), notifications).expect("inbox"));
        Arc::new(Agent::new(
            session_id,
            AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ))
    }

    /// Minimal concrete registry that captures the completion listener.
    struct Probe {
        seed: Vec<JobSnapshot>,
        listener: Arc<Mutex<Option<JobDoneListener>>>,
    }

    impl JobRegistry for Probe {
        fn start(&self, _spec: JobStart) -> JobId {
            JobId::new("bash-1")
        }

        fn list(&self, _caller: Option<&Arc<Agent>>) -> Vec<JobSnapshot> {
            self.seed.clone()
        }

        fn get(&self, _id: &JobId, _caller: Option<&Arc<Agent>>) -> anyhow::Result<JobSnapshot> {
            Ok(base())
        }

        fn read(&self, _id: &JobId, _caller: Option<&Arc<Agent>>) -> anyhow::Result<JobRead> {
            Ok(JobRead {
                text: String::new(),
                snapshot: base(),
            })
        }

        fn kill(
            &self,
            _id: &JobId,
            _caller: Option<&Arc<Agent>>,
            _reason: Option<&str>,
        ) -> anyhow::Result<JobKillOutcome> {
            Ok(JobKillOutcome::Requested)
        }

        fn wait(
            &self,
            _id: &JobId,
            _timeout_ms: f64,
            _caller: Option<&Arc<Agent>>,
            _signal: Option<AbortSignal>,
        ) -> BoxFuture<'static, anyhow::Result<JobSnapshot>> {
            Box::pin(async { Ok(base()) })
        }

        fn on_job_done(&self, listener: JobDoneListener) -> EffectHandle {
            *self.listener.lock().expect("lock") = Some(listener);
            EffectHandle::synchronous("probe.onJobDone", || Ok(()))
        }

        fn on_jobs_changed(&self, _listener: JobsChangedListener) -> EffectHandle {
            EffectHandle::synchronous("probe.onJobsChanged", || Ok(()))
        }

        fn attach_controller(&self, _name: &str) -> EffectHandle {
            EffectHandle::synchronous("probe.attachController", || Ok(()))
        }
    }

    async fn setup(seed: Vec<JobSnapshot>) -> anyhow::Result<Arc<Mutex<Option<JobDoneListener>>>> {
        let ctx = Context::new();
        let registry = InvariantRegistry::install(&ctx, &InvariantConfig::default())?;
        let listener = Arc::new(Mutex::new(None));
        let probe = Arc::new(Probe {
            seed,
            listener: listener.clone(),
        });
        let service = JobRegistryService::new(probe);
        service.provide(&ctx)?;
        let registration = register_invariant(&registry)?;
        registration.await_ready().await?;
        Ok(listener)
    }

    fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
        payload
            .downcast_ref::<String>()
            .map_or_else(|| "non-string panic".to_owned(), Clone::clone)
    }

    #[tokio::test]
    async fn accepts_coherent_current_and_terminal_snapshots() {
        let listener = setup(vec![running()]).await.expect("setup");
        let listener = listener.lock().expect("lock").clone().expect("listener");

        let ctx = Context::new();
        let owner = stub_agent(&ctx, "owner");
        assert!(catch_unwind(AssertUnwindSafe(|| listener(&base(), None))).is_ok());
        let owned_snapshot = JobSnapshot {
            id: JobId::new("subagent-2"),
            kind: "subagent".to_owned(),
            owner_session: Some(owner.id().clone()),
            ..base()
        };
        assert!(catch_unwind(AssertUnwindSafe(|| listener(&owned_snapshot, Some(&owner)))).is_ok());
    }

    #[tokio::test]
    async fn rejects_incoherent_snapshots() {
        let listener = setup(Vec::new()).await.expect("setup");
        let listener = listener.lock().expect("lock").clone().expect("listener");
        let ctx = Context::new();

        let empty_kind = JobSnapshot {
            id: JobId::new("-1"),
            kind: String::new(),
            ..base()
        };
        let wrong_prefix = JobSnapshot {
            id: JobId::new("other-1"),
            ..base()
        };
        let non_numeric = JobSnapshot {
            id: JobId::new("bash-x"),
            ..base()
        };
        let zero_ordinal = JobSnapshot {
            id: JobId::new("bash-0"),
            ..base()
        };
        let running_with_finish = JobSnapshot {
            status: JobStatus::Running,
            ..base()
        };
        let finished_before_start = JobSnapshot {
            finished_at: Some(9),
            ..base()
        };
        let huge_started = JobSnapshot {
            started_at: 9_007_199_254_740_992,
            finished_at: Some(9_007_199_254_740_992),
            ..base()
        };

        for (snapshot, expected) in [
            (empty_kind, "positive ordinal"),
            (
                wrong_prefix,
                "must be \"bash-\" followed by a positive ordinal",
            ),
            (non_numeric, "positive ordinal"),
            (zero_ordinal, "positive ordinal"),
            (
                running_with_finish,
                "finishedAt must be present exactly for a terminal status",
            ),
            (
                terminal_without_finish(),
                "finishedAt must be present exactly for a terminal status",
            ),
            (finished_before_start, "no earlier than startedAt"),
            (
                huge_started,
                "startedAt must be a non-negative epoch integer",
            ),
        ] {
            let result = catch_unwind(AssertUnwindSafe(|| listener(&snapshot, None)));
            let message = panic_message(&result.expect_err("must reject"));
            assert!(message.contains(expected), "{message:?} !~ {expected:?}");
        }

        let recorded = SessionId::new("recorded");
        let actual = stub_agent(&ctx, "actual");
        let mismatched_owner = JobSnapshot {
            owner_session: Some(recorded),
            ..base()
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            listener(&mismatched_owner, Some(&actual))
        }));
        let message = panic_message(&result.expect_err("owner mismatch"));
        assert!(
            message.contains("does not match its completion owner"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn rejects_incoherent_record_present_at_installation() {
        let seed = vec![JobSnapshot {
            label: String::new(),
            ..base()
        }];
        let error = setup(seed).await.err().expect("must reject at install");
        assert!(
            format!("{error:#}").contains("label must be non-empty"),
            "{error:#}"
        );
    }
}
