//! Rust implementation of the compiler's `System` contract.
#![expect(
    clippy::needless_pass_by_value,
    reason = "V8 callback ABI passes arguments by value"
)]
//!
//! Every filesystem effect the compiler performs during analysis crosses this
//! boundary, mirroring the Node system the source relies on: BOM-aware reads,
//! sorted directory listings, canonical realpaths, and case detection.

use std::path::{Path, PathBuf};

use v8::MapFnTo as _;

use crate::Result;

use super::{
    js::{self, Scope, Val},
    paths,
};

/// Installs the Rust system on the compiler namespace through `setSys`.
pub(crate) fn install<'s>(scope: &mut Scope<'s, '_>, ts: Val<'s>, library: &Path) -> Result<()> {
    let case_sensitive = !swap_case(&library.to_string_lossy())
        .parse::<PathBuf>()
        .is_ok_and(|swapped| swapped.is_file());
    let library_value = js::string(scope, &library.to_string_lossy());
    let case_value = js::boolean(scope, case_sensitive);
    let entries = function(
        scope,
        file_system_entries.map_fn_to(),
        &[("library", library_value)],
    )?;
    let realpath = function(scope, real_path.map_fn_to(), &[])?;
    let data = js::object(
        scope,
        &[
            ("library", library_value),
            ("caseSensitive", case_value),
            ("ts", ts),
            ("entries", entries),
            ("realpath", realpath),
        ],
    )?;
    let mut members: Vec<(&str, Val<'_>)> = vec![
        ("args", js::array(scope, &[])),
        ("newLine", js::string(scope, "\n")),
        ("useCaseSensitiveFileNames", case_value),
        ("realpath", realpath),
    ];
    let callbacks: [(&str, v8::FunctionCallback); 16] = [
        ("write", write.map_fn_to()),
        ("readFile", read_file.map_fn_to()),
        ("writeFile", write_file.map_fn_to()),
        ("resolvePath", resolve_path.map_fn_to()),
        ("fileExists", file_exists.map_fn_to()),
        ("directoryExists", directory_exists.map_fn_to()),
        ("createDirectory", create_directory.map_fn_to()),
        ("getExecutingFilePath", executing_file_path.map_fn_to()),
        ("getCurrentDirectory", current_directory.map_fn_to()),
        ("getDirectories", directories.map_fn_to()),
        ("readDirectory", read_directory.map_fn_to()),
        ("exit", exit.map_fn_to()),
        ("getEnvironmentVariable", environment_variable.map_fn_to()),
        ("getModifiedTime", modified_time.map_fn_to()),
        ("setModifiedTime", set_modified_time.map_fn_to()),
        ("deleteFile", delete_file.map_fn_to()),
    ];
    for (name, callback) in callbacks {
        let built = v8::Function::builder_raw(callback)
            .data(data)
            .build(scope)
            .ok_or_else(|| js::failure(format!("cannot create system callback {name}")))?;
        members.push((name, built.into()));
    }
    let system = js::object(scope, &members)?;
    js::call(scope, ts, "setSys", &[system])?;
    Ok(())
}

fn function<'s>(
    scope: &mut Scope<'s, '_>,
    callback: v8::FunctionCallback,
    data: &[(&str, Val<'s>)],
) -> Result<Val<'s>> {
    let data = js::object(scope, data)?;
    Ok(v8::Function::builder_raw(callback)
        .data(data)
        .build(scope)
        .ok_or_else(|| js::failure("cannot create system helper"))?
        .into())
}

fn swap_case(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() {
                character.to_ascii_uppercase()
            } else if character.is_ascii_uppercase() {
                character.to_ascii_lowercase()
            } else {
                character
            }
        })
        .collect()
}

fn argument_path(
    scope: &mut Scope<'_, '_>,
    args: &v8::FunctionCallbackArguments<'_>,
    index: i32,
) -> PathBuf {
    PathBuf::from(args.get(index).to_rust_string_lossy(scope))
}

fn throw(scope: &mut Scope<'_, '_>, message: &str) {
    let Some(message) = v8::String::new(scope, message) else {
        return;
    };
    let error = v8::Exception::error(scope, message);
    scope.throw_exception(error);
}

fn write(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    print!("{}", args.get(0).to_rust_string_lossy(scope));
}

/// Decodes a file the way the Node system does: UTF-16 by BOM, otherwise UTF-8.
pub(crate) fn decode_file(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn read_file<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let path = argument_path(scope, &args, 0);
    match std::fs::read(&path) {
        Ok(bytes) => result.set(js::string(scope, &decode_file(&bytes))),
        Err(_) => result.set_undefined(),
    }
}

