//! One test binary per crate: every top-level test file is a module here.

mod analyzer_contracts;
mod catalog_parity;
mod emitter_parity;
mod renderer_parity;
mod repository_graphs_parity;
mod schema_matrix;
mod workspace_artifacts;

#[path = "support/catalog_cases.rs"]
mod cases;
