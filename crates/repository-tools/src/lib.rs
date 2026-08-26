//! Dependency-light policies shared by Rust-owned repository commands.

/// Agent Note tree discovery plus classification and format verification.
pub mod agent_note_tree;
/// Frozen Agent Note archive triplets, seals, and append-only verification.
pub mod archived_agent_notes;
pub mod clean;
/// Shipped configuration credential/endpoint source-ownership policy.
pub mod config_source_ownership;
/// Cordis Loader configuration discovery.
pub mod cordis_config_files;
pub mod coverage_exempt;
/// Workspace source-alias to built-declaration path mapping.
pub mod doc_typecheck_paths;
/// Standing-document word-budget policy.
pub mod document_budgets;
pub mod markdown_util;
pub mod md_wrap;
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
/// Cross-product Skill invocation metadata policy.
pub mod skill_invocation_metadata;
/// Vendored package lockfile link-integrity policy.
pub mod vendored_links;
