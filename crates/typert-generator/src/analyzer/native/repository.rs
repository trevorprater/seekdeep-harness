//! Shared compiler projects and syntax queries for native repository commands.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;

use super::{
    engine::{Compiler, locate_library},
    js::{self, Scope, Val},
    paths,
};

mod config;
mod declarations;
mod diagnostics;

pub use declarations::{DeclarationProjection, RepositoryDeclaration};
pub use diagnostics::{CompilerDiagnostics, VirtualSource};

/// Parsed compiler options, source roots, and resolved project references.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryConfig {
    /// TypeScript's ordered list of matched source files.
    pub file_names: Vec<String>,
    /// Effective options, with compiler enumeration values preserved.
    pub options: Value,
    /// Absolute config filenames in authored reference order.
    pub project_references: Vec<String>,
}

/// A source file loaded and bound by a repository's semantic program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySourceFile {
    /// Compiler-owned absolute filename.
    pub file_name: String,
    /// Filename relative to the repository root, with forward slashes.
    pub relative_path: String,
    /// Source text decoded by the compiler system.
    pub text: String,
    /// Whether the compiler classified this file as a declaration file.
    pub is_declaration_file: bool,
}

/// An owned compiler library for configuration, syntax, and snippet checking.
#[derive(Debug)]
pub struct RepositoryCompiler {
    compiler: Compiler,
}

impl RepositoryCompiler {
    /// Loads the workspace's TypeScript dependency.
    ///
    /// # Errors
    /// Returns compiler lookup or initialization errors.
    pub fn new(root: &Path) -> Result<Self> {
        Self::load(&locate_repository_library(root)?)
    }

    /// Loads an explicitly selected compiler library.
    ///
    /// # Errors
    /// Returns compiler initialization errors.
    pub fn load(library: &Path) -> Result<Self> {
        Ok(Self {
            compiler: Compiler::load(
                &library
                    .canonicalize()
                    .unwrap_or_else(|_| library.to_owned()),
            )?,
        })
    }

    /// Canonical path of the selected compiler dependency.
    pub fn library(&self) -> &Path {
        self.compiler.library()
    }

    /// Reads JSONC through the compiler's own configuration reader.
    ///
    /// # Errors
    /// Returns the compiler's flattened read or parse diagnostic.
    pub fn read_config(&mut self, path: &Path) -> Result<Value> {
        self.compiler
            .run(|scope, ts| config::read_config(scope, ts, path))
    }

    /// Resolves `extends`, include/exclude globs, options, and references.
    ///
    /// # Errors
    /// Returns every configuration diagnostic in compiler order.
    pub fn parse_config(
        &mut self,
        path: &Path,
        current_directory: &Path,
    ) -> Result<RepositoryConfig> {
        self.compiler
            .run(|scope, ts| config::parse_config(scope, ts, path, current_directory))
    }
}

/// One Host-only semantic graph and its shared compiler type checker.
///
/// Referenced projects are flattened in authored order, retaining the Host
/// aggregate's options. The root solution and Client aggregate are not merged
/// into this graph.
pub struct TypeScriptProject {
    root: PathBuf,
    root_names: Vec<String>,
    options: Value,
    program: v8::Global<v8::Value>,
    checker: v8::Global<v8::Value>,
    compiler: Compiler,
}

impl std::fmt::Debug for TypeScriptProject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypeScriptProject")
            .field("root", &self.root)
            .field("root_names", &self.root_names)
            .finish_non_exhaustive()
    }
}

impl TypeScriptProject {
    /// Loads the Host graph using the workspace's compiler dependency.
    ///
    /// # Errors
    /// Returns compiler loading, configuration, or program construction errors.
    pub fn new(root: &Path) -> Result<Self> {
        Self::with_compiler(root, &locate_repository_library(root)?)
    }

    /// Loads the Host graph using an explicitly selected compiler library.
    ///
    /// # Errors
    /// Returns compiler loading, configuration, or program construction errors.
    pub fn with_compiler(root: &Path, library: &Path) -> Result<Self> {
        let root = paths::resolve(root);
        let mut compiler = Compiler::load(
            &library
                .canonicalize()
                .unwrap_or_else(|_| library.to_owned()),
        )?;
        let (root_names, options, program, checker) = compiler.run(|scope, ts| {
            let (root_names, options) = config::load_project_graph(scope, ts, &root)?;
            let mut options = options;
            semantic_compiler_options(&mut options);
            let names = js::from_json(scope, &serde_json::json!(root_names))?;
            let compiler_options = js::from_json(scope, &options)?;
            let program = js::call(scope, ts, "createProgram", &[names, compiler_options])?;
            let checker = js::call(scope, program, "getTypeChecker", &[])?;
            Ok((
                root_names,
                options,
                v8::Global::new(scope, program),
                v8::Global::new(scope, checker),
            ))
        })?;
        Ok(Self {
            root,
            root_names,
            options,
            program,
            checker,
            compiler,
        })
    }

