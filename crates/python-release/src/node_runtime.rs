//! Complete compiled Node closures with target-specific official executables.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use seekdeep_code_runtime_worker_thread::node_assets;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::executable::{Target, validate_native_artifact};

mod distribution;

pub use distribution::{
    AcquiredNode, CurlFetcher, DistributionFetcher, NodeDistribution, acquire_distributions,
    archive_checksum, select_distribution,
};

/// Runtime directory resolved beside native and Python-carried executables.
pub const DIRECTORY: &str = "code-runtime-node";

/// Provenance of the official executable included in a release closure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NodeProvenance {
    /// Exact v-prefixed official Node.js release version.
    pub version: String,
    /// Native platform and architecture, for example `macos-arm64`.
    pub target: String,
    /// Official distribution archive filename.
    pub archive: String,
    /// SHA256 verified before extracting the archive.
    pub archive_sha256: String,
    /// Canonical official archive URL.
    pub url: String,
    /// Relative executable path inside the compiled runtime closure.
    pub executable: String,
    /// Relative upstream license path inside the closure.
    pub license: String,
}

/// Validates a complete release closure, including its bundled Node executable.
///
/// # Errors
/// Rejects missing or changed assets, symlinks, incompatible provenance, and wrong native binaries.
pub fn verify_directory(directory: &Path, target: &Target) -> anyhow::Result<NodeProvenance> {
    node_assets::verify_manifest(directory)?;
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join(node_assets::MANIFEST))?)?;
    let node: NodeProvenance =
        serde_json::from_value(manifest.get("node").cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "release Node runtime has no bundled executable provenance: {}",
                directory.display()
            )
        })?)?;
    anyhow::ensure!(
        node.target == target.platform_arch(),
        "release Node runtime target {} does not match {}",
        node.target,
        target.platform_arch()
    );
    anyhow::ensure!(
        node.executable == "bin/node" && node.license == "bin/LICENSE",
        "release Node runtime has unsupported executable or license paths"
    );
    let expected = NodeDistribution::for_version(&node.version, target)?;
    anyhow::ensure!(
        node.version == expected.version
            && node.archive == expected.archive
            && node.url == expected.url(),
        "release Node runtime provenance does not identify its official archive"
    );
    anyhow::ensure!(
        valid_sha256(&node.archive_sha256),
        "release Node runtime archive digest is invalid"
    );
    validate_native_artifact(&directory.join(&node.executable), target)?;
    anyhow::ensure!(
        !fs::read(directory.join(&node.license))?.is_empty(),
        "release Node runtime license is empty"
    );
    Ok(node)
}

/// Copies a verified release closure without dropping dependency or snippet subdirectories.
///
/// # Errors
/// Rejects an invalid closure, an existing destination, symlinks, and copy failures.
pub fn copy_directory(source: &Path, destination: &Path, target: &Target) -> anyhow::Result<()> {
    verify_directory(source, target)?;
    copy_tree(source, destination)?;
    verify_directory(destination, target)?;
    Ok(())
}

/// Finds the matching shipped closure beside a runtime executable.
///
/// # Errors
/// Fails when neither a target-qualified nor a flat adjacent release closure is valid.
pub fn adjacent_directory(executable: &Path, target: &Target) -> anyhow::Result<PathBuf> {
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime executable has no parent"))?;
    for candidate in [
        parent.join(DIRECTORY).join(target.platform_arch()),
        parent.join(DIRECTORY),
    ] {
        if candidate.join(node_assets::MANIFEST).exists() {
            verify_directory(&candidate, target)?;
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "compiled Node runtime is missing beside {}: expected {DIRECTORY}/{} or a matching flat {DIRECTORY} closure",
        executable.display(),
        target.platform_arch()
    );
}

/// Combines the compiled worker closure with an already verified official distribution.
///
/// # Errors
/// Rejects linked or modified inputs, existing destinations, and an invalid final closure.
pub fn stage_distribution(
    compiled: &Path,
    distribution: &NodeDistribution,
    extracted: &Path,
    archive_sha256: &str,
    destination: &Path,
) -> anyhow::Result<()> {
    node_assets::verify_manifest(compiled)?;
    anyhow::ensure!(
        valid_sha256(archive_sha256),
        "official Node archive digest is invalid"
    );
    let target = &distribution.target;
    validate_native_artifact(&extracted.join("bin/node"), target)?;
    anyhow::ensure!(
        !fs::read(extracted.join("LICENSE"))?.is_empty(),
        "official Node archive has no license"
    );
    copy_tree(compiled, destination)?;
    fs::create_dir_all(destination.join("bin"))?;
    copy_regular_file(&extracted.join("bin/node"), &destination.join("bin/node"))?;
    copy_regular_file(&extracted.join("LICENSE"), &destination.join("bin/LICENSE"))?;
    let provenance = NodeProvenance {
        version: distribution.version.clone(),
        target: target.platform_arch(),
        archive: distribution.archive.clone(),
        archive_sha256: archive_sha256.to_owned(),
        url: distribution.url(),
        executable: "bin/node".to_owned(),
        license: "bin/LICENSE".to_owned(),
    };
    node_assets::write_manifest_with_node(destination, serde_json::to_value(provenance)?)?;
    verify_directory(destination, target)?;
    Ok(())
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.to_string_lossy().contains('\\')
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        fs::symlink_metadata(source)?.is_dir(),
        "Node runtime source is not a real directory: {}",
        source.display()
    );
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            copy_regular_file(&from, &to)?;
        }
    }
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}

fn copy_regular_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        fs::symlink_metadata(source)?.is_file(),
        "Node runtime asset is not a regular file: {}",
        source.display()
    );
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut fs::File::open(source)?, &mut output)?;
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}
