//! Native workspace analysis, with compiler-independent package and merge rules.

mod merge;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native;
mod package;

pub use merge::merge_workspace_models;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    AnalysisMode, Compiler, DiscoveredTypertPackage, WorkspaceAnalyzer, WorkspaceAnalyzerOptions,
    WorkspaceCaches, locate_library, repository, repository_graphs, run_with_stack,
};
pub use package::{
    ModuleIdentity, client_export_subpaths, external_module_identity_for_file,
    host_export_subpaths, is_dual_face_package, is_remote_segment, is_standard_library_file,
    module_identity, package_export_targets, source_path_for_export,
};
