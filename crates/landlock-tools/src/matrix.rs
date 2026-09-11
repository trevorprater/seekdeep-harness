//! GitHub Actions matrices derived from the platform package metadata.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::repo::{Prebuilds, Repository, read_json};

/// Supported workflow matrix shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixKind {
    /// One native runner per distinct platform.
    Ci,
    /// One native artifact-producing job per platform package.
    ReleasePrebuild,
}

/// Create a CI or release-prebuild matrix in source publication order.
///
/// # Errors
///
/// Returns when metadata is invalid or a declared platform has no native runner.
pub fn github_matrix(repo: &Repository, kind: MatrixKind) -> Result<Value> {
    let manifests = repo
        .platform_dirs()?
        .into_iter()
        .map(|directory| {
            let prebuilds: Prebuilds = serde_json::from_value(read_json(
                &repo.package_path(&directory).join("prebuilds.json"),
            )?)?;
            Ok((directory, prebuilds))
        })
        .collect::<Result<Vec<_>>>()?;
    let include = match kind {
        MatrixKind::Ci => {
            let mut platforms = manifests
                .iter()
                .map(|(_, prebuilds)| &prebuilds.platform)
                .collect::<Vec<_>>();
            platforms.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            platforms.dedup();
            platforms
                .into_iter()
                .map(|platform| {
                    Ok(json!({ "platform": platform, "runner": runner_for(platform)? }))
                })
                .collect::<Result<Vec<_>>>()?
        }
        MatrixKind::ReleasePrebuild => manifests
            .iter()
            .map(|(directory, prebuilds)| {
                let name = directory.file_name().unwrap_or_default().to_string_lossy();
                Ok(json!({
                    "platform": prebuilds.platform,
                    "package": name,
                    "dir": directory.to_string_lossy().replace('\\', "/"),
                    "runner": runner_for(&prebuilds.platform)?,
                    "artifact": format!("prebuild-{name}"),
                }))
            })
            .collect::<Result<Vec<_>>>()?,
    };
    Ok(json!({ "include": include }))
}

fn runner_for(platform: &str) -> Result<&'static str> {
    match platform {
        "linux-x64" => Ok("ubuntu-24.04"),
        "linux-arm64" => Ok("ubuntu-24.04-arm"),
        _ => bail!("missing GitHub runner for platform: {platform}"),
    }
}
