use std::{fs, path::Path};

use anyhow::{Result, bail};

use crate::{
    process::Runner,
    repo::{Repository, verify_platform_binaries},
};

use super::{relative_path, remove_if_present};

/// Copies release artifacts into their declared platform packages and checks every binary.
///
/// # Errors
///
/// Rejects absent or unrecognized artifacts, failed copies, and invalid platform payloads.
pub fn assemble_prebuilds(
    repository: &Repository,
    artifact_root: &Path,
    runner: &mut dyn Runner,
) -> Result<()> {
    if !artifact_root.exists() {
        bail!(
            "prebuild artifact directory does not exist: {}",
            artifact_root.display()
        );
    }
    let platforms = repository.platform_dirs()?;
    for directory in &platforms {
        let bin = repository.root.join(directory).join("bin");
        remove_if_present(&bin)?;
        fs::create_dir_all(bin)?;
    }

    let mut artifacts = fs::read_dir(artifact_root)?.collect::<std::io::Result<Vec<_>>>()?;
    artifacts.sort_by_key(fs::DirEntry::file_name);
    for artifact in artifacts {
        let source_root = artifact.path();
        if !source_root.metadata()?.is_dir() {
            continue;
        }
        let artifact_name = artifact.file_name().to_string_lossy().into_owned();
        let Some(platform) = platforms.iter().find(|directory| {
            directory
                .file_name()
                .is_some_and(|name| artifact_name == format!("prebuild-{}", name.to_string_lossy()))
        }) else {
            bail!("cannot map artifact to a platform package: {artifact_name}");
        };
        let mut files = fs::read_dir(&source_root)?.collect::<std::io::Result<Vec<_>>>()?;
        files.sort_by_key(fs::DirEntry::file_name);
        for file in files {
            let source = file.path();
            let destination = repository
                .root
                .join(platform)
                .join("bin")
                .join(file.file_name());
            fs::copy(&source, &destination)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
            }
            runner.log(&format!(
                "Copied {} -> {}",
                relative_path(&repository.root, &source),
                relative_path(&repository.root, &destination)
            ));
        }
    }
    for directory in repository.platform_dirs()? {
        let verified = verify_platform_binaries(&repository.root.join(directory))?;
        runner.log(&format!(
            "Verified {}: {} binaries",
            verified.name, verified.count
        ));
    }
    Ok(())
}
