//! Isolated package staging; repository sources and developer caches remain untouched.

use std::{
    fs::{self, File, FileTimes},
    path::Path,
    sync::LazyLock,
};

use regex::Regex;

use crate::{RUNTIME_DISTRIBUTION, runtime_suffixes};

/// Copies a source package into a new staging directory, preserving file metadata.
///
/// # Errors
/// Rejects an existing destination and propagates filesystem failures.
pub fn copy_package(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if ignored(&text) {
            continue;
        }
        let from = entry.path();
        let to = destination.join(name);
        if from.is_dir() {
            copy_package(&from, &to)?;
        } else {
            copy_file(&from, &to)?;
        }
    }
    copy_metadata(source, destination)?;
    Ok(())
}

fn ignored(name: &str) -> bool {
    matches!(
        name,
        ".venv" | ".pytest_cache" | "__pycache__" | "dist" | "node_modules"
    ) || name.strip_suffix(".pyc").is_some()
        || name.starts_with("seekdeep-jsonrpc-agent-pkg-")
        || name.starts_with("seekdeep-python-sdk-ffi-")
}

fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::copy(source, destination)?;
    copy_metadata(source, destination)
}

fn copy_metadata(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(source)?;
    fs::set_permissions(destination, metadata.permissions())?;
    let mut times = FileTimes::new();
    if let Ok(modified) = metadata.modified() {
        times = times.set_modified(modified);
    }
    if let Ok(accessed) = metadata.accessed() {
        times = times.set_accessed(accessed);
    }
    File::open(destination)?.set_times(times)?;
    Ok(())
}

/// Rewrites the first exact project version declaration, matching source staging.
///
/// # Errors
/// Rejects a missing declaration or an unreadable/unwritable project file.
pub fn rewrite_version(pyproject: &Path, version: &str) -> anyhow::Result<()> {
    static VERSION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?m)^version = "[^"]+"$"#).expect("project version line"));
    let text = read_text(pyproject)?;
    anyhow::ensure!(
        VERSION.is_match(&text),
        "could not rewrite version in {}",
        pyproject.display()
    );
    let text = VERSION.replacen(
        &text,
        1,
        regex::NoExpand(&format!("version = \"{version}\"")),
    );
    fs::write(pyproject, text.as_bytes())?;
    Ok(())
}

/// Copies the project license and optional runtime notices into the staged package.
///
/// # Errors
/// Rejects missing legal files or a project without an exact license declaration.
pub fn stage_license_files(
    root: &Path,
    destination: &Path,
    include_notices: bool,
) -> anyhow::Result<()> {
    static LICENSE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?m)^(license = "[^"]+")$"#).expect("project license line"));
    copy_file(&root.join("LICENSE"), &destination.join("LICENSE"))?;
    let license_files = if include_notices {
        copy_file(
            &root.join("THIRD_PARTY_NOTICES.md"),
            &destination.join("THIRD_PARTY_NOTICES.md"),
        )?;
        "[\"LICENSE\", \"THIRD_PARTY_NOTICES.md\"]"
    } else {
        "[\"LICENSE\"]"
    };
    let pyproject = destination.join("pyproject.toml");
    let text = read_text(&pyproject)?;
    anyhow::ensure!(
        LICENSE.is_match(&text),
        "could not declare license files in {}",
        pyproject.display()
    );
    let text = LICENSE.replacen(&text, 1, |captures: &regex::Captures<'_>| {
        format!("{}\nlicense-files = {license_files}", &captures[1])
    });
    fs::write(pyproject, text.as_bytes())?;
    Ok(())
}

/// Stages the SDK with an exact matching runtime dependency pin.
///
/// # Errors
/// Propagates package/license/version failures and rejects an absent runtime pin.
pub fn stage_sdk(root: &Path, destination: &Path, version: &str) -> anyhow::Result<()> {
    copy_package(&root.join("python/sdk"), destination)?;
    stage_license_files(root, destination, false)?;
    let pyproject = destination.join("pyproject.toml");
    rewrite_version(&pyproject, version)?;
    let pin = Regex::new(&format!(
        r#""{}==[^"]+""#,
        regex::escape(RUNTIME_DISTRIBUTION)
    ))?;
    let text = read_text(&pyproject)?;
    anyhow::ensure!(
        pin.is_match(&text),
        "SDK must contain exactly one runtime dependency pin"
    );
    let text = pin.replacen(
        &text,
        1,
        regex::NoExpand(&format!("\"{RUNTIME_DISTRIBUTION}=={version}\"")),
    );
    fs::write(pyproject, text.as_bytes())?;
    Ok(())
}

/// Stages one runtime executable and the manifest-required helper, preserving mode bits.
///
/// # Errors
/// Propagates package/legal/version failures and missing native payload errors.
pub fn stage_runtime(
    root: &Path,
    destination: &Path,
    version: &str,
    executable: &Path,
    executable_name: &str,
) -> anyhow::Result<()> {
    copy_package(&root.join("python/sdk-runtime"), destination)?;
    stage_license_files(root, destination, true)?;
    rewrite_version(&destination.join("pyproject.toml"), version)?;
    let runtime = destination.join("src/deepseek_harness_runtime/runtime");
    fs::create_dir_all(&runtime)?;
    for suffix in runtime_suffixes(executable_name) {
        let mut source = executable.as_os_str().to_owned();
        source.push(suffix);
        copy_file(
            Path::new(&source),
            &runtime.join(format!("{executable_name}{suffix}")),
        )?;
    }
    let binding_name = crate::runtime_binding_name(executable_name)?;
    let binding = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime executable parent is absent"))?
        .join(&binding_name);
    crate::executable::validate_native_library(
        &binding,
        &crate::runtime_binding_target(executable_name)?,
    )?;
    copy_file(&binding, &runtime.join(&binding_name))?;
    let package = runtime
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime package parent is absent"))?;
    for (name, text) in seekdeep_python_sdk::bindings::runtime_bindings(&binding_name)? {
        fs::write(package.join(name), text)?;
    }
    fs::write(destination.join("hatch_build.py"), NATIVE_HATCH_BINDING)?;
    Ok(())
}

fn read_text(path: &Path) -> anyhow::Result<String> {
    Ok(fs::read_to_string(path)?
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

pub(crate) const NATIVE_HATCH_BINDING: &str = r#""""Generated binding to the compiled Rust runtime-wheel policy."""
import json
import os
import subprocess
from pathlib import Path
from hatchling.builders.hooks.plugin.interface import BuildHookInterface

class RuntimeBuildHook(BuildHookInterface):
    def initialize(self, version, build_data):
        tool = os.environ.get("SEEKDEEP_PYTHON_RELEASE_TOOL")
        command = [tool] if tool else [
            "cargo", "run", "--quiet", "--manifest-path",
            str(Path(__file__).resolve().parents[2] / "Cargo.toml"),
            "--package", "seekdeep-python-release", "--bin", "seekdeep-python-release", "--",
        ]
        environment = os.environ.copy()
        environment["CARGO_INCREMENTAL"] = "0"
        environment.setdefault("CARGO_BUILD_JOBS", "2")
        result = subprocess.run(
            command + ["hook", "--root", self.root, "--version", version, "--target", self.target_name],
            capture_output=True, text=True, env=environment,
        )
        if result.returncode:
            raise RuntimeError(result.stderr.strip())
        build_data.update(json.loads(result.stdout))
"#;
