//! Process-local provider for the background-job capability seam (`ctx.jobs`).
//!
//! It keeps every record in memory and hands out fresh snapshots, never live
//! state. Registrations outlive producer and controller fibers; agent or
//! service disposal cancels live work and awaits compliant producers, while a
//! throwing teardown cancel force-fails only the record and reports a possible
//! orphan.

use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, Agent};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_jobs::{
    JobDoneListener, JobHooks, JobId, JobKillOutcome, JobOutcome, JobRead, JobRegistry,
    JobRegistryService, JobSnapshot, JobStart, JobStatus, JobTerminalStatus, JobsChangedListener,
};
use seekdeep_schemastery::Schema;
use seekdeep_scope::{
    scope_of,
    store::{AnonymousEntries, LayerEffectOptions, ScopeLayer, ScopedLayers},
};
use seekdeep_util::{
    abort::AbortSignal,
    timeout::{deadline, timeout_of},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

/// Timeout code that distinguishes a bounded wait from caller cancellation.
pub const TASK_WAIT_TIMEOUT: &str = "TASK_WAIT_TIMEOUT";

/// Default maximum number of active jobs in one exact-owner bucket.
pub const DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER: u64 = 10;

/// Largest positive safe integer, mirroring JavaScript `Number.MAX_SAFE_INTEGER`.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Cordis plugin name retained by loader-facing diagnostics.
pub const NAME: &str = "jobs-local";

/// The registry provides `ctx.jobs` without requiring any child services.
pub const INJECT: &[&str] = &[];

/// Configuration for the process-local job registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Maximum `running` plus `stopping` jobs per exact owner or in the shared
    /// unowned bucket; omission defaults to
    /// [`DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER`].
    pub max_concurrent_jobs_per_owner: Option<u64>,
}

/// The source-compatible admission schema for [`Config`].
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([(
        "maxConcurrentJobsPerOwner",
        Schema::number()
            .step(1.0)
            .min(1.0)
            .max(MAX_SAFE_INTEGER as f64)
            .with_default(DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER),
    )])
}

/// One scope's contributions: the job controllers attached from it and the
/// completion and change listeners registered there. All three tables are
/// anonymous because a contribution is identified by its own disposer, never
/// by a name a second registrant could shadow.
#[derive(Default)]
struct JobLayer {
    controllers: AnonymousEntries<()>,
    listeners: AnonymousEntries<JobDoneListener>,
    changed: AnonymousEntries<JobsChangedListener>,
}

impl ScopeLayer for JobLayer {
    fn is_empty(&self) -> bool {
        self.controllers.is_empty() && self.listeners.is_empty() && self.changed.is_empty()
    }
}

/// Pointer-identity map key for exact live agent instances.
#[derive(Clone)]
struct OwnerKey(Arc<Agent>);

impl PartialEq for OwnerKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for OwnerKey {}

impl std::hash::Hash for OwnerKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// Multi-waiter settlement signal that stays observable after the fact.
struct Settled {
    flag: AtomicBool,
    notify: Notify,
}

