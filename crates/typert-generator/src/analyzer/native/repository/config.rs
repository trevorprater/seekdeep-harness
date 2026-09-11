//! Compiler-owned JSONC configuration parsing and Host reference flattening.

use std::{collections::HashSet, path::Path};

use indexmap::IndexSet;
use serde_json::Value;
use v8::MapFnTo as _;

use crate::Result;

use super::{
    super::{
        js::{self, Scope, Val},
        paths,
    },
    RepositoryConfig,
};

pub(super) fn read_config<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    path: &Path,
) -> Result<Value> {
    let system = js::get(scope, ts, "sys")?;
    let read_file = js::get(scope, system, "readFile")?;
    let path = js::string(scope, &paths::slash(&paths::resolve(path)));
    let read = js::call(scope, ts, "readConfigFile", &[path, read_file])?;
    if let Some(error) = js::get_defined(scope, read, "error")? {
        return Err(js::failure(flatten_diagnostic(scope, ts, error)?));
    }
    let config = js::get(scope, read, "config")?;
    js::to_json(scope, config)
}

pub(super) fn parse_config<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    path: &Path,
    current_directory: &Path,
) -> Result<RepositoryConfig> {
    let system = js::get(scope, ts, "sys")?;
    let directory = js::string(scope, &paths::slash(current_directory));
    let data = js::object(scope, &[("ts", ts), ("directory", directory)])?;
    let directory_callback = callback(scope, current_directory_callback.map_fn_to(), data)?;
    let diagnostic_callback = callback(scope, unrecoverable_diagnostic.map_fn_to(), data)?;
    let mut entries = vec![
        ("getCurrentDirectory", directory_callback),
        ("onUnRecoverableConfigFileDiagnostic", diagnostic_callback),
    ];
    for name in [
        "useCaseSensitiveFileNames",
        "readDirectory",
        "fileExists",
        "readFile",
    ] {
        entries.push((name, js::get(scope, system, name)?));
    }
    let host = js::object(scope, &entries)?;
    let path_value = js::string(scope, &paths::slash(&paths::resolve(path)));
    let existing_options = js::object(scope, &[])?;
    let get_parsed = js::get(scope, ts, "getParsedCommandLineOfConfigFile")?;
    let parsed = js::invoke(scope, get_parsed, ts, &[path_value, existing_options, host])?;
    if parsed.is_null_or_undefined() {
        return Err(js::failure(format!(
            "cannot parse TypeScript config {}",
            path.display()
        )));
    }
    let errors = js::get_items(scope, parsed, "errors")?;
    if !errors.is_empty() {
        let messages = errors
            .into_iter()
            .map(|error| flatten_diagnostic(scope, ts, error))
            .collect::<Result<Vec<_>>>()?;
        return Err(js::failure(messages.join("\n")));
    }
    let file_names = js::get_items(scope, parsed, "fileNames")?
        .into_iter()
        .map(|value| js::text(scope, value))
        .collect();
    let options = js::get(scope, parsed, "options")?;
    let options = js::to_json(scope, options)?;
    let project_references = js::get_items(scope, parsed, "projectReferences")?
        .into_iter()
        .map(|reference| {
            let path = js::call(scope, ts, "resolveProjectReferencePath", &[reference])?;
            Ok(js::text(scope, path))
        })
        .collect::<Result<_>>()?;
    Ok(RepositoryConfig {
        file_names,
        options,
        project_references,
    })
}

pub(super) fn load_project_graph<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    root: &Path,
) -> Result<(Vec<String>, Value)> {
    let root_path = root.join("tsconfig.host.json");
    let current_directory =
        std::env::current_dir().map_err(|error| js::failure(error.to_string()))?;
    let parsed = parse_config(scope, ts, &root_path, &current_directory)?;
    let options = parsed.options.clone();
    let mut roots = IndexSet::new();
    let mut visited = HashSet::new();
    collect(
        scope,
        ts,
        &paths::slash(&root_path),
        parsed,
        &current_directory,
        &mut visited,
        &mut roots,
    )?;
    Ok((roots.into_iter().collect(), options))
}

fn collect<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    path: &str,
    parsed: RepositoryConfig,
    current_directory: &Path,
    visited: &mut HashSet<String>,
    roots: &mut IndexSet<String>,
) -> Result<()> {
    if !visited.insert(path.to_owned()) {
        return Ok(());
    }
    roots.extend(parsed.file_names);
    for reference in parsed.project_references {
        let parsed = parse_config(scope, ts, Path::new(&reference), current_directory)?;
        collect(
            scope,
            ts,
            &reference,
            parsed,
            current_directory,
            visited,
            roots,
        )?;
    }
    Ok(())
}

pub(super) fn flatten_diagnostic<'s>(
    scope: &mut Scope<'s, '_>,
    ts: Val<'s>,
    error: Val<'s>,
) -> Result<String> {
    let text = js::get(scope, error, "messageText")?;
    let newline = js::string(scope, "\n");
    let text = js::call(scope, ts, "flattenDiagnosticMessageText", &[text, newline])?;
    Ok(js::text(scope, text))
}

pub(super) fn callback<'s>(
    scope: &mut Scope<'s, '_>,
    function: v8::FunctionCallback,
    data: Val<'s>,
) -> Result<Val<'s>> {
    Ok(v8::Function::builder_raw(function)
        .data(data)
        .build(scope)
        .ok_or_else(|| js::failure("cannot create repository compiler callback"))?
        .into())
}

pub(super) fn throw(scope: &mut Scope<'_, '_>, error: impl std::fmt::Display) {
    if let Some(message) = v8::String::new(scope, &error.to_string()) {
        let error = v8::Exception::error(scope, message);
        scope.throw_exception(error);
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn current_directory_callback<'s>(
    scope: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    match js::get(scope, args.data(), "directory") {
        Ok(directory) => result.set(directory),
        Err(error) => throw(scope, error),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
fn unrecoverable_diagnostic<'s>(
    scope: &mut Scope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _: v8::ReturnValue<'s>,
) {
    let error =
        js::get(scope, args.data(), "ts").and_then(|ts| flatten_diagnostic(scope, ts, args.get(0)));
    match error {
        Ok(error) => throw(scope, error),
        Err(error) => throw(scope, error),
    }
}
