//! External-config JSON-RPC agent launcher.

/// Config resolution, boot, and process lifecycle.
pub mod runner;

mod runtime_catalog;

/// Product-facing executable name.
pub const NAME: &str = "seekdeep-jsonrpc-agent";