impl Settled {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            flag: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn mark(&self) {
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(self: &Arc<Self>) {
        if self.flag.load(Ordering::Acquire) {
            return;
        }
        loop {
            let notified = self.notify.notified();
            if self.flag.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// The registry's mutable per-job record (never handed out).
struct TrackedTask {
    id: JobId,
    kind: String,
    label: String,
    output_limit_bytes: Option<u64>,
    /// Exact lifecycle owner; session-id authorization is derived from it.
    owner: Option<Arc<Agent>>,
    hooks: Arc<Mutex<Box<dyn JobHooks>>>,
    status: JobStatus,
    detail: Option<String>,
    output: Option<String>,
    started_at: u64,
    finished_at: Option<u64>,
    reported: bool,
    /// Resolves once the terminal snapshot is recorded and waiters released.
    settled: Arc<Settled>,
    /// Live waits; settlement with a waiter marks the job reported.
    waiters: usize,
}

/// All registry state guarded by one mutex for exact settlement ordering.
#[derive(Default)]
struct Inner {
    store: IndexMap<JobId, TrackedTask>,
    counters: HashMap<String, u64>,
    #[allow(clippy::mutable_key_type)]
    owner_cleanups: HashMap<OwnerKey, EffectHandle>,
    listeners_closed: bool,
}

/// Shared state that outlives the public service wrapper and its fibers.
struct LocalJobState {
    context: Context,
    max_concurrent_jobs_per_owner: usize,
    inner: Mutex<Inner>,
    layers: ScopedLayers<JobLayer>,
}

impl LocalJobState {
    /// Whether an attached job controller can collect and stop work owned by
    /// `owner`. The global layer holds every controller attached from an
    /// unscoped context and therefore serves every owner; a scoped controller
    /// serves exactly the agents composed under it.
    fn serves_owner(&self, owner: Option<&Arc<Agent>>) -> bool {
        if !self.layers.global.controllers.is_empty() {
            return true;
        }
        let scope = owner.and_then(|agent| scope_of(agent.context()));
        self.layers
            .chain_layers(scope)
            .iter()
            .any(|layer| !layer.controllers.is_empty())
    }

    /// Counts authoritative active records for one exact owner or the shared
    /// unowned bucket.
    fn active_task_count(&self, owner: Option<&Arc<Agent>>) -> usize {
        self.inner
            .lock()
            .store
            .values()
            .filter(|job| {
                let same_owner = match (&job.owner, owner) {
                    (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                    (None, None) => true,
                    (Some(_), None) | (None, Some(_)) => false,
                };
                same_owner && matches!(job.status, JobStatus::Running | JobStatus::Stopping)
            })
            .count()
    }

    /// The completion listeners that own `owner`'s notices, in registration
    /// order per layer: the global layer's first, then each scoped layer along
    /// the owner's chain.
    fn listeners_for(&self, owner: Option<&Arc<Agent>>) -> Vec<JobDoneListener> {
        let mut listeners = self.layers.global.listeners.values().collect::<Vec<_>>();
        let scope = owner.and_then(|agent| scope_of(agent.context()));
        for layer in self.layers.chain_layers(scope) {
            listeners.extend(layer.listeners.values());
        }
        listeners
    }

    /// The change observers that own `owner`'s updates, resolved exactly like
    /// [`Self::listeners_for`].
    fn changed_for(&self, owner: Option<&Arc<Agent>>) -> Vec<JobsChangedListener> {
        let mut listeners = self.layers.global.changed.values().collect::<Vec<_>>();
        let scope = owner.and_then(|agent| scope_of(agent.context()));
        for layer in self.layers.chain_layers(scope) {
            listeners.extend(layer.changed.values());
        }
        listeners
    }

    /// Announces that one owner's visible set changed, containing each observer
    /// so one cannot break a lifecycle commit that already happened.
    fn notify_changed(&self, owner: Option<&Arc<Agent>>) {
        for listener in self.changed_for(owner) {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| listener(owner))) {
                tracing::warn!(
                    "jobs: onJobsChanged listener threw: {}",
                    panic_message(&payload)
                );
            }
        }
    }

    /// Records the first terminal outcome, releases waiters, then announces
    /// completion. First-wins preserves a teardown force-failure against late
    /// producer settlement, and completion is announced last.
    fn settle(&self, id: &JobId, outcome: JobOutcome) {
        let (owner, snapshot, listeners_closed) = {
            let mut inner = self.inner.lock();
            let Some(job) = inner.store.get_mut(id) else {
                return;
            };
            if job.status.is_terminal() {
                return;
            }
            job.status = match outcome.status {
                JobTerminalStatus::Completed => JobStatus::Completed,
                JobTerminalStatus::Killed => JobStatus::Killed,
                JobTerminalStatus::Failed => JobStatus::Failed,
            };
            job.detail = outcome.detail;
            job.output = outcome.output;
            job.finished_at = Some(now_millis());
            if job.waiters > 0 {
                job.reported = true;
            }
            let snapshot = snapshot_of(job);
            let owner = job.owner.clone();
            job.settled.mark();
            (owner, snapshot, inner.listeners_closed)
        };
        self.notify_changed(owner.as_ref());
        if listeners_closed {
            return;
        }
        for listener in self.listeners_for(owner.as_ref()) {
            if let Err(payload) =
                catch_unwind(AssertUnwindSafe(|| listener(&snapshot, owner.as_ref())))
            {
                tracing::warn!(
                    "jobs: onJobDone listener threw for {}: {}",
                    id,
                    panic_message(&payload)
                );
            }
        }
    }

    /// Attaches one awaited cleanup through the exact owner's scope.
    ///
    /// # Panics
    ///
    /// Panics when the agent registry is absent or the owner is not its
    /// currently registered instance, and when the owning scope rejects the
    /// cross-fiber effect.
    fn ensure_owner_cleanup(self: &Arc<Self>, owner: &Arc<Agent>) {
        let owner_id = owner.id().clone();
        let agents = self.context.get(AGENTS).unwrap_or_else(|| {
            panic!("background job ownership requires the agent registry (load @deepseek-ai/seekdeep-agent)")
        });
        if agents
            .get(&owner_id)
            .is_none_or(|live| !Arc::ptr_eq(&live, owner))
        {
            panic!(
                "agent \"{owner_id}\" is not the registered agent instance (background job owner must be live)"
            );
        }
        if self
            .inner
            .lock()
            .owner_cleanups
            .contains_key(&OwnerKey(owner.clone()))
        {
            return;
        }
        let weak_self = Arc::downgrade(self);
        let owner_clone = owner.clone();
        let detach = owner
            .context()
            .own(EffectHandle::new("jobs.ownerCleanup()", move || {
                let weak_self = weak_self.clone();
                let owner = owner_clone.clone();
                Box::pin(async move {
                    if let Some(state) = weak_self.upgrade() {
                        state
                            .inner
                            .lock()
                            .owner_cleanups
                            .remove(&OwnerKey(owner.clone()));
                        state.dispose_owned(&owner).await;
                    }
                    Ok(())
                })
            }));
        match detach {
            Ok(effect) => {
                self.inner
                    .lock()
                    .owner_cleanups
                    .insert(OwnerKey(owner.clone()), effect);
            }
            Err(error) => panic!("{error}"),
        }
    }

    /// Cancels, awaits terminal records, and drops every job owned by one exact
    /// agent lifecycle.
    async fn dispose_owned(&self, owner: &Arc<Agent>) {
        let owned_ids: Vec<JobId> = {
            let inner = self.inner.lock();
            inner
                .store
                .iter()
                .filter(|(_, job)| job.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, owner)))
                .map(|(id, _)| id.clone())
                .collect()
        };
        self.cancel_for_teardown(&owned_ids, "owner disposed");
        await_settled(&self.inner, &owned_ids).await;
        {
            let mut inner = self.inner.lock();
            for id in &owned_ids {
                inner.store.shift_remove(id);
            }
        }
        if !owned_ids.is_empty() {
            self.notify_changed(Some(owner));
        }
    }

