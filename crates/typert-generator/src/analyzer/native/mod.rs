//! Native workspace analysis over the hosted compiler library.

pub(crate) mod engine;
mod face;
pub(crate) mod js;
pub(crate) mod jstext;
pub(crate) mod paths;
mod remote;
pub mod repository;
pub mod repository_graphs;
pub(crate) mod syntax;
pub(crate) mod system;
mod workspace;

pub use engine::{Compiler, locate_library, run_with_stack};
pub use workspace::{
    AnalysisMode, DiscoveredTypertPackage, WorkspaceAnalyzer, WorkspaceAnalyzerOptions,
    WorkspaceCaches,
};
