//! The TypeScript compiler library hosted in a Rust-owned engine.
//!
//! Rust owns the isolate, the filesystem system object, and every analysis
//! decision; the compiler library supplies parsing, binding, and checking.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Once,
};

use crate::{Result, TypertGeneratorError};

use super::{
    js::{self, Scope, Val},
    system,
};

static INITIALIZE: Once = Once::new();

/// Native stack reserved for analysis threads started by [`run_with_stack`].
pub(crate) const ANALYSIS_STACK_BYTES: usize = 512 * 1024 * 1024;

thread_local! {
    static ANALYSIS_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn initialize_engine() {
    INITIALIZE.call_once(|| {
        if ANALYSIS_THREAD.with(std::cell::Cell::get) {
            // Model conversion recurses through Rust frames between compiler
            // calls, so the engine's stack guard must span the analysis thread.
            let kilobytes = (ANALYSIS_STACK_BYTES - 32 * 1024 * 1024) / 1024;
            v8::V8::set_flags_from_string(&format!("--stack-size={kilobytes}"));
        }
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// Runs `routine` on a thread whose stack accommodates deep workspace analysis.
///
/// The first engine initialization in the process happens on such a thread, so
/// call this before any other compiler use when analyzing real workspaces.
///
/// # Panics
/// Propagates a panic from the analysis thread.
pub fn run_with_stack<R: Send + 'static>(routine: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::Builder::new()
        .name("typert-analysis".to_owned())
        .stack_size(ANALYSIS_STACK_BYTES)
        .spawn(move || {
            ANALYSIS_THREAD.with(|flag| flag.set(true));
            routine()
        })
        .expect("spawn analysis thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Numeric compiler enumerations captured once per engine.
#[derive(Debug, Default, Clone)]
pub(crate) struct Constants {
    syntax_kind: HashMap<String, u32>,
    syntax_kind_names: HashMap<u32, String>,
    type_flags: HashMap<String, u32>,
    symbol_flags: HashMap<String, u32>,
    element_flags: HashMap<String, u32>,
    other: HashMap<String, u32>,
}

impl Constants {
    fn capture<'s>(scope: &mut Scope<'s, '_>, ts: Val<'s>) -> Result<Self> {
        let mut constants = Self::default();
        constants.syntax_kind = enumeration(scope, ts, "SyntaxKind")?;
        constants.syntax_kind_names = constants
            .syntax_kind
            .iter()
            .map(|(name, value)| (*value, name.clone()))
            .collect();
        for (name, value) in enumeration_names(scope, ts, "SyntaxKind")? {
            constants.syntax_kind_names.insert(name, value);
        }
        constants.type_flags = enumeration(scope, ts, "TypeFlags")?;
        constants.symbol_flags = enumeration(scope, ts, "SymbolFlags")?;
        constants.element_flags = enumeration(scope, ts, "ElementFlags")?;
        for group in [
            "IndexKind",
            "TypeFormatFlags",
            "NodeBuilderFlags",
            "EmitHint",
            "ScriptTarget",
            "ModuleKind",
            "JsxEmit",
            "ScriptKind",
        ] {
            for (name, value) in enumeration(scope, ts, group)? {
                constants.other.insert(format!("{group}.{name}"), value);
            }
        }
        Ok(constants)
    }

    pub(crate) fn kind(&self, name: &str) -> u32 {
        *self
            .syntax_kind
            .get(name)
            .unwrap_or_else(|| panic!("compiler SyntaxKind.{name} is absent"))
    }

    /// `ts.SyntaxKind[kind]`: the first member spelled with this value.
    pub(crate) fn kind_name(&self, kind: u32) -> String {
        self.syntax_kind_names
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| kind.to_string())
    }

    pub(crate) fn type_flag(&self, name: &str) -> u32 {
        *self
            .type_flags
            .get(name)
            .unwrap_or_else(|| panic!("compiler TypeFlags.{name} is absent"))
    }

    pub(crate) fn symbol_flag(&self, name: &str) -> u32 {
        *self
            .symbol_flags
            .get(name)
            .unwrap_or_else(|| panic!("compiler SymbolFlags.{name} is absent"))
    }

    pub(crate) fn element_flag(&self, name: &str) -> u32 {
        *self
            .element_flags
            .get(name)
            .unwrap_or_else(|| panic!("compiler ElementFlags.{name} is absent"))
    }

    /// Any other captured enumeration member, spelled `Group.Member`.
    pub(crate) fn value(&self, name: &str) -> u32 {
        *self
            .other
            .get(name)
            .unwrap_or_else(|| panic!("compiler constant {name} is absent"))
    }
}

fn enumeration<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    name: &str,
) -> Result<HashMap<String, u32>> {
    let value = js::get(scope, ts, name)?;
    let json = js::to_json(scope, value)?;
    let members = json
        .as_object()
        .ok_or_else(|| js::failure(format!("compiler enumeration {name} is not an object")))?;
    Ok(members
        .iter()
        .filter_map(|(member, value)| Some((member.clone(), u32::try_from(value.as_u64()?).ok()?)))
        .collect())
}

/// Reverse members (`SyntaxKind[kind]`) in the compiler's own precedence.
fn enumeration_names<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    name: &str,
) -> Result<Vec<(u32, String)>> {
    let value = js::get(scope, ts, name)?;
    let json = js::to_json(scope, value)?;
    let members = json
        .as_object()
        .ok_or_else(|| js::failure(format!("compiler enumeration {name} is not an object")))?;
    Ok(members
        .iter()
        .filter_map(|(member, value)| {
            Some((member.parse::<u32>().ok()?, value.as_str()?.to_owned()))
        })
        .collect())
}

/// One loaded compiler library and its Rust-owned system.
pub struct Compiler {
    context: v8::Global<v8::Context>,
    ts: v8::Global<v8::Object>,
    isolate: v8::OwnedIsolate,
    library: PathBuf,
    constants: Constants,
}

impl std::fmt::Debug for Compiler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Compiler")
            .field("library", &self.library)
            .finish_non_exhaustive()
    }
}

