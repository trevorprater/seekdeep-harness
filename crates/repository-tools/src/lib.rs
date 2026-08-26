//! Dependency-light policies shared by Rust-owned repository commands.

/// Agent Note tree discovery plus classification and format verification.
pub mod agent_note_tree;
/// Frozen Agent Note archive triplets, seals, and append-only verification.
pub mod archived_agent_notes;
pub mod built_package_invariants;
pub mod clean;
pub mod client_domain_graph;
/// Shipped configuration credential/endpoint source-ownership policy.
pub mod config_source_ownership;
/// Cordis Loader configuration discovery.
pub mod cordis_config_files;
pub mod cordis_config_metadata;
pub mod coverage_exempt;
pub mod doc_site_fragments;
/// Workspace source-alias to built-declaration path mapping.
pub mod doc_typecheck_paths;
/// Standing-document word-budget policy.
pub mod document_budgets;
pub mod fixture_cleanup;
pub mod markdown_util;
pub mod md_links;
pub mod md_wrap;
pub mod mermaid;
pub mod module_graph;
pub mod node_next_types;
pub mod package_graph;
pub mod package_invariants;
/// First-party SeekDeep package license policy.
pub mod package_licenses;
pub mod package_paths;
pub mod package_readme_limitations;
/// Byte-identical bilingual Markdown derivative partitioning.
pub mod paired_markdown_derivatives;
pub mod project_reference_faces;
/// Unavailable public-repository reference detection across tracked files.
pub mod public_repository_links;
/// Static and packed publication-payload policy.
pub mod publication_payload;
pub mod release_bump;
pub mod release_bump_command;
pub mod release_families;
pub mod release_pack;
pub mod release_process;
pub mod release_publish;
pub mod release_tarball;
pub mod release_verify;
pub mod release_verify_packed_install;
/// Shared repository glob discovery and line-oriented reference scanning.
pub mod repo_files;
pub mod run_oxlint;
pub mod runtime_closure;
/// Cross-product Skill invocation metadata policy.
pub mod skill_invocation_metadata;
pub mod translation_pairing;
pub mod translation_pairing_command;
pub mod translation_pairing_git;
pub mod translation_pairing_record;
pub mod translation_prompt;
pub mod translation_prompt_verifier;
/// Vendored package lockfile link-integrity policy.
pub mod vendored_links;
