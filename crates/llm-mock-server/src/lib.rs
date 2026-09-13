//! Scriptable OpenAI-compatible HTTP/SSE fault server and standalone CLI.

/// Standalone command-line parsing.
pub mod cli;
/// Package-owned invariant registration.
pub mod invariant;
mod server;

pub use server::{
    ConcreteMockLlmBehavior, DEFAULT_MOCK_LLM_RANDOM_WEIGHTS, MAX_MOCK_LLM_TIMER_DELAY_MS,
    MOCK_LLM_BEHAVIORS, MockLlmBehavior, MockLlmEventObserver, MockLlmRandomWeights,
    MockLlmRequestOutcome, MockLlmRequestRecord, MockLlmServer, MockLlmServerEvent,
    MockLlmServerOptions, start_mock_llm_server,
};