impl Compiler {
    /// Loads `typescript.js` from `library` and installs the Rust filesystem system.
    ///
    /// # Errors
    /// Reports an unreadable library, a script that does not define the compiler
    /// namespace, or a namespace missing the system hooks the analyzer relies on.
    pub fn load(library: &Path) -> Result<Self> {
        initialize_engine();
        let library = super::paths::resolve(library);
        let source = std::fs::read_to_string(&library).map_err(|error| {
            TypertGeneratorError::Workspace(format!(
                "typert-generator: cannot read compiler library {}: {error}",
                library.display()
            ))
        })?;
        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        let (context, ts, constants) = {
            v8::scope!(let handles, &mut isolate);
            let context = v8::Context::new(handles, v8::ContextOptions::default());
            let scope = &mut v8::ContextScope::new(handles, context);
            js::evaluate(scope, &source, &library.to_string_lossy())?;
            let global = context.global(scope);
            let ts = js::get(scope, global.into(), "ts")?;
            let namespace = ts
                .to_object(scope)
                .filter(|_| ts.is_object())
                .ok_or_else(|| {
                    TypertGeneratorError::Workspace(format!(
                        "typert-generator: {} does not define the compiler namespace",
                        library.display()
                    ))
                })?;
            system::install(scope, ts, &library)?;
            let constants = Constants::capture(scope, ts)?;
            (
                v8::Global::new(scope, context),
                v8::Global::new(scope, namespace),
                constants,
            )
        };
        Ok(Self {
            context,
            ts,
            isolate,
            library,
            constants,
        })
    }

    /// Path of the loaded compiler library.
    pub fn library(&self) -> &Path {
        &self.library
    }

    pub(crate) fn constants(&self) -> &Constants {
        &self.constants
    }

    /// Runs one analysis routine against the compiler namespace.
    pub(crate) fn run<R>(
        &mut self,
        routine: impl for<'s, 'i> FnOnce(&mut Scope<'s, 'i>, Val<'s>) -> Result<R>,
    ) -> Result<R> {
        v8::scope!(let handles, &mut self.isolate);
        let context = v8::Local::new(handles, &self.context);
        let scope = &mut v8::ContextScope::new(handles, context);
        let ts = v8::Local::new(scope, &self.ts);
        routine(scope, ts.into())
    }
}

/// Locates the compiler library for a workspace the way the source's package
/// dependency resolves it.
///
/// # Errors
/// Reports a workspace whose `node_modules` carries no TypeScript library.
pub fn locate_library(root: &Path) -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("SEEKDEEP_TYPESCRIPT_LIBRARY") {
        return Ok(PathBuf::from(explicit));
    }
    let mut current = Some(super::paths::resolve(root));
    while let Some(directory) = current {
        let candidate = directory.join("node_modules/typescript/lib/typescript.js");
        if candidate.is_file() {
            return Ok(candidate);
        }
        current = directory.parent().map(Path::to_owned);
    }
    Err(TypertGeneratorError::Workspace(format!(
        "typert-generator: cannot find node_modules/typescript/lib/typescript.js above {}",
        root.display()
    )))
}
