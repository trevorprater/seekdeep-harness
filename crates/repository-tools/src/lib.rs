//! Dependency-light policies shared by Rust-owned repository commands.

/// Agent Note tree discovery plus classification and format verification.
pub mod agent_note_tree;
/// Cordis Loader configuration discovery.
pub mod cordis_config_files;
/// Workspace source-alias to built-declaration path mapping.
pub mod doc_typecheck_paths;
/// Byte-identical bilingual Markdown derivative partitioning.
pub mod paired_markdown_derivatives;
/// Static and packed publication-payload policy.
pub mod publication_payload;
