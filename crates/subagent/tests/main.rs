//! One test binary per crate: every top-level test file is a module here.

mod continuation_inheritance_parity;
mod continuation_parity;
mod continuation_routing_parity;
mod continuation_settlement_parity;
mod invariant_parity;
mod lifecycle_parity;
mod list_children_parity;
mod registry_order_parity;
mod settlement_fence_fixture_parity;

#[path = "support/mod.rs"]
mod support;
