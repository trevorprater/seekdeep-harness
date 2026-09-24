//! Packed npm tarball identity, member, and publish-order readers.

use std::path::Path;

use serde_json::Value;

use crate::release_process::{ReleaseRunOptions, capture};

/// Name of the file recording a packed family's upload order.
pub const PUBLISH_ORDER_FILE: &str = "publish-order.txt";

/// Name and version declared by one packed npm manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedIdentity {
    /// Package name from the packed manifest.
    pub name: String,
    /// Package version from the packed manifest.
    pub version: String,
}

/// Lists every path inside a gzipped npm tarball.
///
/// # Errors
///
/// Returns `tar` process failures.
pub fn tarball_files(tarball: &Path) -> anyhow::Result<Vec<String>> {
    let output = capture(
        "tar",
        &["-tzf".to_owned(), tarball.to_string_lossy().into_owned()],
        &ReleaseRunOptions::default(),
    )?;
    Ok(output
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Reads a packed tarball's own `package/package.json` identity.
///
/// # Errors
///
/// Returns `tar`, JSON, manifest-shape, or identity-field failures.
pub fn packed_identity(tarball: &Path) -> anyhow::Result<PackedIdentity> {
    let tarball_text = tarball.to_string_lossy();
    let output = capture(
        "tar",
        &[
            "-xOzf".to_owned(),
            tarball_text.clone().into_owned(),
            "package/package.json".to_owned(),
        ],
        &ReleaseRunOptions::default(),
    )?;
    let manifest = serde_json::from_str::<Value>(&output)?;
    let Some(object) = manifest.as_object() else {
        anyhow::bail!("{tarball_text} has no manifest");
    };
    let name = object.get("name").and_then(Value::as_str);
    let version = object.get("version").and_then(Value::as_str);
    let (Some(name), Some(version)) = (name, version) else {
        anyhow::bail!("{tarball_text} manifest lacks name/version");
    };
    Ok(PackedIdentity {
        name: name.to_owned(),
        version: version.to_owned(),
    })
}

/// Reads tarball filenames in the packed directory's upload order.
///
/// # Errors
///
/// Returns order-file read failures.
pub fn read_publish_order(directory: &Path) -> anyhow::Result<Vec<String>> {
    let bytes = std::fs::read(directory.join(PUBLISH_ORDER_FILE))?;
    let source = String::from_utf8_lossy(&bytes);
    Ok(source
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}