    /// Closes listeners, cancels live jobs, awaits settlement, and detaches
    /// owner effects.
    async fn dispose_all(&self) {
        {
            let mut inner = self.inner.lock();
            inner.listeners_closed = true;
        }
        let all: Vec<(JobId, Option<Arc<Agent>>, Arc<Settled>)> = {
            let inner = self.inner.lock();
            inner
                .store
                .iter()
                .map(|(id, job)| (id.clone(), job.owner.clone(), job.settled.clone()))
                .collect()
        };
        let all_ids: Vec<JobId> = all.iter().map(|(id, _, _)| id.clone()).collect();
        self.cancel_for_teardown(&all_ids, "jobs service disposed");
        for (_, _, settled) in &all {
            settled.wait().await;
        }

        let mut emptied: Vec<Option<Arc<Agent>>> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        for (_, owner, _) in &all {
            let key = owner
                .as_ref()
                .map_or(usize::MAX, |agent| Arc::as_ptr(agent) as usize);
            if seen.insert(key) {
                emptied.push(owner.clone());
            }
        }
        {
            let mut inner = self.inner.lock();
            inner.store.clear();
        }
        for owner in &emptied {
            self.notify_changed(owner.as_ref());
        }

        let cleanups: Vec<EffectHandle> = {
            let mut inner = self.inner.lock();
            let cleanups = inner.owner_cleanups.values().cloned().collect();
            inner.owner_cleanups.clear();
            cleanups
        };
        for cleanup in cleanups {
            let _ = cleanup.dispose().await;
        }
    }

