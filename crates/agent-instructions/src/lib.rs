//! Workspace context loader for AGENTS.md/CLAUDE.md instruction files.

pub mod config;
pub mod digest;
pub mod files;
pub mod index;
pub mod invariant;
pub mod render;
pub mod state;

pub use config::{
    Config, DEFAULT_INSTRUCTION_FILE_CANDIDATES, DEFAULT_LOCAL_INSTRUCTION_FILE_CANDIDATES,
    DEFAULT_MAX_SOURCE_BYTES, DEFAULT_PROJECT_ROOT_MARKERS, ResolvedConfig,
    ResolvedDiscoveryConfig, config_schema, resolve_config, resolve_discovery_config,
    workspace_baseline_identity,
};
pub use digest::{instruction_content_sha1, trimmed_instruction_digest};
pub use files::{
    DiscoverOptions, ProbedInstructionFile, RenderedInstructionSet, ScopeInstructionProbe,
    ancestor_chain, dedup_instruction_files_by_directory, descendant_dirs_between,
    discover_baseline_instruction_files, find_project_root, load_baseline_instruction_set,
    load_baseline_instructions, probe_scope_instruction, read_scope_instruction, relative_display,
};
pub use index::{apply, plugin};
pub use render::{
    AgentInstructionAction, AgentInstructionChange, ChangeRenderItem, InstructionFile,
    LoadedInstructionFile, RenderedWorkspaceContext, TruncatedInstruction, USER_GLOBAL_DIRECTORY,
    USER_GLOBAL_FILE, candidate_scope_key, decode_scope_key, instruction_scope_key,
    render_instruction_changes, render_workspace_context, render_workspace_instruction_set,
    scope_for_display_path,
};
pub use state::{
    AGENT_INSTRUCTIONS_KIND, AgentInstructionSource, BaselineInstructionState,
    InstructionVersionCache, InstructionVersionState, InstructionVersionUpdate, ReconcileOptions,
    ReconciledInstructionContext, apply_instruction_version_updates, baseline_instruction_state,
    reconcile_instruction_context, retained_instruction_version_updates, workspace_context_message,
};
