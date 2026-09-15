use std::{collections::BTreeMap, ffi::OsString, path::Path};

use super::{
    BaselineRunner, RELEASE_ENTRY_PACKAGE, ReleaseBundle, command_failure, npm_client_environment,
    strings,
};
use crate::release_process::ReleaseRunOptions;

/// Publication bound to an already verified bundle, registry, and isolated npm cwd.
pub struct RegistryPublication<'a> {
    bundle: &'a ReleaseBundle,
    options: ReleaseRunOptions,
}

impl<'a> RegistryPublication<'a> {
    /// Captures the parent environment while clearing inherited pnpm user-agent overrides.
    #[must_use]
    pub fn new(
        bundle: &'a ReleaseBundle,
        temporary_directory: &Path,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Self {
        Self {
            bundle,
            options: ReleaseRunOptions {
                cwd: Some(temporary_directory.to_owned()),
                env: Some(npm_client_environment(environment)),
            },
        }
    }

    /// Publishes absent packages, refuses conflicting bytes, repairs tags, and verifies all results.
    ///
    /// # Errors
    /// Returns authentication, confirmation, registry, immutable-version, publication, or tag failures.
    pub fn publish(
        &self,
        runner: &mut impl BaselineRunner,
        assume_yes: bool,
    ) -> anyhow::Result<()> {
        self.ping(runner)?;
        let identity = runner.capture(
            "npm",
            &strings(&["whoami", &self.registry_argument()]),
            &self.options,
        )?;
        runner.log(&format!(
            "publish-npm-baseline: registry identity {identity} at {}",
            self.bundle.manifest.registry
        ));
        if !assume_yes {
            runner.confirm(
                &format!("Publish {} packages as {} to {}? Press Enter to continue or type anything to cancel: ", self.bundle.manifest.packages.len(), self.bundle.manifest.version, self.bundle.manifest.registry),
                "publish requires an interactive terminal or --yes",
                "publication cancelled",
            )?;
        }
        for package in &self.bundle.manifest.packages {
            if let Some(existing) = self.remote_integrity(package.name.as_str(), runner)? {
                if existing != package.integrity {
                    anyhow::bail!(
                        "{}@{} already exists with different integrity",
                        package.name,
                        self.bundle.manifest.version
                    );
                }
                runner.log(&format!(
                    "publish-npm-baseline: already published {}@{}",
                    package.name, self.bundle.manifest.version
                ));
            } else {
                runner.run(
                    "npm",
                    &strings(&[
                        "publish",
                        &self.bundle.tarball_path(package).to_string_lossy(),
                        &self.registry_argument(),
                        &format!("--tag={}", self.bundle.manifest.dist_tag),
                    ]),
                    &self.options,
                )?;
            }
            self.ensure_dist_tag(
                package.name.as_str(),
                &self.bundle.manifest.dist_tag,
                runner,
            )?;
        }
        self.ensure_dist_tag(RELEASE_ENTRY_PACKAGE, "latest", runner)?;
        self.verify_remote(runner)?;
        self.verify_entry_tag(runner)
    }

    /// Queries package integrities and channel mappings without changing registry state.
    ///
    /// # Errors
    /// Returns unavailable registry, absent package, integrity, or tag mismatches.
    pub fn verify(&self, runner: &mut impl BaselineRunner) -> anyhow::Result<()> {
        self.ping(runner)?;
        self.verify_remote(runner)?;
        self.verify_entry_tag(runner)
    }

    fn registry_argument(&self) -> String {
        format!("--registry={}", self.bundle.manifest.registry)
    }

    fn ping(&self, runner: &mut impl BaselineRunner) -> anyhow::Result<()> {
        runner.capture(
            "npm",
            &strings(&["ping", &self.registry_argument()]),
            &self.options,
        )?;
        Ok(())
    }

    fn remote_integrity(
        &self,
        name: &str,
        runner: &mut impl BaselineRunner,
    ) -> anyhow::Result<Option<String>> {
        let identity = format!("{name}@{}", self.bundle.manifest.version);
        let arguments = strings(&[
            "view",
            &identity,
            "dist.integrity",
            "--json",
            &self.registry_argument(),
        ]);
        let result = runner.result("npm", &arguments, &self.options)?;
        if result.status != Some(0) {
            let output = format!("{}\n{}", result.stdout, result.stderr);
            if ["E404", "NOT_FOUND", "404 Not Found"]
                .iter()
                .any(|message| output.contains(message))
            {
                return Ok(None);
            }
            return Err(command_failure(
                "npm",
                &strings(&["view", &identity]),
                &result,
            ));
        }
        let value = if result.stdout.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str::<serde_json::Value>(&result.stdout)?
        };
        let value = value
            .as_str()
            .filter(|value| value.starts_with("sha512-"))
            .ok_or_else(|| anyhow::anyhow!("registry returned no integrity for {identity}"))?;
        Ok(Some(value.to_owned()))
    }

