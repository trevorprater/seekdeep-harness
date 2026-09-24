//! Host-only semantic TypeScript projects for repository commands.

pub use seekdeep_typert_generator::analyzer::repository::{
    CompilerDiagnostics, DeclarationProjection, RepositoryCompiler, RepositoryConfig,
    RepositoryDeclaration, RepositorySourceFile, TypeScriptProject, VirtualSource,
    locate_repository_library, semantic_compiler_options,
};
