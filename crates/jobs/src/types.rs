//! Types shared by job producers, the registry, and controllers.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_agent::Agent;
use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};

use crate::brand::JobId;

/// Task lifecycle: running, optionally stopping, then exactly one terminal
/// status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// The job is running.
    Running,
    /// Cancellation was requested.
    Stopping,
    /// The job finished normally.
    Completed,
    /// The job was cancelled.
    Killed,
    /// The job broke.
    Failed,
}

impl JobStatus {
    /// Whether this is a terminal status.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Killed | Self::Failed)
    }
}

/// The terminal statuses a producer reports through its hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobTerminalStatus {
    /// Finished.
    Completed,
    /// Cancelled.
    Killed,
    /// Broke.
    Failed,
}

/// Terminal result supplied by a producer through the done hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutcome {
    /// How the job ended.
    pub status: JobTerminalStatus,
    /// Kind-specific detail rendered into status lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Final output for jobs without readOutput.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Producer declaration passed to the registry start method.
pub struct JobStart {
    /// Producer kind - also the id prefix.
    pub kind: String,
    /// One-line model-facing label.
    pub label: String,
    /// Optional UTF-8 byte cap for model-facing notices and output reads.
    pub output_limit_bytes: Option<u64>,
    /// Owning live agent; absent for an unowned job.
    pub owner: Option<Arc<Agent>>,
    /// Start the work after preflight and synchronously return its hooks.
    pub run: Box<dyn FnOnce() -> Box<dyn JobHooks> + Send>,
}

/// Hooks through which the runtime controls and observes producer work.
pub trait JobHooks: Send {
    /// Request termination. Must be synchronous, idempotent, and eventually
    /// settle done; throws propagate.
    fn cancel(&self, reason: Option<&str>);
    /// Resolves after the producer releases its resources. A rejection is
    /// converted to a failed record by the runtime.
    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>>;
    /// Consume output produced since the previous call; absent for
    /// final-output-only jobs.
    fn read_output(&self) -> Option<String> {
        None
    }
}

/// A read-only projection of one job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    /// The registry-issued id.
    pub id: JobId,
    /// The producer kind.
    pub kind: String,
    /// The producer-supplied one-line label.
    pub label: String,
    /// Producer-owned cap for model-facing notices and output reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_limit_bytes: Option<u64>,
    /// Owner session id used for authorization; absent for unowned jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session: Option<SessionId>,
    /// Current lifecycle state.
    pub status: JobStatus,
    /// Kind-specific status detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Epoch ms when the job was registered.
    pub started_at: u64,
    /// Epoch ms when the job settled; absent while running/stopping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// True when a kill/read/wait/teardown has committed to report the terminal state.
    pub reported: bool,
}

/// Output and post-read state returned by the read method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRead {
    /// The consuming delta, or the idempotent final output after settlement.
    pub text: String,
    /// The job's state at read time.
    pub snapshot: JobSnapshot,
}

/// Completion callback with the exact owner supplied at start, or absent for
/// an unowned job.
pub type JobDoneListener = Arc<dyn Fn(&JobSnapshot, Option<&Arc<Agent>>) + Send + Sync + 'static>;

/// Observation callback for a change to one owner's visible set.
pub type JobsChangedListener = Arc<dyn Fn(Option<&Arc<Agent>>) + Send + Sync + 'static>;

/// The outcome of a kill request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobKillOutcome {
    /// Cancellation was requested for live work.
    Requested,
    /// The job had already finished.
    AlreadyFinished,
}