    fn remote_dist_tag(
        &self,
        name: &str,
        tag: &str,
        runner: &mut impl BaselineRunner,
    ) -> anyhow::Result<Option<String>> {
        let output = runner.capture(
            "npm",
            &strings(&["dist-tag", "ls", name, &self.registry_argument()]),
            &self.options,
        )?;
        Ok(parse_dist_tag_listing(&output, name)?.remove(tag))
    }

    fn ensure_dist_tag(
        &self,
        name: &str,
        tag: &str,
        runner: &mut impl BaselineRunner,
    ) -> anyhow::Result<()> {
        if self.remote_dist_tag(name, tag, runner)?.as_deref()
            == Some(self.bundle.manifest.version.as_str())
        {
            return Ok(());
        }
        runner.run(
            "npm",
            &strings(&[
                "dist-tag",
                "add",
                &format!("{name}@{}", self.bundle.manifest.version),
                tag,
                &self.registry_argument(),
            ]),
            &self.options,
        )
    }

    fn verify_remote(&self, runner: &mut impl BaselineRunner) -> anyhow::Result<()> {
        for package in &self.bundle.manifest.packages {
            let integrity = self
                .remote_integrity(package.name.as_str(), runner)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "package is missing: {}@{}",
                        package.name,
                        self.bundle.manifest.version
                    )
                })?;
            if integrity != package.integrity {
                anyhow::bail!(
                    "integrity mismatch: {}@{}",
                    package.name,
                    self.bundle.manifest.version
                );
            }
            let version = self.remote_dist_tag(
                package.name.as_str(),
                &self.bundle.manifest.dist_tag,
                runner,
            )?;
            if version.as_deref() != Some(self.bundle.manifest.version.as_str()) {
                anyhow::bail!(
                    "{}@{} points to {}; expected {}",
                    package.name,
                    self.bundle.manifest.dist_tag,
                    version.as_deref().unwrap_or("<missing>"),
                    self.bundle.manifest.version
                );
            }
            runner.log(&format!(
                "publish-npm-baseline: verified {}@{}",
                package.name, self.bundle.manifest.version
            ));
        }
        runner.log(&format!(
            "publish-npm-baseline: verified {} packages and dist-tag {}",
            self.bundle.manifest.packages.len(),
            self.bundle.manifest.dist_tag
        ));
        Ok(())
    }

    fn verify_entry_tag(&self, runner: &mut impl BaselineRunner) -> anyhow::Result<()> {
        let version = self.remote_dist_tag(RELEASE_ENTRY_PACKAGE, "latest", runner)?;
        if version.as_deref() != Some(self.bundle.manifest.version.as_str()) {
            anyhow::bail!(
                "{RELEASE_ENTRY_PACKAGE}@latest points to {}; expected {}",
                version.as_deref().unwrap_or("<missing>"),
                self.bundle.manifest.version
            );
        }
        runner.log(&format!(
            "publish-npm-baseline: verified {RELEASE_ENTRY_PACKAGE}@latest at {}",
            self.bundle.manifest.version
        ));
        Ok(())
    }
}

/// Validates HTTP(S) registry transport and removes trailing slashes without rewriting its spelling.
///
/// # Errors
/// Returns invalid URLs or unsupported transport diagnostics.
pub fn normalize_registry(value: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("registry must use HTTP or HTTPS: {value}");
    }
    Ok(value.trim_end_matches('/').to_owned())
}

/// Parses npm's line-oriented dist-tag output, rejecting malformed and duplicate entries.
///
/// # Errors
/// Returns source-compatible invalid-line or duplicate-tag diagnostics.
pub fn parse_dist_tag_listing(raw: &str, name: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut tags = BTreeMap::new();
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let Some((tag, version)) = line
            .split_once(": ")
            .filter(|(tag, version)| !tag.is_empty() && !version.is_empty())
        else {
            anyhow::bail!("registry returned an invalid dist-tag for {name}: {line}");
        };
        if tags.insert(tag.to_owned(), version.to_owned()).is_some() {
            anyhow::bail!("registry returned duplicate dist-tag {tag} for {name}");
        }
    }
    Ok(tags)
}
