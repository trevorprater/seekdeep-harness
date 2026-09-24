use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result, bail};
use regex::Regex;
use serde_json::Value;

use crate::{
    process::Runner,
    repo::{Repository, read_json, verify_platform_binaries},
};

use super::{command, manifest_string, run_checked};

const TAG_PREFIX: &str = "refs/tags/landlock-run-v";

/// Workflow values relevant to the release tag/version boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseEnvironment {
    /// Workflow ref, including the `refs/tags/` prefix when invoked from a tag.
    pub github_ref: String,
    /// Whether the workflow intends to publish the verified version.
    pub release_publish: bool,
}

impl ReleaseEnvironment {
    /// Reads only release-orchestration variables, never runtime launcher selection.
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            github_ref: std::env::var("GITHUB_REF").unwrap_or_default(),
            release_publish: std::env::var("RELEASE_PUBLISH").is_ok_and(|value| value == "true"),
        }
    }
}

fn unique_versions(packages: &[(PathBuf, Value)]) -> Result<Vec<String>> {
    let mut versions = Vec::new();
    for (_, manifest) in packages {
        let version = manifest_string(manifest, "version")?.to_owned();
        if !versions.contains(&version) {
            versions.push(version);
        }
    }
    Ok(versions)
}

fn packages(repository: &Repository) -> Result<Vec<(PathBuf, Value)>> {
    repository
        .package_dirs()?
        .into_iter()
        .map(|directory| {
            let manifest = read_json(&repository.root.join(&directory).join("package.json"))?;
            Ok((directory, manifest))
        })
        .collect()
}

/// Verifies a single published version, the workflow tag, and optional prebuild payloads.
///
/// # Errors
///
/// Rejects divergent package versions, an invalid publication ref, or invalid binaries.
pub fn verify_release(
    repository: &Repository,
    environment: &ReleaseEnvironment,
    prebuilds: bool,
    runner: &mut dyn Runner,
) -> Result<String> {
    let packages = packages(repository)?;
    let versions = unique_versions(&packages)?;
    if versions.len() != 1 {
        let mut lines = vec!["published package versions must match:".to_owned()];
        for (directory, manifest) in &packages {
            lines.push(format!(
                "{}: {}",
                directory.display(),
                manifest_string(manifest, "version")?
            ));
        }
        bail!("{}", lines.join("\n"));
    }
    let version = &versions[0];
    if environment.release_publish && !environment.github_ref.starts_with(TAG_PREFIX) {
        bail!("publishing requires running the workflow from a landlock-run-v* tag");
    }
    if let Some(tag_version) = environment.github_ref.strip_prefix(TAG_PREFIX)
        && tag_version != version
    {
        bail!("tag/version mismatch: tag landlock-run-v{tag_version}, packages {version}");
    }
    runner.log(&format!("Verified release version {version}"));
    if prebuilds {
        for directory in repository.platform_dirs()? {
            let verified = verify_platform_binaries(&repository.root.join(directory))?;
            runner.log(&format!(
                "Verified {}: {} binaries",
                verified.name, verified.count
            ));
        }
    }
    Ok(version.clone())
}

fn js_number(value: f64) -> String {
    ryu_js::Buffer::new().format(value).to_owned()
}

/// Applies the source release syntax, including explicitly supplied prerelease versions.
///
/// # Errors
///
/// Rejects unsupported bump names and incremental bumps from a non-plain current version.
pub fn next_version(current: &str, release: &str) -> Result<String> {
    let explicit = Regex::new(r"^\d+\.\d+\.\d+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$")?;
    if release.is_ascii() && explicit.is_match(release) {
        return Ok(release.to_owned());
    }
    if !matches!(release, "major" | "minor" | "patch") {
        bail!("Usage: pnpm release:bump <major|minor|patch|x.y.z>");
    }
    let plain = Regex::new(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")?;
    let Some(parts) = plain.captures(current) else {
        bail!(
            "increment types need a plain x.y.z current version (current: {current}) — pass an explicit target version instead"
        );
    };
    let mut values = [0.0; 3];
    for (index, value) in values.iter_mut().enumerate() {
        *value = parts[index + 1].parse::<f64>()?;
    }
    match release {
        "major" => Ok(format!("{}.0.0", js_number(values[0] + 1.0))),
        "minor" => Ok(format!(
            "{}.{}.0",
            js_number(values[0]),
            js_number(values[1] + 1.0)
        )),
        "patch" => Ok(format!(
            "{}.{}.{}",
            js_number(values[0]),
            js_number(values[1]),
            js_number(values[2] + 1.0)
        )),
        _ => unreachable!("release syntax was checked above"),
    }
}

/// Bumps root and published manifests, refreshes the lockfile, and verifies their version.
///
/// # Errors
///
/// Propagates validation, write, package-manager, and release-verification failures.
pub fn bump_release(
    repository: &Repository,
    bump: &str,
    environment: &ReleaseEnvironment,
    runner: &mut dyn Runner,
) -> Result<String> {
    if bump.is_empty() {
        bail!("Usage: pnpm release:bump <major|minor|patch|x.y.z>");
    }
    let packages = packages(repository)?;
    let versions = unique_versions(&packages)?;
    if versions.len() != 1 {
        bail!("published package versions differ: {}", versions.join(", "));
    }
    let target = next_version(&versions[0], bump)?;
    let files = std::iter::once(PathBuf::from("package.json")).chain(
        packages
            .into_iter()
            .map(|(directory, _)| directory.join("package.json")),
    );
    for file in files {
        let full = repository.root.join(&file);
        let mut manifest = read_json(&full)?;
        manifest["version"] = Value::String(target.clone());
        fs::write(
            full,
            format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        )?;
        runner.log(&format!("{}: {target}", file.display()));
    }
    let repository_root = repository
        .root
        .parent()
        .and_then(std::path::Path::parent)
        .context("launcher workspace has no containing repository")?;
    let mut install = command(
        "pnpm",
        vec![
            "install".to_owned(),
            "--ignore-scripts".to_owned(),
            "--lockfile-only".to_owned(),
        ],
        repository_root,
        false,
    );
    install.env.insert("CI".to_owned(), "true".to_owned());
    run_checked(runner, &install)?;
    verify_release(repository, environment, false, runner)?;
    runner.log(&format!("Release version bumped to {target}"));
    Ok(target)
}

/// Bumps and commits the package family; tag creation remains an explicit separate action.
///
/// # Errors
///
/// Propagates the bump and Git operation failures, preserving child exit statuses.
pub fn commit_release(
    repository: &Repository,
    bump: &str,
    environment: &ReleaseEnvironment,
    runner: &mut dyn Runner,
) -> Result<String> {
    if bump.is_empty() {
        bail!("Usage: pnpm release:commit <major|minor|patch|x.y.z>");
    }
    let version = bump_release(repository, bump, environment, runner)?;
    for args in [
        vec![
            "add".to_owned(),
            "package.json".to_owned(),
            "packages/*/package.json".to_owned(),
            "../../pnpm-lock.yaml".to_owned(),
        ],
        vec![
            "commit".to_owned(),
            "-m".to_owned(),
            format!("release(landlock-run): {version}"),
        ],
    ] {
        let mut git = command("git", args, &repository.root, false);
        git.env.insert("CI".to_owned(), "true".to_owned());
        run_checked(runner, &git)?;
    }
    runner.log(&format!(
        "Committed release {version}. Create the tag manually: git tag landlock-run-v{version}"
    ));
    Ok(version)
}
