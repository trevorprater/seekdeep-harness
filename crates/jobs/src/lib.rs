//! The background-job Service Definition (ctx.jobs): the contract for job
//! ids, session-scoped access, lifecycle state, completion listeners, and
//! owner cleanup.

pub mod brand;
pub mod index;
pub mod invariant;
pub mod types;

pub use brand::JobId;
pub use index::{JOBS, JobRegistry, JobRegistryService};
pub use invariant::register_invariant;
pub use types::{
    JobDoneListener, JobHooks, JobKillOutcome, JobOutcome, JobRead, JobSnapshot, JobStart,
    JobStatus, JobTerminalStatus, JobsChangedListener,
};
