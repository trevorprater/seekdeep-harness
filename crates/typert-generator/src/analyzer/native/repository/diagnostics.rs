//! Read-only compilation of virtual sources against built declarations.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use v8::MapFnTo as _;

use crate::Result;

use super::{
    super::{
        js::{self, Scope, Val},
        paths,
    },
    RepositoryCompiler, config,
};

/// A compiler source file whose contents need not exist on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualSource {
    /// Absolute path used for module and type resolution.
    pub file_name: String,
    /// TypeScript source text.
    pub text: String,
}

/// Compiler diagnostics rendered using the source command's plain formatter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerDiagnostics {
    /// Diagnostics in the compiler's own sorted order.
    pub codes: Vec<u32>,
    /// Plain, newline-terminated TypeScript diagnostics.
    pub formatted: String,
}

impl CompilerDiagnostics {
    /// Whether the compiler accepted every source and its dependencies.
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
}

impl RepositoryCompiler {
    /// Checks virtual files without writing source files, declarations, or caches.
    ///
    /// `options` are resolved compiler options, as returned by `parse_config`.
    /// All diagnostics are retained, including option and dependency failures.
    ///
    /// # Errors
    /// Returns compiler initialization/query failures and rejects any attempted emit.
    pub fn compile_no_emit(
        &mut self,
        root: &Path,
        options: &Value,
        sources: &[VirtualSource],
    ) -> Result<CompilerDiagnostics> {
        self.compiler.run(|scope, ts| {
            let options = js::from_json(scope, options)?;
            let parents = js::boolean(scope, true);
            let host = js::call(scope, ts, "createCompilerHost", &[options, parents])?;
            let source_map = js::JsMap::new(scope);
            let mut root_names = Vec::new();
            for source in sources {
                let path = paths::slash(&paths::resolve(Path::new(&source.file_name)));
                let path = js::string(scope, &path);
                let text = js::string(scope, &source.text);
                source_map.set(scope, path, text);
                root_names.push(path);
            }
            let directory = js::string(scope, &paths::slash(root));
            let mut entries = vec![
                ("ts", ts),
                ("host", host),
                ("sources", source_map.value()),
                ("directory", directory),
            ];
            for name in ["fileExists", "readFile", "getSourceFile"] {
                entries.push((name, js::get(scope, host, name)?));
            }
            let data = js::object(scope, &entries)?;
            for (name, implementation) in [
                ("fileExists", virtual_file_exists.map_fn_to()),
                ("readFile", virtual_read_file.map_fn_to()),
                ("getSourceFile", virtual_source_file.map_fn_to()),
                ("writeFile", reject_emit.map_fn_to()),
            ] {
                let callback = config::callback(scope, implementation, data)?;
                js::set(scope, host, name, callback)?;
            }
            let roots = js::array(scope, &root_names);
            let program = js::call(scope, ts, "createProgram", &[roots, options, host])?;
            let diagnostics = js::call(scope, ts, "getPreEmitDiagnostics", &[program])?;
            let codes = js::items(scope, diagnostics)?
                .into_iter()
                .map(|diagnostic| js::get_number(scope, diagnostic, "code").map(js::integer))
                .collect::<Result<Vec<_>>>()?;
            let canonical = config::callback(scope, canonical_file_name.map_fn_to(), data)?;
            let directory = config::callback(scope, current_directory.map_fn_to(), data)?;
            let newline = config::callback(scope, new_line.map_fn_to(), data)?;
            let format_host = js::object(
                scope,
                &[
                    ("getCanonicalFileName", canonical),
                    ("getCurrentDirectory", directory),
                    ("getNewLine", newline),
                ],
            )?;
            let formatted = js::call(scope, ts, "formatDiagnostics", &[diagnostics, format_host])?;
            Ok(CompilerDiagnostics {
                codes,
                formatted: js::text(scope, formatted),
            })
        })
    }
}

fn virtual_source<'s>(
    scope: &mut Scope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
) -> Result<Option<Val<'s>>> {
    let file_name = js::text(scope, args.get(0));
    let path = js::string(scope, &paths::slash(&paths::resolve(Path::new(&file_name))));
    let sources = js::get(scope, args.data(), "sources")?;
    let sources = js::JsMap::from_value(sources)?;
    Ok(sources.get(scope, path))
}

fn base_call<'s>(
    scope: &mut Scope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    name: &str,
) -> Result<Val<'s>> {
    let host = js::get(scope, args.data(), "host")?;
    let base = js::get(scope, args.data(), name)?;
    let arguments = (0..args.length())
        .map(|index| args.get(index))
        .collect::<Vec<_>>();
    js::invoke(scope, base, host, &arguments)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn virtual_file_exists<'s>(
    scope: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let outcome = virtual_source(scope, &args).and_then(|source| {
        if source.is_some() {
            Ok(js::boolean(scope, true))
        } else {
            base_call(scope, &args, "fileExists")
        }
    });
    match outcome {
        Ok(value) => result.set(value),
        Err(error) => config::throw(scope, error),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn virtual_read_file<'s>(
    scope: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let outcome = virtual_source(scope, &args)
        .and_then(|source| source.map_or_else(|| base_call(scope, &args, "readFile"), Ok));
    match outcome {
        Ok(value) => result.set(value),
        Err(error) => config::throw(scope, error),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn virtual_source_file<'s>(
    scope: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let outcome = (|| {
        let Some(source) = virtual_source(scope, &args)? else {
            return base_call(scope, &args, "getSourceFile");
        };
        let ts = js::get(scope, args.data(), "ts")?;
        let parents = js::boolean(scope, true);
        js::call(
            scope,
            ts,
            "createSourceFile",
            &[args.get(0), source, args.get(1), parents],
        )
    })();
    match outcome {
        Ok(value) => result.set(value),
        Err(error) => config::throw(scope, error),
    }
}

fn reject_emit(
    scope: &mut Scope<'_, '_>,
    _: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    config::throw(
        scope,
        "doc-typecheck: noEmit compilation attempted to write output",
    );
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn canonical_file_name<'s>(
    _: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    result.set(args.get(0));
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn current_directory<'s>(
    scope: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    match js::get(scope, args.data(), "directory") {
        Ok(value) => result.set(value),
        Err(error) => config::throw(scope, error),
    }
}

fn new_line<'s>(
    scope: &mut Scope<'s, '_>,
    _: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    result.set(js::string(scope, "\n"));
}
