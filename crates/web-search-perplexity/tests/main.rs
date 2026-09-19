//! One test binary per crate: every top-level test file is a module here.

mod perplexity_e2e;
mod perplexity_spec;

#[path = "support/mod.rs"]
mod support;
