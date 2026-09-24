//! One test binary per crate: every top-level test file is a module here.

mod code_mode_parity;
mod coding_harness_parity;
mod continuable_settlement_parity;
mod headless_round_trip;
mod keyless_loader_smoke;
mod loader_settlement;
mod provider_snapshot_parity;
mod replay_snapshot_parity;

#[cfg(not(windows))]
#[path = "support/retry_snapshot_backend.rs"]
mod retry_backend;
#[cfg(not(windows))]
#[path = "support/settlement_fence.rs"]
mod settlement_fence;
