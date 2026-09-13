//! Integrity-safe, retryable publication of already packed release tarballs.

use std::path::Path;

use base64::Engine as _;
use sha2::{Digest as _, Sha512};

use crate::{
    release_families::ReleaseFamily,
    release_process::{ReleaseCommandResult, ReleaseRunOptions, attempt},
    release_tarball::{packed_identity, read_publish_order},
};

const TRANSIENT_CODES: &[&str] = &[
    "E409",
    "E429",
    "E500",
    "E502",
    "E503",
    "E504",
    "ETIMEDOUT",
    "ECONNRESET",
    "EAI_AGAIN",
];
const PUBLISH_ATTEMPTS: usize = 4;
const PUBLISH_SPACING_MS: u64 = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RegistryState {
    Absent,
    Present(String),
}

/// Completed publish-decision summary and ordered log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleasePublishResult {
    /// Tarballs actually published.
    pub published: usize,
    /// Identical versions already present.
    pub skipped: usize,
    /// Source-compatible progress lines.
    pub log: Vec<String>,
}

/// Runs publication with real npm commands and blocking retry delays.
///
/// # Errors
///
/// Returns order, tarball, registry, integrity, publish, or retry failures.
pub fn publish_release(
    family: ReleaseFamily,
    directory: &Path,
) -> anyhow::Result<ReleasePublishResult> {
    publish_release_with(
        family,
        directory,
        |command, args| attempt(command, args, &ReleaseRunOptions::default()),
        std::thread::sleep,
    )
}

/// Runs publication through injected npm and delay boundaries.
///
/// # Errors
///
/// Returns order, tarball, registry, integrity, publish, or retry failures.
pub fn publish_release_with(
    family: ReleaseFamily,
    directory: &Path,
    mut execute: impl FnMut(&str, &[String]) -> anyhow::Result<ReleaseCommandResult>,
    mut sleep: impl FnMut(std::time::Duration),
) -> anyhow::Result<ReleasePublishResult> {
    let mut result = ReleasePublishResult::default();
    for filename in read_publish_order(directory)? {
        let tarball = directory.join(filename);
        let identity = packed_identity(&tarball)?;
        match registry_state(&identity.name, &identity.version, &mut execute)? {
            RegistryState::Present(registry) => {
                let local = integrity_of(&tarball)?;
                if registry != local {
                    anyhow::bail!(
                        "{}@{} is already published with different content\n  registry: {registry}\n  packed:   {local}\nBump the version, or investigate why the build is not reproducible.",
                        identity.name,
                        identity.version
                    );
                }
                result.log.push(format!(
                    "release publish: {}@{} already published, skipping",
                    identity.name, identity.version
                ));
                result.skipped += 1;
            }
            RegistryState::Absent => {
                if result.published > 0 {
                    sleep(std::time::Duration::from_millis(PUBLISH_SPACING_MS));
                }
                publish_tarball(
                    &tarball,
                    &identity.name,
                    &identity.version,
                    &mut execute,
                    &mut sleep,
                    &mut result.log,
                )?;
                result.log.push(format!(
                    "release publish: {}@{} published",
                    identity.name, identity.version
                ));
                result.published += 1;
            }
        }
    }
    result.log.push(format!(
        "release publish: family {}, {} published, {} already present",
        family.identifier(),
        result.published,
        result.skipped
    ));
    Ok(result)
}

/// Renders publish progress and summary.
#[must_use]
pub fn render_release_publish_result(result: &ReleasePublishResult) -> String {
    format!("{}\n", result.log.join("\n"))
}

fn publish_tarball(
    tarball: &Path,
    name: &str,
    version: &str,
    execute: &mut impl FnMut(&str, &[String]) -> anyhow::Result<ReleaseCommandResult>,
    sleep: &mut impl FnMut(std::time::Duration),
    log: &mut Vec<String>,
) -> anyhow::Result<()> {
    let tag = version
        .contains('-')
        .then(|| ["--tag".to_owned(), "next".to_owned()]);
    for attempt_number in 1..=PUBLISH_ATTEMPTS {
        let mut args = vec!["publish".to_owned(), tarball.to_string_lossy().into_owned()];
        if let Some(tag) = &tag {
            args.extend(tag.iter().cloned());
        }
        let publish = execute("npm", &args)?;
        let output = format!("{}{}", publish.stdout, publish.stderr);
        if publish.status == Some(0) {
            return Ok(());
        }
        if let RegistryState::Present(registry) = registry_state(name, version, execute)?
            && registry == integrity_of(tarball)?
        {
            log.push(format!(
                "release publish: {name}@{version} landed despite a reported failure, continuing"
            ));
            return Ok(());
        }
        if attempt_number == PUBLISH_ATTEMPTS || !transient_failure(&output) {
            anyhow::bail!("npm publish {name}@{version} failed:\n{output}");
        }
        let backoff = PUBLISH_SPACING_MS * 2_u64.pow(u32::try_from(attempt_number - 1)?);
        log.push(format!(
            "release publish: {name}@{version} hit a transient registry failure (attempt {attempt_number} of {PUBLISH_ATTEMPTS}), retrying in {backoff}ms"
        ));
        sleep(std::time::Duration::from_millis(backoff));
    }
    Ok(())
}

fn registry_state(
    name: &str,
    version: &str,
    execute: &mut impl FnMut(&str, &[String]) -> anyhow::Result<ReleaseCommandResult>,
) -> anyhow::Result<RegistryState> {
    let result = execute(
        "npm",
        &[
            "view".to_owned(),
            format!("{name}@{version}"),
            "dist.integrity".to_owned(),
            "--json".to_owned(),
        ],
    )?;
    if result.status != Some(0) {
        let output = format!("{}{}", result.stdout, result.stderr);
        if output.contains("E404") || output.contains("404 Not Found") {
            return Ok(RegistryState::Absent);
        }
        anyhow::bail!("npm view {name}@{version} failed:\n{output}");
    }
    let integrity = serde_json::from_str::<serde_json::Value>(&result.stdout)?;
    let Some(integrity) = integrity.as_str().filter(|value| !value.is_empty()) else {
        anyhow::bail!("registry reported no dist.integrity for {name}@{version}");
    };
    Ok(RegistryState::Present(integrity.to_owned()))
}

fn integrity_of(tarball: &Path) -> anyhow::Result<String> {
    let mut hash = Sha512::new();
    hash.update(std::fs::read(tarball)?);
    Ok(format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(hash.finalize())
    ))
}

fn transient_failure(output: &str) -> bool {
    TRANSIENT_CODES
        .iter()
        .any(|code| output.contains(&format!("code {code}")))
}
