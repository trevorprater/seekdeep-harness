//! Integrity and dependency closure of the installed Node runtime.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// Asset manifest emitted by the runtime packager.
pub const MANIFEST: &str = "assets-manifest.json";
/// Exact native watcher packages used by the compiled loader.
pub const DEPENDENCIES: &[(&str, &str)] = &[("chokidar", "4.0.3"), ("readdirp", "4.1.2")];

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    dependencies: BTreeMap<String, String>,
    files: BTreeMap<String, AssetFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<Value>,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct AssetFile {
    bytes: u64,
    sha256: String,
}

fn dependencies() -> BTreeMap<String, String> {
    DEPENDENCIES
        .iter()
        .map(|(name, version)| ((*name).to_owned(), (*version).to_owned()))
        .collect()
}

fn files(root: &Path) -> anyhow::Result<BTreeMap<String, AssetFile>> {
    let mut files = BTreeMap::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "Node runtime assets must not contain symlinks: {}",
                path.display()
            );
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            anyhow::ensure!(
                metadata.is_file(),
                "Node runtime asset is not a regular file: {}",
                path.display()
            );
            let relative = path.strip_prefix(root)?;
            if relative == Path::new(MANIFEST) {
                continue;
            }
            let name = relative
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow::anyhow!("Node runtime asset path is not UTF-8"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?
                .join("/");
            let content = std::fs::read(&path)?;
            files.insert(
                name,
                AssetFile {
                    bytes: u64::try_from(content.len())?,
                    sha256: format!("{:x}", Sha256::digest(&content)),
                },
            );
        }
    }
    Ok(files)
}

/// Records every generated regular file and the exact watcher dependency versions.
///
/// # Errors
///
/// Rejects unreadable files, symlinks, and invalid artifact paths.
pub fn write_manifest(directory: &Path) -> anyhow::Result<()> {
    write(directory, None)
}

/// Records a standalone package including its per-target Node provenance.
///
/// # Errors
///
/// Rejects unreadable files, symlinks, and invalid artifact paths.
pub fn write_manifest_with_node(directory: &Path, node: Value) -> anyhow::Result<()> {
    write(directory, Some(node))
}

fn write(directory: &Path, node: Option<Value>) -> anyhow::Result<()> {
    let manifest = Manifest {
        schema_version: 1,
        dependencies: dependencies(),
        files: files(directory)?,
        node,
    };
    let mut encoded = serde_json::to_vec_pretty(&manifest)?;
    encoded.push(b'\n');
    std::fs::write(directory.join(MANIFEST), encoded)?;
    Ok(())
}

/// Verifies the complete installed closure against its recorded file hashes.
///
/// # Errors
///
/// Rejects missing, extra, modified, or linked files and unsupported manifests.
pub fn verify_manifest(directory: &Path) -> anyhow::Result<()> {
    let path = directory.join(MANIFEST);
    anyhow::ensure!(
        std::fs::symlink_metadata(&path)?.is_file(),
        "Node runtime manifest must be a regular file"
    );
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        manifest.schema_version == 1,
        "unsupported Node runtime asset manifest"
    );
    anyhow::ensure!(
        manifest.dependencies == dependencies(),
        "Node runtime dependency versions do not match this installation"
    );
    for required in [
        "loader.mjs",
        "plugin-loader.cjs",
        "wasm-runtime.cjs",
        "seekdeep_code_runtime_node.js",
        "seekdeep_code_runtime_node_bg.wasm",
        "package.json",
        "snippets/package.json",
        "node_modules/chokidar/package.json",
        "node_modules/readdirp/package.json",
    ] {
        anyhow::ensure!(
            manifest.files.contains_key(required),
            "Node runtime manifest is missing {required}"
        );
    }
    if let Some(node) = &manifest.node {
        for field in ["executable", "license"] {
            let path = node
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("Node provenance is missing {field}"))?;
            anyhow::ensure!(
                manifest.files.contains_key(path),
                "Node provenance names an unrecorded {field}: {path}"
            );
        }
    }
    let actual = files(directory)?;
    for (path, expected) in &manifest.files {
        anyhow::ensure!(
            actual.get(path) == Some(expected),
            "Node runtime asset is missing or modified: {path}"
        );
    }
    anyhow::ensure!(
        actual.len() == manifest.files.len(),
        "Node runtime contains unrecorded asset files"
    );
    Ok(())
}

pub(crate) fn bundled_node_required(directory: &Path) -> anyhow::Result<bool> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(directory.join(MANIFEST))?)?;
    Ok(manifest.node.is_some())
}
