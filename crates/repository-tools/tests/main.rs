//! One test binary per crate: every top-level test file is a module here.

mod agent_note_tree_parity;
mod archived_agent_notes_parity;
mod browser_gif_parity;
mod built_package_invariants_parity;
mod ci_source_oracle;
mod ci_workflow_contract;
mod clean_parity;
mod client_bundle_parity;
mod client_bundle_wasm_maps;
mod client_catalog_parity;
mod client_css_declaration_boundary;
mod client_domain_graph_parity;
mod config_source_ownership_parity;
mod cordis_catalog_partition_parity;
mod cordis_config_files_parity;
mod cordis_config_metadata_parity;
mod cordis_config_verifier_parity;
mod cordis_core_api_parity;
mod cordis_walk_parity;
mod coverage_exempt_parity;
mod coverage_launcher_parity;
mod coverage_uncovered_locations_parity;
mod doc_graphs_links;
mod doc_graphs_source_parity;
mod doc_site_configuration_parity;
mod doc_site_fragments_parity;
mod doc_site_projection_parity;
mod doc_source_links;
mod doc_typecheck_paths_parity;
mod document_budgets_parity;
mod fixture_cleanup_parity;
mod jsdoc_parity;
mod lefthook_installer_parity;
mod lint_rule_fingerprint_parity;
mod markdown_util_parity;
mod md_links_parity;
mod md_wrap_parity;
mod mermaid_parity;
mod module_graph_parity;
mod native_test_gates_parity;
mod node_next_types_parity;
mod notices_vendor_parity;
mod npm_baseline_parity;
mod package_graph_parity;
mod package_invariants_parity;
mod package_licenses_parity;
mod package_paths_parity;
mod package_readme_limitations_parity;
mod package_readme_model_experience_parity;
mod paired_markdown_derivatives_parity;
mod project_reference_faces_parity;
mod public_repository_links_parity;
mod publication_payload_parity;
mod publint_all_parity;
mod release_bump_command_parity;
mod release_bump_parity;
mod release_families_parity;
mod release_pack_parity;
mod release_process_parity;
mod release_publish_parity;
mod release_tarball_parity;
mod release_verify_packed_install_parity;
mod release_verify_parity;
mod repo_files_parity;
mod rescope_exact_edit_parity;
mod run_gates_parity;
mod run_oxlint_parity;
mod runtime_closure_parity;
mod scoped_events_generator_parity;
mod skill_invocation_metadata_parity;
mod slot_walk_parity;
mod translation_brief_command_parity;
mod translation_brief_parity;
mod translation_pairing_command_parity;
mod translation_pairing_core_parity;
mod translation_pairing_git_parity;
mod translation_pairing_merge_parity;
mod translation_pairing_record_parity;
mod translation_prompt_parity;
mod translation_prompt_verifier_parity;
mod type_equiv_oracle;
mod typescript_repository_commands_parity;
mod vendored_links_parity;
mod workspace_constraints_parity;

#[path = "npm_baseline/bundle.rs"]
mod bundle;
#[path = "npm_baseline/capture.rs"]
mod capture;
#[path = "npm_baseline/pack.rs"]
mod pack;
#[path = "npm_baseline/registry.rs"]
mod registry;
#[cfg(unix)]
#[path = "npm_baseline/release.rs"]
mod release;
#[path = "npm_baseline/smoke.rs"]
mod smoke;
#[path = "npm_baseline/support.rs"]
mod support;
