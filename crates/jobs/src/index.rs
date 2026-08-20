//! The background-job Service Definition (ctx.jobs).

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_agent::Agent;
use seekdeep_cordis::{Context, Service, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;

use crate::{
    brand::JobId,
    types::{JobDoneListener, JobKillOutcome, JobRead, JobSnapshot, JobStart, JobsChangedListener},
};

/// Typed Cordis slot for the job registry.
pub const JOBS: ServiceKey<JobRegistryService> = ServiceKey::new("jobs");

/// Wrapper publishing a concrete job registry on the seam slot.
#[derive(Clone)]
pub struct JobRegistryService(Arc<dyn JobRegistry>);

impl JobRegistryService {
    /// Wraps one concrete registry.
    #[must_use]
    pub fn new(registry: Arc<dyn JobRegistry>) -> Arc<Self> {
        Arc::new(Self(registry))
    }

    /// Returns the wrapped object-safe registry.
    #[must_use]
    pub fn registry(&self) -> Arc<dyn JobRegistry> {
        self.0.clone()
    }

    /// Publishes this registry on the seam slot.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(JOBS, self.clone())
    }
}

impl std::ops::Deref for JobRegistryService {
    type Target = dyn JobRegistry;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// Abstract background job registry contract.
pub trait JobRegistry: Service + Send + Sync {
    /// Preflight access, validation, and owner cleanup before starting and
    /// atomically registering work.
    fn start(&self, spec: JobStart) -> JobId;

    /// List caller-owned and unowned jobs in registration order.
    fn list(&self, caller: Option<&Arc<Agent>>) -> Vec<JobSnapshot>;

    /// Return a non-consuming snapshot without changing read cursor or notice
    /// state.
    ///
    /// # Errors
    ///
    /// Returns for an unknown or foreign job.
    fn get(&self, id: &JobId, caller: Option<&Arc<Agent>>) -> anyhow::Result<JobSnapshot>;

    /// Read the next stream delta, or the idempotent final output after
    /// settlement.
    ///
    /// # Errors
    ///
    /// Returns for an unknown or foreign job.
    fn read(&self, id: &JobId, caller: Option<&Arc<Agent>>) -> anyhow::Result<JobRead>;

    /// Request cancellation, then mark the job stopping and reported.
    ///
    /// # Errors
    ///
    /// Returns for an unknown or foreign job.
    fn kill(
        &self,
        id: &JobId,
        caller: Option<&Arc<Agent>>,
        reason: Option<&str>,
    ) -> anyhow::Result<JobKillOutcome>;

    /// Wait for settlement or timeout without cancelling the job.
    ///
    /// # Errors
    ///
    /// Returns for invalid, unknown, or foreign input.
    fn wait(
        &self,
        id: &JobId,
        timeout_ms: u64,
        caller: Option<&Arc<Agent>>,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'static, anyhow::Result<JobSnapshot>>;

    /// Register an effect-scoped completion listener.
    fn on_job_done(&self, listener: JobDoneListener) -> EffectHandle;

    /// Register an effect-scoped observer of visible-set changes.
    fn on_jobs_changed(&self, listener: JobsChangedListener) -> EffectHandle;

    /// Attach an effect-scoped controller that can read and stop jobs.
    fn attach_controller(&self, name: &str) -> EffectHandle;
}
