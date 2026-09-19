use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde_json::Value;

use crate::{
    process::Runner,
    repo::{Repository, read_json},
};

use super::{
    capture_checked, command, manifest_string, path_string, relative_path, remove_if_present,
    run_checked,
};

/// Selects the destination and platform coverage of a release pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackOptions {
    /// Output directory replaced by this pack operation.
    pub destination: PathBuf,
    /// Whether to omit platforms that cannot run on this host.
    pub current_platform_only: bool,
    /// Node-style platform and architecture name used to select the native payload.
    pub host_platform: String,
}

/// Returns the package-manager tarball name for a manifest.
///
/// # Errors
///
/// Returns when the manifest does not declare a package name and version.
pub fn tarball_name(manifest: &Value) -> Result<String> {
    let name = manifest_string(manifest, "name")?;
    let version = manifest_string(manifest, "version")?;
    let name = name
        .strip_prefix('@')
        .map_or_else(|| name.to_owned(), |name| name.replacen('/', "-", 1));
    Ok(format!("{name}-{version}.tgz"))
}

/// Packs executable platform payloads with npm and workspace-dependent entry payloads with pnpm.
///
/// # Errors
///
/// Returns on discovery, prepack, child process, missing output, or filesystem failure.
pub fn pack_release(
    repository: &Repository,
    options: &PackOptions,
    runner: &mut dyn Runner,
) -> Result<Vec<String>> {
    remove_if_present(&options.destination)?;
    fs::create_dir_all(&options.destination)?;
    let platforms = repository.platform_dirs()?;
    let platform_set = platforms.iter().cloned().collect::<BTreeSet<_>>();
    let mut directories = Vec::new();
    for directory in platforms {
        if !options.current_platform_only
            || read_json(&repository.root.join(&directory).join("prebuilds.json"))?["platform"]
                .as_str()
                == Some(options.host_platform.as_str())
        {
            directories.push(directory);
        }
    }
    directories.extend(repository.entry_dirs()?);
    let mut order = Vec::new();
    for directory in directories {
        let manifest = read_json(&repository.root.join(&directory).join("package.json"))?;
        let spec = if platform_set.contains(&directory) {
            command(
                "npm",
                vec![
                    "pack".to_owned(),
                    format!("./{}", directory.display()),
                    "--pack-destination".to_owned(),
                    path_string(&options.destination),
                ],
                &repository.root,
                false,
            )
        } else {
            command(
                "pnpm",
                vec![
                    "--dir".to_owned(),
                    path_string(&directory),
                    "pack".to_owned(),
                    "--pack-destination".to_owned(),
                    path_string(&options.destination),
                ],
                &repository.root,
                false,
            )
        };
        run_checked(runner, &spec)?;
        let tarball = tarball_name(&manifest)?;
        let output = options.destination.join(&tarball);
        if !output.exists() {
            bail!("expected pack output not found: {}", output.display());
        }
        order.push(tarball);
    }
    fs::write(
        options.destination.join("publish-order.txt"),
        format!("{}\n", order.join("\n")),
    )?;
    runner.log(&format!(
        "Packed {} packages into {}",
        order.len(),
        relative_path(&repository.root, &options.destination)
    ));
    Ok(order)
}

/// Reads a package manifest directly from its packed bytes.
///
/// # Errors
///
/// Propagates missing archive, tar extraction, and JSON decoding failures.
pub fn read_packed_manifest(tarball: &Path, cwd: &Path, runner: &mut dyn Runner) -> Result<Value> {
    let mut spec = command(
        "tar",
        vec![
            "-xOf".to_owned(),
            path_string(tarball),
            "package/package.json".to_owned(),
        ],
        cwd,
        true,
    );
    spec.max_buffer = 64 * 1024 * 1024;
    let output = capture_checked(runner, &spec)?;
    Ok(serde_json::from_str(&output.stdout)?)
}
