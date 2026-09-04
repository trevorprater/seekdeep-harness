//! Compiler-independent Typert analysis models and artifact generation.

pub mod emitter;
pub mod model;
mod remote;
pub mod renderer;
mod schema;
mod source_map;
mod text;

/// Source-classified failure from model traversal or artifact generation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TypertGeneratorError {
    /// Unsupported compiler-independent model record.
    #[error("{0}")]
    Model(String),
    /// Broken graph edge or unrenderable declaration.
    #[error("{0}")]
    Render(String),
    /// Unsupported artifact projection.
    #[error("{0}")]
    Emit(String),
    /// Invalid source workspace or authored type contract.
    #[error("{0}")]
    Analysis(String),
}

impl TypertGeneratorError {
    /// Source-compatible error class name.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Model(_) => "Error",
            Self::Render(_) => "TypeGraphRenderError",
            Self::Emit(_) => "TypertEmitError",
            Self::Analysis(_) => "TypertAnalysisError",
        }
    }
}

/// Result preserving the source error class and diagnostic.
pub type Result<T> = std::result::Result<T, TypertGeneratorError>;
