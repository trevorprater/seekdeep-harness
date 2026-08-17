//! `SQLite` durable session-persistence backend.

/// `SQLite` service and lifecycle orchestration.
pub mod backend;
/// Package-owned invariant companion.
pub mod invariant;
/// Database schema and row reconstruction helpers.
pub mod schema;

pub use backend::{INJECT, NAME, SqliteConfig, SqliteSessionPersistence, install, plugin};
pub use schema::{JournalMode, SCHEMA_VERSION};
