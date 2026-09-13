//! Build-artifact assembly, package verification, and ordered release operations.

mod assemble;
mod installed;
mod pack;
mod publish;
mod version;

pub use assemble::assemble_prebuilds;
pub use installed::{InstallVerification, PackedInstallOptions, verify_packed_install};
pub use pack::{PackOptions, pack_release, read_packed_manifest, tarball_name};
pub use publish::{PublishReport, integrity_of, publish_release};
pub use version::{ReleaseEnvironment, bump_release, commit_release, next_version, verify_release};

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, bail};
use serde_json::Value;

use crate::process::{CommandOutput, CommandSpec, ProcessFailure, Runner, run_checked};

fn command(program: &str, args: Vec<String>, cwd: &Path, capture: bool) -> CommandSpec {
    CommandSpec {
        program: program.to_owned(),
        args,
        cwd: cwd.to_owned(),
        env: BTreeMap::new(),
        capture,
        max_buffer: 1_048_576,
        inherit_stdin: !capture,
    }
}

fn manifest_string<'a>(manifest: &'a Value, field: &str) -> Result<&'a str> {
    let Some(value) = manifest.get(field).and_then(Value::as_str) else {
        bail!("package manifest lacks {field}");
    };
    Ok(value)
}

fn capture_checked(runner: &mut dyn Runner, spec: &CommandSpec) -> Result<CommandOutput> {
    let output = runner.run(spec)?;
    if let Some(error) = &output.spawn_error {
        return Err(ProcessFailure {
            status: output.status,
            message: error.clone(),
        }
        .into());
    }
    if output.status != Some(0) {
        return Err(ProcessFailure {
            status: output.status,
            message: output.stderr.trim_end_matches('\n').to_owned(),
        }
        .into());
    }
    Ok(output)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn relative_path(root: &Path, target: &Path) -> String {
    let root_parts = root.components().collect::<Vec<_>>();
    let target_parts = target.components().collect::<Vec<_>>();
    let common = root_parts
        .iter()
        .zip(&target_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = std::path::PathBuf::new();
    for _ in common..root_parts.len() {
        result.push("..");
    }
    for part in &target_parts[common..] {
        result.push(part.as_os_str());
    }
    path_string(&result)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Converts a command error to the source-compatible process status.
pub fn finish_cli(result: Result<()>) {
    if let Err(error) = result {
        if !error.to_string().is_empty() {
            eprintln!("{error:#}");
        }
        std::process::exit(crate::process::exit_code(&error));
    }
}

/// Resolves the optional fixture/repository override and preserves the source command arguments.
///
/// # Errors
///
/// Returns for a missing root argument or inaccessible current directory.
pub fn cli_repository_args() -> Result<(crate::repo::Repository, Vec<String>)> {
    let mut root = crate::repo::default_root();
    let mut arguments = Vec::new();
    let mut input = std::env::args().skip(1);
    while let Some(argument) = input.next() {
        if argument == "--root" {
            let Some(value) = input.next() else {
                bail!("--root requires a directory");
            };
            root = resolve_cli_path(Some(&value), &root)?;
        } else {
            arguments.push(argument);
        }
    }
    Ok((crate::repo::Repository::new(root), arguments))
}

/// Resolves a positional path as Node's `path.resolve`, retaining the default when absent.
///
/// # Errors
///
/// Returns when a relative path cannot be resolved against the current directory.
pub fn resolve_cli_path(value: Option<&str>, default: &Path) -> Result<std::path::PathBuf> {
    let path = value
        .filter(|value| !value.is_empty())
        .map_or(default, Path::new);
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = std::path::PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}
