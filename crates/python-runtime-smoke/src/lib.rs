//! Keyless native orchestration for the installed Python SDK and packaged runtime.

mod constants;
mod json;
/// Deterministic OpenAI streaming responses selected from request history.
pub mod model;
mod peer;
mod runner;
mod server;
pub use runner::{Options, Scenario, run};
/// Lossless advanced-scenario normalization and source-constrained comparison.
pub mod snapshot;

/// Reports an interrupt whose managed child cleanup has completed successfully.
pub fn is_interrupted(error: &anyhow::Error) -> bool {
    error.is::<peer::Interrupted>()
}
