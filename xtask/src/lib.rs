//! Reusable implementation modules for repository gates.

/// Rust-owned plugin configuration catalog generation and validation.
pub mod config_catalog;
/// macOS runtime-wheel deployment-target validation.
pub mod macos_deployment;
/// Session persistence catalog generation and validation.
pub mod persistence_catalog;
/// Canonical packed-row layout for repository Session JSONL fixtures.
pub mod session_fixture_layout;
/// Runtime-harvested model-facing tool-schema catalog.
pub mod tool_catalog;