    /// Repository root against which file names are rendered.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Ordered and deduplicated source roots from all referenced projects.
    pub fn root_names(&self) -> &[String] {
        &self.root_names
    }

    /// Host options with emit-only settings disabled for semantic queries.
    pub fn options(&self) -> &Value {
        &self.options
    }

    /// Runs a native semantic query against the owned program and checker.
    pub(crate) fn with_program<R>(
        &mut self,
        routine: impl for<'s, 'i> FnOnce(&mut Scope<'s, 'i>, Val<'s>, Val<'s>, Val<'s>) -> Result<R>,
    ) -> Result<R> {
        let program = &self.program;
        let checker = &self.checker;
        self.compiler.run(|scope, ts| {
            let program = v8::Local::new(scope, program);
            let checker = v8::Local::new(scope, checker);
            routine(scope, ts, program, checker)
        })
    }

    /// Returns every loaded file, including libraries and imported dependencies.
    ///
    /// # Errors
    /// Returns compiler query errors.
    pub fn source_files(&mut self) -> Result<Vec<RepositorySourceFile>> {
        let root = self.root.clone();
        self.with_program(|scope, _, program, _| {
            let files = js::call(scope, program, "getSourceFiles", &[])?;
            js::items(scope, files)?
                .into_iter()
                .map(|file| source_file_record(scope, file, &root))
                .collect()
        })
    }

    /// Returns one root or imported file by repository-relative path.
    ///
    /// # Errors
    /// Fails if the program did not load the requested file.
    pub fn source_file(&mut self, relative_path: &str) -> Result<RepositorySourceFile> {
        let root = self.root.clone();
        self.with_program(|scope, _, program, _| {
            let path = js::string(scope, &paths::slash(&paths::join(&root, relative_path)));
            let file = js::call(scope, program, "getSourceFile", &[path])?;
            if file.is_null_or_undefined() {
                return Err(js::failure(format!(
                    "TypeScript project did not load {relative_path}"
                )));
            }
            source_file_record(scope, file, &root)
        })
    }

    /// Renders a loaded filename relative to this project's root.
    pub fn relative_path(&self, file_name: &Path) -> String {
        paths::relative(&self.root, file_name)
    }
}

fn source_file_record<'s>(
    scope: &mut Scope<'s, '_>,
    file: Val<'s>,
    root: &Path,
) -> Result<RepositorySourceFile> {
    let file_name = js::get_string(scope, file, "fileName")?;
    Ok(RepositorySourceFile {
        relative_path: paths::relative(root, Path::new(&file_name)),
        file_name,
        text: js::get_string(scope, file, "text")?,
        is_declaration_file: js::get_bool(scope, file, "isDeclarationFile")?,
    })
}

/// Disables the exact emit-only options ignored by semantic repository gates.
pub fn semantic_compiler_options(options: &mut Value) {
    if let Some(options) = options.as_object_mut() {
        options.insert("noEmit".to_owned(), Value::Bool(true));
        for key in [
            "composite",
            "declaration",
            "declarationMap",
            "sourceMap",
            "incremental",
        ] {
            options.insert(key.to_owned(), Value::Bool(false));
        }
    }
}

/// Resolves the workspace dependency, including the Rust build's support install.
///
/// Explicit library selection retains priority. The support install is used
/// only when ordinary workspace dependency resolution found no compiler.
///
/// # Errors
/// Preserves the ordinary lookup diagnostic when neither installation exists.
pub fn locate_repository_library(root: &Path) -> Result<PathBuf> {
    match locate_library(root) {
        Ok(library) => Ok(library),
        Err(error) => {
            let support = paths::resolve(root)
                .join("support/browser-dependencies/node_modules/typescript/lib/typescript.js");
            if support.is_file() {
                Ok(support)
            } else {
                Err(error)
            }
        }
    }
}
