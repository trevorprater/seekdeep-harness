//! Package invariant publication metadata and Rust ownership audit.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use serde_json::Value;

/// One compatibility package and its source-oracle invariant surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageInvariantOwner {
    /// Repository-relative package directory.
    pub directory: String,
    /// Manifest path.
    pub manifest_path: String,
    /// Pinned source invariant surface.
    pub source_surface: String,
    /// Target package identity.
    pub package_name: String,
}

/// One invariant ownership/publication violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageInvariantViolation {
    /// Repository-relative owner path.
    pub path: String,
    /// Diagnostic.
    pub message: String,
}

/// Discovers every depth-two compatibility package invariant owner.
///
/// # Errors
///
/// Returns traversal, manifest, JSON, or package-name failures.
pub fn package_invariant_owners(root: &Path) -> anyhow::Result<Vec<PackageInvariantOwner>> {
    let mut manifests = package_manifests(root)?;
    manifests.sort_by(|left, right| {
        relative(root, left)
            .encode_utf16()
            .cmp(relative(root, right).encode_utf16())
    });
    let mut owners = Vec::new();
    for path in manifests {
        let manifest_path = relative(root, &path);
        let manifest = serde_json::from_str::<Value>(&std::fs::read_to_string(&path)?)?;
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{manifest_path}: package invariant owner must declare a package name"
                )
            })?;
        let directory = manifest_path
            .strip_suffix("/package.json")
            .unwrap_or(&manifest_path)
            .to_owned();
        let source_directory = directory.replace(
            "packages/subagent/subagent-seekdeep-sdk",
            "packages/subagent/subagent-dsh-sdk",
        );
        owners.push(PackageInvariantOwner {
            source_surface: format!("{source_directory}/src/invariant.ts"),
            directory,
            manifest_path,
            package_name: name.to_owned(),
        });
    }
    Ok(owners)
}

/// Collects all package invariant publication and Rust ownership violations.
///
/// # Errors
///
/// Returns discovery, manifest, parity-manifest, or target-read failures.
pub fn collect_package_invariant_violations(
    root: &Path,
) -> anyhow::Result<Vec<PackageInvariantViolation>> {
    let parity =
        serde_json::from_str::<Value>(&std::fs::read_to_string(root.join("porting/parity.json"))?)?;
    let surfaces = parity
        .get("surfaces")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("porting/parity.json has no surfaces array"))?;
    let catalog = std::fs::read_to_string(root.join("crates/invariants/src/noop/catalog.rs"))?;
    let mut violations = Vec::new();
    for owner in package_invariant_owners(root)? {
        let manifest = serde_json::from_str::<Value>(&std::fs::read_to_string(
            root.join(&owner.manifest_path),
        )?)?;
        check_manifest(&owner, &manifest, &mut violations);
        check_rust_owner(root, &owner, surfaces, &catalog, &mut violations)?;
    }
    Ok(violations)
}

/// Renders the source-compatible command report.
#[must_use]
pub fn render_package_invariant_report(
    owners: usize,
    violations: &[PackageInvariantViolation],
) -> String {
    if violations.is_empty() {
        return format!(
            "verify-package-invariants: {owners} hand-owned package companion(s) conform.\n"
        );
    }
    let mut output = "verify-package-invariants: violations found:\n".to_owned();
    for violation in violations {
        let _ = writeln!(output, "  {}: {}", violation.path, violation.message);
    }
    output
}

fn check_manifest(
    owner: &PackageInvariantOwner,
    manifest: &Value,
    violations: &mut Vec<PackageInvariantViolation>,
) {
    let export = manifest
        .get("exports")
        .and_then(Value::as_object)
        .and_then(|exports| exports.get("./invariant"))
        .and_then(Value::as_object);
    if export
        .and_then(|export| export.get("types"))
        .and_then(Value::as_str)
        != Some("./lib/types/invariant.d.ts")
        || export
            .and_then(|export| export.get("default"))
            .and_then(Value::as_str)
            != Some("./lib/invariant.js")
    {
        add(
            violations,
            &owner.manifest_path,
            "exports[\"./invariant\"] must target ./lib/types/invariant.d.ts and ./lib/invariant.js",
        );
    }
    if !manifest
        .get("files")
        .and_then(Value::as_array)
        .is_some_and(|files| {
            files
                .iter()
                .any(|file| file.as_str() == Some("lib/invariant.js"))
        })
    {
        add(
            violations,
            &owner.manifest_path,
            "files must publish lib/invariant.js",
        );
    }
    if owner.package_name == "@seekdeep-ai/seekdeep-invariants" {
        return;
    }
    for (section, suffix) in [
        ("peerDependencies", "must be a workspace:^ peerDependency"),
        (
            "devDependencies",
            "must also be a workspace:^ devDependency",
        ),
    ] {
        if manifest
            .get(section)
            .and_then(Value::as_object)
            .and_then(|deps| deps.get("@seekdeep-ai/seekdeep-invariants"))
            .and_then(Value::as_str)
            != Some("workspace:^")
        {
            add(
                violations,
                &owner.manifest_path,
                &format!("@seekdeep-ai/seekdeep-invariants {suffix}"),
            );
        }
    }
}

fn check_rust_owner(
    root: &Path,
    owner: &PackageInvariantOwner,
    surfaces: &[Value],
    catalog: &str,
    violations: &mut Vec<PackageInvariantViolation>,
) -> anyhow::Result<()> {
    let Some(surface) = surfaces.iter().find(|surface| {
        surface.get("source").and_then(Value::as_str) == Some(&owner.source_surface)
    }) else {
        add(
            violations,
            &owner.source_surface,
            "missing parity row for package-owned invariant companion",
        );
        return Ok(());
    };
    if surface.get("status").and_then(Value::as_str) != Some("verified") {
        add(
            violations,
            &owner.source_surface,
            "package-owned invariant parity row is not verified",
        );
        return Ok(());
    }
    let targets = surface
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if targets.is_empty() {
        add(
            violations,
            &owner.source_surface,
            "verified invariant parity row names no Rust target",
        );
        return Ok(());
    }
    let mut existing = false;
    for target in &targets {
        let path = root.join(target);
        if !path.exists() {
            continue;
        }
        existing = true;
        if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs")
            && std::fs::read_to_string(&path)?.contains("@generated")
        {
            add(
                violations,
                target,
                "invariant companions must be hand-owned and may not carry @generated markers",
            );
        }
    }
    if !existing {
        add(
            violations,
            &owner.source_surface,
            "verified invariant parity row has no existing target",
        );
    }
    if targets
        .iter()
        .any(|target| target.contains("invariants/src/noop"))
        && (!catalog.contains(&format!("\"{}\"", owner.source_surface))
            || !catalog.contains(&format!("\"{}\"", owner.package_name)))
    {
        add(
            violations,
            &owner.source_surface,
            "shared no-op catalog lacks the exact source surface or package identity",
        );
    }
    Ok(())
}

fn package_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    for group in directories(&root.join("packages"))? {
        for package in directories(&group)? {
            let manifest = package.join("package.json");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    Ok(manifests)
}

fn directories(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut output = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            output.push(entry.path());
        }
    }
    Ok(output)
}

fn add(violations: &mut Vec<PackageInvariantViolation>, path: &str, message: &str) {
    violations.push(PackageInvariantViolation {
        path: path.to_owned(),
        message: message.to_owned(),
    });
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