    /// Cancels jobs during teardown with per-job containment. A throwing cancel
    /// force-fails the record and reports a possible orphan.
    fn cancel_for_teardown(&self, ids: &[JobId], reason: &str) {
        for id in ids {
            let hooks = {
                let mut inner = self.inner.lock();
                let Some(job) = inner.store.get_mut(id) else {
                    continue;
                };
                if job.status.is_terminal() {
                    continue;
                }
                job.reported = true;
                job.hooks.clone()
            };
            match catch_unwind(AssertUnwindSafe(|| hooks.lock().cancel(Some(reason)))) {
                Ok(()) => {
                    let owner = {
                        let mut inner = self.inner.lock();
                        let Some(job) = inner.store.get_mut(id) else {
                            continue;
                        };
                        if job.status.is_terminal() {
                            continue;
                        }
                        job.status = JobStatus::Stopping;
                        job.owner.clone()
                    };
                    self.notify_changed(owner.as_ref());
                }
                Err(payload) => {
                    let message = panic_message(&payload);
                    let detail =
                        format!("cancel threw during teardown; work may be orphaned: {message}");
                    tracing::warn!(
                        "jobs: cancel of {id} threw during teardown; job record forced failed and work may be orphaned: {message}"
                    );
                    self.settle(
                        id,
                        JobOutcome {
                            status: JobTerminalStatus::Failed,
                            detail: Some(detail),
                            output: None,
                        },
                    );
                }
            }
        }
    }
}

/// The in-memory `jobs` registry.
pub struct LocalJobRegistry(Arc<LocalJobState>);

impl LocalJobRegistry {
    /// Builds, publishes, and lifecycle-owns a process-local registry.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures from publishing the
    /// seam slot or attaching the teardown effect.
    pub fn new(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        let state = Arc::new(LocalJobState {
            context: context.clone(),
            max_concurrent_jobs_per_owner: usize::try_from(
                config
                    .max_concurrent_jobs_per_owner
                    .unwrap_or(DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER),
            )
            .unwrap_or(usize::MAX),
            inner: Mutex::new(Inner::default()),
            layers: ScopedLayers::new(|_| JobLayer::default(), || {}),
        });
        let registry = Arc::new(Self(state.clone()));
        let service = JobRegistryService::new(registry.clone());
        service.provide(context)?;
        let teardown_state = state.clone();
        context.own(EffectHandle::new("jobs teardown", move || {
            let state = teardown_state.clone();
            Box::pin(async move {
                state.dispose_all().await;
                Ok(())
            })
        }))?;
        Ok(registry)
    }

    /// Builds the loader-compatible jobs-local plugin.
    #[must_use]
    pub fn plugin() -> Plugin {
        Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
            Box::pin(async move {
                let config: Config = serde_json::from_value(config)?;
                Self::new(&context, config)?;
                Ok(())
            })
        })
        .with_config_validator(|value: &Value| {
            config_schema()
                .resolve(value)
                .map_err(|error| anyhow::anyhow!("{error}"))
        })
    }
}