fn write_file(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    let path = argument_path(scope, &args, 0);
    let mut data = args.get(1).to_rust_string_lossy(scope);
    if args.get(2).boolean_value(scope) {
        data.insert(0, '\u{FEFF}');
    }
    if let Err(error) = std::fs::write(&path, data) {
        throw(scope, &format!("{}: {error}", path.display()));
    }
}

fn resolve_path<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let path = argument_path(scope, &args, 0);
    let resolved = paths::resolve(&path);
    result.set(js::string(scope, &resolved.to_string_lossy()));
}

fn file_exists(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    mut result: v8::ReturnValue<'_>,
) {
    let path = argument_path(scope, &args, 0);
    result.set_bool(path.is_file());
}

fn directory_exists(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    mut result: v8::ReturnValue<'_>,
) {
    let path = argument_path(scope, &args, 0);
    result.set_bool(path.is_dir());
}

fn create_directory(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    let path = argument_path(scope, &args, 0);
    if let Err(error) = std::fs::create_dir(&path)
        && !path.is_dir()
    {
        throw(scope, &format!("{}: {error}", path.display()));
    }
}

fn executing_file_path<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    match js::get(scope, args.data(), "library") {
        Ok(library) => result.set(library),
        Err(error) => throw(scope, &error.to_string()),
    }
}

fn current_directory<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    result.set(js::string(scope, &directory.to_string_lossy()));
}

/// Sorted accessible entries, following symbolic links the way Node's `statSync` does.
fn entries_of(path: &Path) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let Ok(entries) = std::fs::read_dir(if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    }) else {
        return (files, directories);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if metadata.is_file() {
            files.push(name);
        } else if metadata.is_dir() {
            directories.push(name);
        }
    }
    files.sort_by(|left, right| crate::text::utf16_compare(left, right));
    directories.sort_by(|left, right| crate::text::utf16_compare(left, right));
    (files, directories)
}

fn string_array<'s>(scope: &mut Scope<'s, '_>, values: &[String]) -> Val<'s> {
    let items = values
        .iter()
        .map(|value| js::string(scope, value))
        .collect::<Vec<_>>();
    js::array(scope, &items)
}

fn directories<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let path = argument_path(scope, &args, 0);
    let (_, directories) = entries_of(&path);
    result.set(string_array(scope, &directories));
}

fn file_system_entries<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let path = argument_path(scope, &args, 0);
    let (files, directories) = entries_of(&path);
    let files = string_array(scope, &files);
    let directories = string_array(scope, &directories);
    match js::object(scope, &[("files", files), ("directories", directories)]) {
        Ok(value) => result.set(value),
        Err(error) => throw(scope, &error.to_string()),
    }
}

fn read_directory<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let data = args.data();
    let outcome = (|| -> Result<Val<'s>> {
        let ts = js::get(scope, data, "ts")?;
        let case_sensitive = js::get(scope, data, "caseSensitive")?;
        let entries = js::get(scope, data, "entries")?;
        let realpath = js::get(scope, data, "realpath")?;
        let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let current = js::string(scope, &directory.to_string_lossy());
        js::call(
            scope,
            ts,
            "matchFiles",
            &[
                args.get(0),
                args.get(1),
                args.get(2),
                args.get(3),
                case_sensitive,
                current,
                args.get(4),
                entries,
                realpath,
            ],
        )
    })();
    match outcome {
        Ok(value) => result.set(value),
        Err(error) => throw(scope, &error.to_string()),
    }
}

fn exit(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    let code = args.get(0).to_rust_string_lossy(scope);
    throw(scope, &format!("compiler requested process exit {code}"));
}

fn environment_variable<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let value = std::env::var(name).unwrap_or_default();
    result.set(js::string(scope, &value));
}

fn real_path<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let path = argument_path(scope, &args, 0);
    let resolved = std::fs::canonicalize(&path).unwrap_or(path);
    result.set(js::string(scope, &resolved.to_string_lossy()));
}

fn modified_time<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue<'s>,
) {
    let path = argument_path(scope, &args, 0);
    let Some(millis) = std::fs::metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64() * 1000.0)
    else {
        result.set_undefined();
        return;
    };
    match v8::Date::new(scope, millis) {
        Some(date) => result.set(date.into()),
        None => result.set_undefined(),
    }
}

fn set_modified_time(
    _: &mut v8::PinScope<'_, '_>,
    _: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
}

fn delete_file(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    let path = argument_path(scope, &args, 0);
    let _ = std::fs::remove_file(path);
}
