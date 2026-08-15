//! Durable sessions, agents, prompts, tools, and the default agent loop.

/// Compact durable encoding for assistant delta runs.
pub mod chunk_rows;
/// Relational execution-log invariants.
pub mod invariant;
/// Durable event vocabulary understood by this build.
pub mod known_event_types;
/// Ownership of unpublished provider-prepared sessions.
pub mod preparation;
/// Deterministic repair for crash-interrupted turns.
pub mod repair;
/// Request-header canonicalization and reconstruction.
pub mod request_header;
/// Append-only durable session state.
pub mod session;
/// Live session publication and fork ownership.
pub mod session_store;
