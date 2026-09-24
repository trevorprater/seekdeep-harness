//! Process-local implementation of the background-job registry seam.

pub mod index;
pub mod invariant;

pub use index::{
    Config, DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER, INJECT, LocalJobRegistry, MAX_SAFE_INTEGER,
    NAME, TASK_WAIT_TIMEOUT, config_schema,
};
pub use invariant::register_invariant;
