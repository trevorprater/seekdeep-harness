//! Credential-free release-family tarball packing boundary.

use std::path::{Path, PathBuf};

use crate::{
    release_families::{ReleaseFamily, ReleaseMember, tarball_name},
    release_process::{ReleaseRunOptions, run},
    release_tarball::{PUBLISH_ORDER_FILE, tarball_files},
};

/// Completed release pack output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasePackResult {
    /// Family packed.
    pub family: ReleaseFamily,
    /// Absolute destination.
    pub destination: PathBuf,
    /// Tarball filenames in publication order.
    pub order: Vec<String>,
}

/// Packs a family with the real `pnpm pack` command.
///
/// # Errors
///
/// Returns discovery, version, process, tarball, payload, destination, or
/// order-file failures.
pub fn pack_release(
    root: &Path,
    family: ReleaseFamily,
    destination: &Path,
) -> anyhow::Result<ReleasePackResult> {
    pack_release_with(root, family, destination, |member, output| {
        run(
            "pnpm",
            &[
                "--dir".to_owned(),
                member.directory.clone(),
                "pack".to_owned(),
                "--pack-destination".to_owned(),
                output.to_string_lossy().into_owned(),
            ],
            &ReleaseRunOptions {
                cwd: Some(root.to_owned()),
                env: None,
            },
        )
    })
}

/// Packs a family through an injected pack runner, preserving all validation.
///
/// # Errors
///
/// Returns discovery, version, runner, tarball, payload, destination, or
/// order-file failures.
pub fn pack_release_with(
    root: &Path,
    family: ReleaseFamily,
    destination: &Path,
    mut runner: impl FnMut(&ReleaseMember, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<ReleasePackResult> {
    let members = family.members(root)?;
    let members = family.publish_order(&members)?;
    family.verify_versions(&members)?;
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    std::fs::create_dir_all(destination)?;
    let mut order = Vec::new();
    for member in &members {
        runner(member, destination)?;
        let filename = tarball_name(member);
        let tarball = destination.join(&filename);
        if !tarball.exists() {
            anyhow::bail!(
                "{} produced no tarball at {}",
                member.name,
                tarball.display()
            );
        }
        family.validate_payload(member, &tarball_files(&tarball)?)?;
        order.push(filename);
    }
    std::fs::write(
        destination.join(PUBLISH_ORDER_FILE),
        format!("{}\n", order.join("\n")),
    )?;
    Ok(ReleasePackResult {
        family,
        destination: destination.to_owned(),
        order,
    })
}

/// Renders the source-compatible completion summary.
#[must_use]
pub fn render_release_pack_result(result: &ReleasePackResult, display_output: &str) -> String {
    format!(
        "release pack: family {}, {} tarball(s) in {display_output}\n",
        result.family.identifier(),
        result.order.len()
    )
}
