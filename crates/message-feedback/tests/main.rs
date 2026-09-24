//! One test binary per crate: every top-level test file is a module here.

mod invariant_parity;
mod loader_composition_parity;
mod message_feedback_parity;

#[path = "support/mod.rs"]
mod support;
