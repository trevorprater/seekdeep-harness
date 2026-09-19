//! Worker outcomes shared by the native host and compatibility engines.

use seekdeep_code_runtime::{CodeJsonString, CodeJsonValue, CodeRunFailureKind};
use seekdeep_llm::AbortSignal;
use serde_json::Value;

#[derive(Debug)]
pub(crate) enum EngineCompletion {
    Success(Option<CodeJsonValue>),
    Exception(CodeJsonString),
    InvalidOutput,
    OutputLimit,
    WorkerExit(i32),
    #[cfg(test)]
    HeapLimit,
    ComputeTimeout,
    WallTimeout,
    Abort(Value),
    ForgedFailure(CodeRunFailureKind, CodeJsonString),
}

#[derive(Debug)]
pub(crate) struct EngineOutcome {
    pub(crate) logs: Vec<CodeJsonString>,
    pub(crate) completion: EngineCompletion,
}

pub(crate) struct EngineLimits {
    pub(crate) max_output_bytes: usize,
    pub(crate) max_old_generation_size_mb: f64,
    pub(crate) compute_ms: f64,
    pub(crate) max_wall_ms: f64,
    pub(crate) signal: AbortSignal,
}
