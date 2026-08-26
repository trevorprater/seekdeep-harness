//! Dependency-light policies shared by Rust-owned repository commands.

/// Agent Note tree discovery plus classification and format verification.
pub mod agent_note_tree;
/// Frozen Agent Note archive triplets, seals, and append-only verification.
pub mod archived_agent_notes;
/// Shipped configuration credential/endpoint source-ownership policy.
pub mod config_source_ownership;
/// Cordis Loader configuration discovery.
pub mod cordis_config_files;
/// Workspace source-alias to built-declaration path mapping.
pub mod doc_typecheck_paths;
/// First-party SeekDeep package license policy.
pub mod package_licenses;
/// Byte-identical bilingual Markdown derivative partitioning.
pub mod paired_markdown_derivatives;
/// Unavailable public-repository reference detection across tracked files.
pub mod public_repository_links;
/// Static and packed publication-payload policy.
pub mod publication_payload;
/// Shared repository glob discovery and line-oriented reference scanning.
pub mod repo_files;
