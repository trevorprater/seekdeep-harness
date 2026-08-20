//! Worker-thread workflow engine: the seam implementation that runs each
//! model-written script in an escapable context and bridges `agent()` calls to
//! host subagents.

pub mod invariant;
pub mod meta;
pub mod protocol;
pub mod types;

pub use invariant::register_invariant;
pub use meta::validate_meta;
pub use protocol::{HostToWorkerMessage, HostToWorkerType, WorkerToHostMessage, WorkerToHostType};
pub use types::{ChildHandle, ChildPort, ChildResult, ChildStartRequest, WorkerInit, WorkerLimits};
