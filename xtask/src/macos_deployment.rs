//! Reject runtime executables that require newer macOS than their wheel tag.

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    process::Command,
};

/// Dot-separated numeric deployment version.
#[derive(Clone, Debug)]
pub struct DeploymentVersion(Vec<u64>);

impl DeploymentVersion {
    /// Parses a non-empty dot-separated numeric version.
    ///
    /// # Errors
    ///
    /// Rejects empty, non-numeric, or overflowing components.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !value.is_empty()
                && value.split('.').all(|component| !component.is_empty()
                    && component.bytes().all(|byte| byte.is_ascii_digit())),
            "invalid macOS deployment version: {value:?}"
        );
        let parts = value
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| anyhow::anyhow!("invalid macOS deployment version: {value:?}"))?;
        Ok(Self(parts))
    }

    /// Canonical dot-separated rendering.
    #[must_use]
    pub fn render(&self) -> String {
        self.0
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl Ord for DeploymentVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let width = self.0.len().max(other.0.len());
        (0..width)
            .map(|index| {
                self.0
                    .get(index)
                    .copied()
                    .unwrap_or_default()
                    .cmp(&other.0.get(index).copied().unwrap_or_default())
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialEq for DeploymentVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for DeploymentVersion {}

impl PartialOrd for DeploymentVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Returns the minimum macOS version encoded by one arm64 wheel platform tag.
///
/// # Errors
///
/// Rejects every non-canonical tag.
pub fn claimed_version(platform_tag: &str) -> anyhow::Result<DeploymentVersion> {
    let body = platform_tag
        .strip_prefix("macosx_")
        .and_then(|value| value.strip_suffix("_arm64"))
        .ok_or_else(|| anyhow::anyhow!("unsupported macOS wheel platform tag: {platform_tag:?}"))?;
    let parts = body.split('_').collect::<Vec<_>>();
    anyhow::ensure!(
        parts.len() == 2
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
        "unsupported macOS wheel platform tag: {platform_tag:?}"
    );
    DeploymentVersion::parse(&parts.join("."))
}

/// Returns the newest deployment target from one or more Mach-O slices.
///
/// # Errors
///
/// Rejects output without an `LC_BUILD_VERSION` `minos` line or with a malformed value.
pub fn parse_otool_deployment_target(output: &str) -> anyhow::Result<DeploymentVersion> {
    let mut versions = Vec::new();
    for line in output.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || fields.first().copied() != Some("minos") {
            continue;
        }
        versions.push(DeploymentVersion::parse(fields[1])?);
    }
    versions.into_iter().max().ok_or_else(|| {
        anyhow::anyhow!("otool output contains no LC_BUILD_VERSION deployment target")
    })
}

/// Rejects an executable target newer than its wheel claim.
///
/// # Errors
///
/// Returns the exact incompatible-target diagnostic.
pub fn ensure_compatible(
    executable: &Path,
    actual: &DeploymentVersion,
    platform_tag: &str,
) -> anyhow::Result<()> {
    let claimed = claimed_version(platform_tag)?;
    anyhow::ensure!(
        actual <= &claimed,
        "{} requires macOS {} but the wheel claims {platform_tag}",
        executable.display(),
        actual.render()
    );
    Ok(())
}

/// Reads one Mach-O executable's deployment target with `otool`.
///
/// # Errors
///
/// Returns missing-file, process, exit-status, UTF-8, or parser failures.
pub fn deployment_target(executable: &Path) -> anyhow::Result<DeploymentVersion> {
    anyhow::ensure!(
        executable.is_file(),
        "runtime executable does not exist: {}",
        executable.display()
    );
    let output = Command::new("otool")
        .args(["-l"])
        .arg(executable)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "otool failed for {} with {}: {}",
        executable.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    parse_otool_deployment_target(std::str::from_utf8(&output.stdout)?)
        .map_err(|error| anyhow::anyhow!("{}: {error}", executable.display()))
}

/// Validates every executable and returns the measured targets in input order.
///
/// # Errors
///
/// Returns the first measurement or compatibility failure.
pub fn validate_deployment_targets(
    executables: &[PathBuf],
    platform_tag: &str,
) -> anyhow::Result<Vec<(PathBuf, DeploymentVersion)>> {
    let measured = executables
        .iter()
        .map(|path| deployment_target(path).map(|version| (path.clone(), version)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for (path, version) in &measured {
        ensure_compatible(path, version, platform_tag)?;
    }
    Ok(measured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otool_parser_uses_the_newest_macho_slice() {
        let output = "\n  cmd LC_BUILD_VERSION\nminos 11.0\n  cmd LC_BUILD_VERSION\nminos 13.5\n";
        assert_eq!(
            parse_otool_deployment_target(output).unwrap(),
            DeploymentVersion::parse("13.5").unwrap()
        );
    }

    #[test]
    fn otool_parser_requires_a_deployment_target() {
        assert_eq!(
            parse_otool_deployment_target("Load command 0\n")
                .unwrap_err()
                .to_string(),
            "otool output contains no LC_BUILD_VERSION deployment target"
        );
    }

    #[test]
    fn wheel_tag_rejects_a_newer_executable_target() {
        let runtime = Path::new("runtime");
        ensure_compatible(
            runtime,
            &DeploymentVersion::parse("13.5").unwrap(),
            "macosx_14_0_arm64",
        )
        .unwrap();
        let error = ensure_compatible(
            Path::new("spawn-helper"),
            &DeploymentVersion::parse("14.1").unwrap(),
            "macosx_14_0_arm64",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "spawn-helper requires macOS 14.1 but the wheel claims macosx_14_0_arm64"
        );
    }

    #[test]
    fn numeric_versions_pad_compare_and_malformed_values_fail() {
        assert_eq!(
            DeploymentVersion::parse("14").unwrap(),
            DeploymentVersion::parse("14.0.0").unwrap()
        );
        for value in ["", "14.", ".14", "14.x", "14-1"] {
            assert!(DeploymentVersion::parse(value).is_err(), "{value:?}");
        }
        for tag in ["macosx_14_arm64", "macosx_14_0_x86_64", "macos_14_0_arm64"] {
            assert!(claimed_version(tag).is_err(), "{tag:?}");
        }
    }
}