impl JobRegistry for LocalJobRegistry {
    fn start(&self, spec: JobStart) -> JobId {
        let owner = spec.owner.clone();
        assert!(
            self.0.serves_owner(owner.as_ref()),
            "background jobs unavailable: no job controller serves this agent (load @deepseek-ai/seekdeep-tool-jobs in its composition)"
        );
        assert!(
            !spec.kind.is_empty(),
            "invalid job kind: expected a non-empty string"
        );
        assert!(
            !spec.label.is_empty(),
            "invalid job label: expected a non-empty string"
        );
        if let Some(limit) = spec.output_limit_bytes
            && (limit == 0 || limit > MAX_SAFE_INTEGER)
        {
            panic!("invalid outputLimitBytes: expected a positive safe integer, got {limit}");
        }
        if let Some(ref owner) = owner {
            self.0.ensure_owner_cleanup(owner);
        }
        let active = self.0.active_task_count(owner.as_ref());
        assert!(
            active < self.0.max_concurrent_jobs_per_owner,
            "background job limit reached for this owner (limit: {}); use job_kill to stop an unneeded job, wait for it to finish, then retry",
            self.0.max_concurrent_jobs_per_owner
        );

        let hooks = (spec.run)();
        let done_future = hooks.done();
        let hooks = Arc::new(Mutex::new(hooks));
        let id = {
            let mut inner = self.0.inner.lock();
            let count = inner.counters.get(&spec.kind).copied().unwrap_or(0) + 1;
            inner.counters.insert(spec.kind.clone(), count);
            let id = JobId::new(format!("{}-{count}", spec.kind));
            inner.store.insert(
                id.clone(),
                TrackedTask {
                    id: id.clone(),
                    kind: spec.kind.clone(),
                    label: spec.label.clone(),
                    output_limit_bytes: spec.output_limit_bytes,
                    owner: owner.clone(),
                    hooks,
                    status: JobStatus::Running,
                    detail: None,
                    output: None,
                    started_at: now_millis(),
                    finished_at: None,
                    reported: false,
                    settled: Settled::new(),
                    waiters: 0,
                },
            );
            id
        };

        let state = self.0.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            match done_future.await {
                Ok(outcome) => state.settle(&task_id, outcome),
                Err(error) => {
                    tracing::warn!(
                        "jobs: job {task_id} producer done promise rejected (producer contract violation): {error}"
                    );
                    state.settle(
                        &task_id,
                        JobOutcome {
                            status: JobTerminalStatus::Failed,
                            detail: Some(error.to_string()),
                            output: None,
                        },
                    );
                }
            }
        });
        self.0.notify_changed(owner.as_ref());
        id
    }

    fn list(&self, caller: Option<&Arc<Agent>>) -> Vec<JobSnapshot> {
        let session = caller.map(|agent| agent.id());
        self.0
            .inner
            .lock()
            .store
            .values()
            .filter(|job| {
                job.owner
                    .as_ref()
                    .is_none_or(|owner| session.is_some_and(|id| owner.id() == id))
            })
            .map(snapshot_of)
            .collect()
    }

    fn get(&self, id: &JobId, caller: Option<&Arc<Agent>>) -> anyhow::Result<JobSnapshot> {
        let inner = self.0.inner.lock();
        let job = expect(&inner, id)?;
        assert_access(job, caller)?;
        Ok(snapshot_of(job))
    }

    fn read(&self, id: &JobId, caller: Option<&Arc<Agent>>) -> anyhow::Result<JobRead> {
        let mut inner = self.0.inner.lock();
        let job = expect_mut(&mut inner, id)?;
        assert_access(job, caller)?;
        let text = match job.hooks.lock().read_output() {
            Some(text) => text,
            None => {
                if job.status.is_terminal() {
                    job.output.clone().unwrap_or_default()
                } else {
                    String::new()
                }
            }
        };
        if job.status.is_terminal() {
            job.reported = true;
        }
        Ok(JobRead {
            text,
            snapshot: snapshot_of(job),
        })
    }

    fn kill(
        &self,
        id: &JobId,
        caller: Option<&Arc<Agent>>,
        reason: Option<&str>,
    ) -> anyhow::Result<JobKillOutcome> {
        let hooks = {
            let mut inner = self.0.inner.lock();
            let job = expect_mut(&mut inner, id)?;
            assert_access(job, caller)?;
            if job.status.is_terminal() {
                job.reported = true;
                return Ok(JobKillOutcome::AlreadyFinished);
            }
            job.hooks.clone()
        };
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| hooks.lock().cancel(reason))) {
            return Err(anyhow::anyhow!(panic_message(&payload)));
        }
        let mut inner = self.0.inner.lock();
        let job = expect_mut(&mut inner, id)?;
        if job.status.is_terminal() {
            job.reported = true;
            return Ok(JobKillOutcome::AlreadyFinished);
        }
        job.status = JobStatus::Stopping;
        job.reported = true;
        let owner = job.owner.clone();
        drop(inner);
        self.0.notify_changed(owner.as_ref());
        Ok(JobKillOutcome::Requested)
    }

    fn wait(
        &self,
        id: &JobId,
        timeout_ms: f64,
        caller: Option<&Arc<Agent>>,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'static, anyhow::Result<JobSnapshot>> {
        let state = self.0.clone();
        let id = id.clone();
        let caller = caller.cloned();
        Box::pin(async move { wait_impl(&state, &id, timeout_ms, caller.as_ref(), signal).await })
    }

    fn on_job_done(&self, listener: JobDoneListener) -> EffectHandle {
        self.0
            .layers
            .effect(
                &self.0.context,
                move |layer| Ok(layer.listeners.append(listener)),
                LayerEffectOptions::new("jobs.onJobDone()"),
            )
            .expect("jobs.onJobDone() requires an active context")
    }

    fn on_jobs_changed(&self, listener: JobsChangedListener) -> EffectHandle {
        self.0
            .layers
            .effect(
                &self.0.context,
                move |layer| Ok(layer.changed.append(listener)),
                LayerEffectOptions::new("jobs.onJobsChanged()"),
            )
            .expect("jobs.onJobsChanged() requires an active context")
    }

    fn attach_controller(&self, name: &str) -> EffectHandle {
        let _ = name;
        self.0
            .layers
            .effect(
                &self.0.context,
                move |layer| Ok(layer.controllers.append(())),
                LayerEffectOptions::new("jobs.attachController()"),
            )
            .expect("jobs.attachController() requires an active context")
    }
}

