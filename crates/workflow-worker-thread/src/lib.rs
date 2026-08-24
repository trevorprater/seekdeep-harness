//! Worker-thread workflow engine: the seam implementation that runs each
//! model-written script in an escapable context and bridges `agent()` calls to
//! host subagents.

pub mod host;
pub mod index;
pub mod invariant;
pub(crate) mod job_executor;
pub mod meta;
mod process;
pub mod protocol;
pub mod realm;
pub mod runtime;
pub mod types;
pub mod worker;

pub use host::WorkerRun;
pub use index::{Config, INJECT, NAME, WorkerThreadWorkflowEngine, plugin};
pub use invariant::register_invariant;
pub use meta::{validate_meta, validate_meta_value};
pub use protocol::{HostToWorkerMessage, HostToWorkerType, WorkerToHostMessage, WorkerToHostType};
pub use realm::{MaterializeError, materialize_from_realm, render_thrown};
pub use runtime::{ExecutionObserver, WorkflowExecution};
pub use types::{ChildHandle, ChildPort, ChildResult, ChildStartRequest, WorkerInit, WorkerLimits};
