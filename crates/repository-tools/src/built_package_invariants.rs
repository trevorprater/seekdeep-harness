//! Staged plain-Node verification of compiled package invariant companions.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

use crate::repo_files::repository_glob_matches;

/// Built companion verification report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltInvariantReport {
    /// Package manifests inspected.
    pub checked: usize,
    /// Ordered package failures.
    pub failures: Vec<String>,
}

/// Verifies every compiled companion through a staged package self-reference.
///
/// # Errors
///
/// Returns traversal, manifest, staging, copy, process, or cleanup failures.
pub fn verify_built_package_invariants(
    root: &Path,
    loader_url: Option<&str>,
) -> anyhow::Result<BuiltInvariantReport> {
    let loader_url = match loader_url {
        Some(loader_url) => loader_url.to_owned(),
        None => url::Url::from_file_path(root.join("vendor/loader/lib/index.js"))
            .map_err(|()| anyhow::anyhow!("cannot form Loader file URL"))?
            .to_string(),
    };
    let mut manifests = package_manifests(root)?;
    manifests.sort_by(|left, right| {
        relative(root, left)
            .encode_utf16()
            .cmp(relative(root, right).encode_utf16())
    });
    let mut failures = Vec::new();
    for manifest_path in &manifests {
        let relative_manifest = relative(root, manifest_path);
        let package_dir = manifest_path.parent().unwrap_or(root);
        let manifest = serde_json::from_str::<Value>(&std::fs::read_to_string(manifest_path)?)?;
        let Some(package_name) = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            failures.push(format!("{relative_manifest}: missing package name"));
            continue;
        };
        let invariant = manifest
            .get("exports")
            .and_then(Value::as_object)
            .and_then(|exports| exports.get("./invariant"))
            .and_then(Value::as_object);
        let published = manifest
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|files| {
                files
                    .iter()
                    .any(|file| file.as_str() == Some("lib/invariant.js"))
            });
        if invariant
            .and_then(|entry| entry.get("default"))
            .and_then(Value::as_str)
            != Some("./lib/invariant.js")
            || !published
        {
            failures.push(format!(
                "{package_name}: manifest does not publish ./lib/invariant.js as ./invariant"
            ));
            continue;
        }
        let staging = StagedPackage::new(package_dir)?;
        let result = stage_and_probe(
            package_dir,
            &staging.path,
            &manifest,
            package_name,
            &loader_url,
        );
        if let Err(error) = result {
            failures.push(format!("{package_name}: {error}"));
        }
    }
    Ok(BuiltInvariantReport {
        checked: manifests.len(),
        failures,
    })
}

/// Renders the source-compatible report.
#[must_use]
pub fn render_built_invariant_report(report: &BuiltInvariantReport) -> String {
    if report.failures.is_empty() {
        return format!(
            "verify-built-package-invariants: {} compiled companion(s) passed plain-Node Loader checks.\n",
            report.checked
        );
    }
    let mut output = "verify-built-package-invariants: compiled companion failures:\n".to_owned();
    for failure in &report.failures {
        let _ = writeln!(output, "  {failure}");
    }
    output
}

fn stage_and_probe(
    package_dir: &Path,
    staging: &Path,
    manifest: &Value,
    package_name: &str,
    loader_url: &str,
) -> anyhow::Result<()> {
    std::fs::copy(
        package_dir.join("package.json"),
        staging.join("package.json"),
    )?;
    let patterns = manifest
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|pattern| pattern.starts_with("lib/"))
        .collect::<Vec<_>>();
    if package_dir.join("lib").is_dir() {
        for entry in walkdir::WalkDir::new(package_dir.join("lib")) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = relative(package_dir, entry.path());
            if !patterns.iter().any(|pattern| {
                repository_glob_matches(pattern, &relative)
                    || repository_glob_matches(&format!("{pattern}/**"), &relative)
            }) {
                continue;
            }
            let target = staging.join(&relative);
            std::fs::create_dir_all(target.parent().unwrap_or(staging))?;
            std::fs::copy(entry.path(), target)?;
        }
    }
    let probe = format!(
        "import Loader from {}\nimport * as companion from {}\nconst loader = Object.create(Loader.prototype)\nif ('default' in companion) throw new Error('companion has a default export')\nconst unwrapped = loader.unwrapExports(companion)\nif (unwrapped !== companion) throw new Error('Loader collapsed the companion namespace')\nif (typeof unwrapped.name !== 'string') throw new Error('companion name is missing')\nif (!Array.isArray(unwrapped.inject) || !unwrapped.inject.includes('invariants')) throw new Error('companion does not inject invariants')\nif (typeof unwrapped.apply !== 'function') throw new Error('companion apply is missing')\n",
        serde_json::to_string(loader_url)?,
        serde_json::to_string(&format!("{package_name}/invariant"))?
    );
    let probe_path = staging.join("probe.mjs");
    std::fs::write(&probe_path, probe)?;
    let output = Command::new(node_executable()).arg(&probe_path).output()?;
    if !output.status.success() {
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        anyhow::bail!("{}", diagnostic.trim());
    }
    Ok(())
}

struct StagedPackage {
    path: PathBuf,
}

impl StagedPackage {
    fn new(package_dir: &Path) -> anyhow::Result<Self> {
        for attempt in 0..1_000_u16 {
            let path = package_dir.join(format!(
                ".seekdeep-built-invariant-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("could not allocate staged invariant directory")
    }
}

impl Drop for StagedPackage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn package_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    for group in std::fs::read_dir(root.join("packages"))? {
        let group = group?;
        if !group.file_type()?.is_dir() {
            continue;
        }
        for package in std::fs::read_dir(group.path())? {
            let package = package?;
            let manifest = package.path().join("package.json");
            if package.file_type()?.is_dir() && manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    Ok(manifests)
}

fn node_executable() -> std::ffi::OsString {
    std::env::var_os("npm_node_execpath").unwrap_or_else(|| "node".into())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
