use std::{fs, path::Path, time::Duration};

use anyhow::{Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use sha2::{Digest as _, Sha512};

use crate::process::Runner;

use super::{command, path_string};

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

/// Counts writes and byte-identical versions skipped during an ordered publication.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    /// New versions written or confirmed after an ambiguous write result.
    pub published: usize,
    /// Existing versions whose registry integrity exactly matches the local archive.
    pub skipped: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RegistryState {
    Absent,
    Present { integrity: String },
}

/// Computes the exact integrity string recorded by the npm registry for a tarball.
///
/// # Errors
///
/// Returns when the archive cannot be read.
pub fn integrity_of(tarball: &Path) -> Result<String> {
    Ok(format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(fs::read(tarball)?))
    ))
}

fn packed_identity(
    tarball: &Path,
    cwd: &Path,
    runner: &mut dyn Runner,
) -> Result<(String, String)> {
    let output = runner.run(&command(
        "tar",
        vec![
            "-xOzf".to_owned(),
            path_string(tarball),
            "package/package.json".to_owned(),
        ],
        cwd,
        true,
    ))?;
    if output.status != Some(0) {
        let stderr = if output.unstarted {
            "null"
        } else {
            &output.stderr
        };
        bail!(
            "cannot read the manifest inside {}:\n{}",
            tarball.display(),
            stderr
        );
    }
    let manifest: Value = serde_json::from_str(&output.stdout)?;
    let (Some(name), Some(version)) = (
        manifest.get("name").and_then(Value::as_str),
        manifest.get("version").and_then(Value::as_str),
    ) else {
        bail!("{} manifest lacks name/version", tarball.display());
    };
    Ok((name.to_owned(), version.to_owned()))
}

fn registry_state(
    name: &str,
    version: &str,
    cwd: &Path,
    runner: &mut dyn Runner,
) -> Result<RegistryState> {
    let output = runner.run(&command(
        "npm",
        vec![
            "view".to_owned(),
            format!("{name}@{version}"),
            "dist.integrity".to_owned(),
            "--json".to_owned(),
        ],
        cwd,
        true,
    ))?;
    if output.status != Some(0) {
        let combined = combined_output(&output);
        if combined.contains("E404") || combined.contains("404 Not Found") {
            return Ok(RegistryState::Absent);
        }
        bail!("npm view {name}@{version} failed:\n{combined}");
    }
    let parsed: Value = serde_json::from_str(&output.stdout)?;
    let Some(integrity) = parsed.as_str().filter(|value| !value.is_empty()) else {
        bail!("registry reported no dist.integrity for {name}@{version}");
    };
    Ok(RegistryState::Present {
        integrity: integrity.to_owned(),
    })
}

fn publish_tarball(
    tarball: &Path,
    name: &str,
    version: &str,
    cwd: &Path,
    runner: &mut dyn Runner,
) -> Result<()> {
    let mut args = vec!["publish".to_owned(), path_string(tarball)];
    if version.contains('-') {
        args.extend(["--tag".to_owned(), "next".to_owned()]);
    }
    for attempt in 1..=PUBLISH_ATTEMPTS {
        let output = runner.run(&command("npm", args.clone(), cwd, true))?;
        if output.status == Some(0) {
            return Ok(());
        }
        let combined = combined_output(&output);
        if let RegistryState::Present { integrity } = registry_state(name, version, cwd, runner)?
            && integrity == integrity_of(tarball)?
        {
            runner.log(&format!(
                "landlock publish: {name}@{version} landed despite a reported failure, continuing"
            ));
            return Ok(());
        }
        let transient = TRANSIENT_CODES
            .iter()
            .any(|code| combined.contains(&format!("code {code}")));
        if attempt == PUBLISH_ATTEMPTS || !transient {
            bail!("npm publish {name}@{version} failed:\n{combined}");
        }
        let backoff = PUBLISH_SPACING_MS * (1 << (attempt - 1));
        runner.log(&format!(
            "landlock publish: {name}@{version} hit a transient registry failure (attempt {attempt} of {PUBLISH_ATTEMPTS}), retrying in {backoff}ms"
        ));
        runner.sleep(Duration::from_millis(backoff));
    }
    unreachable!("the last failed attempt returns an error")
}

fn combined_output(output: &crate::process::CommandOutput) -> String {
    if output.unstarted {
        "nullnull".to_owned()
    } else {
        format!("{}{}", output.stdout, output.stderr)
    }
}

/// Publishes package tarballs in dependency order with idempotency and bounded retry checks.
///
/// # Errors
///
/// Rejects unreadable manifests, registry read failures, version/content collisions, and failed writes.
pub fn publish_release(
    destination: &Path,
    cwd: &Path,
    runner: &mut dyn Runner,
) -> Result<PublishReport> {
    let order = fs::read_to_string(destination.join("publish-order.txt"))?;
    let mut report = PublishReport::default();
    for filename in order.split('\n').filter(|line| !line.is_empty()) {
        let tarball = destination.join(filename);
        let (name, version) = packed_identity(&tarball, cwd, runner)?;
        match registry_state(&name, &version, cwd, runner)? {
            RegistryState::Absent => {}
            RegistryState::Present { integrity } => {
                let local = integrity_of(&tarball)?;
                if integrity != local {
                    bail!(
                        "{name}@{version} is already published with different content\n  registry: {integrity}\n  packed:   {local}\nBump the version, or investigate why the build is not reproducible."
                    );
                }
                runner.log(&format!(
                    "landlock publish: {name}@{version} already published, skipping"
                ));
                report.skipped += 1;
                continue;
            }
        }
        if report.published > 0 {
            runner.sleep(Duration::from_millis(PUBLISH_SPACING_MS));
        }
        publish_tarball(&tarball, &name, &version, cwd, runner)?;
        runner.log(&format!("landlock publish: {name}@{version} published"));
        report.published += 1;
    }
    runner.log(&format!(
        "landlock publish: {} published, {} already present",
        report.published, report.skipped
    ));
    Ok(report)
}