/// Looks up a job or fails loud.
fn expect<'a>(inner: &'a Inner, id: &JobId) -> anyhow::Result<&'a TrackedTask> {
    inner
        .store
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("unknown job {id}"))
}

/// Looks up a mutable job or fails loud.
fn expect_mut<'a>(inner: &'a mut Inner, id: &JobId) -> anyhow::Result<&'a mut TrackedTask> {
    inner
        .store
        .get_mut(id)
        .ok_or_else(|| anyhow::anyhow!("unknown job {id}"))
}

/// The isolation fence: a job with an owner is reachable only by callers whose
/// session id matches.
fn assert_access(job: &TrackedTask, caller: Option<&Arc<Agent>>) -> anyhow::Result<()> {
    if let Some(owner) = &job.owner
        && !caller.is_some_and(|caller| owner.id() == caller.id())
    {
        anyhow::bail!("job {} belongs to another session", job.id);
    }
    Ok(())
}

/// Projects a fresh read-only snapshot from the mutable record.
fn snapshot_of(job: &TrackedTask) -> JobSnapshot {
    JobSnapshot {
        id: job.id.clone(),
        kind: job.kind.clone(),
        label: job.label.clone(),
        output_limit_bytes: job.output_limit_bytes,
        owner_session: job.owner.as_ref().map(|owner| owner.id().clone()),
        status: job.status,
        detail: job.detail.clone(),
        started_at: job.started_at,
        finished_at: job.finished_at,
        reported: job.reported,
    }
}

/// Current wall-clock epoch milliseconds.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Renders a caught panic payload for a contained listener diagnostic.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-string panic".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

/// Resolves every registered settlement signal in `ids`, skipping records that
/// are already terminal.
async fn await_settled(inner: &Mutex<Inner>, ids: &[JobId]) {
    let waits: Vec<Arc<Settled>> = {
        let inner = inner.lock();
        ids.iter()
            .filter_map(|id| inner.store.get(id).map(|job| job.settled.clone()))
            .collect()
    };
    for settled in waits {
        settled.wait().await;
    }
}

/// Resolution of a bounded wait against settlement and cancellation.
enum WaitKind {
    /// The job settled first.
    Settled,
    /// The scoped deadline expired first.
    TimedOut,
    /// Caller cancellation arrived first.
    Aborted,
}

/// Waits for one job with bounded timeout and caller cancellation.
async fn wait_impl(
    state: &LocalJobState,
    id: &JobId,
    timeout_ms: f64,
    caller: Option<&Arc<Agent>>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<JobSnapshot> {
    let settled: Arc<Settled> = {
        let mut inner = state.inner.lock();
        let job = expect_mut(&mut inner, id)?;
        assert_access(job, caller)?;
        if !timeout_ms.is_finite() || timeout_ms <= 0.0 {
            anyhow::bail!(
                "invalid wait timeout: expected a positive number of milliseconds, got {timeout_ms}"
            );
        }
        if job.status.is_terminal() {
            job.reported = true;
            return Ok(snapshot_of(job));
        }
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            anyhow::bail!("wait aborted");
        }
        job.waiters += 1;
        job.settled.clone()
    };

    let mut deadline = deadline(signal.as_ref(), timeout_ms, TASK_WAIT_TIMEOUT)?;
    let aborted = deadline.signal.clone();

    let outcome = tokio::select! {
        () = settled.wait() => WaitKind::Settled,
        () = aborted.cancelled() => {
            if timeout_of(&aborted, Some(TASK_WAIT_TIMEOUT)).is_some() {
                WaitKind::TimedOut
            } else {
                WaitKind::Aborted
            }
        }
    };
    deadline.dispose();

    let mut inner = state.inner.lock();
    let job = expect_mut(&mut inner, id)?;
    job.waiters = job.waiters.saturating_sub(1);
    if matches!(outcome, WaitKind::Aborted) {
        anyhow::bail!("wait aborted");
    }
    if job.status.is_terminal() {
        job.reported = true;
    }
    Ok(snapshot_of(job))
}
