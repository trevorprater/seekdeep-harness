//! Commit-addressed npm baseline packing, isolated installation, and publication.

mod bundle;
mod capture;
mod command;
mod pack;
mod registry;
mod smoke;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{IsTerminal as _, Write as _},
    path::Path,
};

use serde::{Deserialize, Serialize};

pub use bundle::{
    BaselinePackage, PackageOrigin, PackedBaselinePackage, ReleaseBundle, ReleaseManifest,
    WorkspacePackageSet,
};
pub use command::{baseline_main, usage};
pub use pack::{BaselinePackOptions, BaselinePackPlan, pack_baseline, plan_baseline};
pub use registry::{RegistryPublication, normalize_registry, parse_dist_tag_listing};
pub use smoke::{
    InstalledWebProbe, installed_artifact_environment, probe_installed_web, smoke_installed_bundle,
};

use crate::release_process::{ReleaseCommandResult, ReleaseRunOptions};

/// Registry used when the caller does not supply one.
pub const DEFAULT_REGISTRY: &str = "https://registry.npm.harnessment.com";
/// Repository-relative root for immutable baseline bundles.
pub const DEFAULT_OUTPUT_DIRECTORY: &str = ".artifacts/npm-baseline";
/// Release bundle metadata filename.
pub const RELEASE_MANIFEST_NAME: &str = "manifest.json";
/// Package whose `latest` dist-tag identifies the complete baseline.
pub const RELEASE_ENTRY_PACKAGE: &str = "@seekdeep-ai/seekdeep";

macro_rules! identifier {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps an identifier validated at its owning boundary.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the preserved wire value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

identifier!(BaselineCommit, "Commit identifying a packed baseline.");
identifier!(
    BaselineVersion,
    "Version shared by every package in one baseline."
);
identifier!(
    BaselinePackageName,
    "Package identity carried by baseline metadata and registry requests."
);

/// Process, terminal, output, and confirmation boundaries for a baseline command.
pub trait BaselineRunner {
    /// Captures a shell-free process without judging its exit status.
    ///
    /// # Errors
    /// Returns process-spawn, wait, or capture failures.
    fn result(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<ReleaseCommandResult>;

    /// Runs a command with inherited streams and requires success.
    ///
    /// # Errors
    /// Returns execution failures and nonzero or signalled exit diagnostics.
    fn run(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<()> {
        let result = self.result(command, arguments, options)?;
        if result.status != Some(0) {
            anyhow::bail!(
                "{} exited with status {}",
                format_command(command, arguments),
                result
                    .status
                    .map_or_else(|| "null".to_owned(), |status| status.to_string())
            );
        }
        Ok(())
    }

    /// Captures a successful command's trimmed stdout.
    ///
    /// # Errors
    /// Returns execution failures including captured output on a nonzero exit.
    fn capture(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<String> {
        let result = self.result(command, arguments, options)?;
        if result.status != Some(0) {
            return Err(command_failure(command, arguments, &result));
        }
        Ok(result.stdout.trim().to_owned())
    }

    /// Reports a progress line.
    fn log(&mut self, message: &str);

    /// Reports a cleanup warning without changing the command's result.
    fn warn(&mut self, message: &str);

    /// Waits for an empty line on an interactive terminal.
    ///
    /// # Errors
    /// Returns the supplied noninteractive or cancellation diagnostic.
    fn confirm(
        &mut self,
        prompt: &str,
        noninteractive_error: &str,
        cancellation_error: &str,
    ) -> anyhow::Result<()>;

    /// Probes an installed entry under a POSIX terminal and shuts it down.
    ///
    /// # Errors
    /// Returns spawn, readiness, timeout, and shutdown failures.
    fn web_probe(&mut self, probe: &InstalledWebProbe) -> anyhow::Result<()>;
}

/// Real process and terminal implementation of the baseline command boundaries.
#[derive(Default)]
pub struct SystemBaselineRunner;

impl BaselineRunner for SystemBaselineRunner {
    fn result(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<ReleaseCommandResult> {
        capture::capture_process(command, arguments, options)
    }

    fn run(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<()> {
        let mut child = std::process::Command::new(command);
        child.args(arguments);
        if let Some(cwd) = &options.cwd {
            child.current_dir(cwd);
        }
        if let Some(environment) = &options.env {
            child.env_clear().envs(environment);
        }
        let status = child
            .status()
            .map_err(|error| capture::spawn_failure(command, &error))?
            .code();
        if status != Some(0) {
            anyhow::bail!(
                "{} exited with status {}",
                format_command(command, arguments),
                status.map_or_else(|| "null".to_owned(), |status| status.to_string())
            );
        }
        Ok(())
    }

    fn log(&mut self, message: &str) {
        println!("{message}");
    }

    fn warn(&mut self, message: &str) {
        eprintln!("{message}");
    }

    fn confirm(
        &mut self,
        prompt: &str,
        noninteractive_error: &str,
        cancellation_error: &str,
    ) -> anyhow::Result<()> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            anyhow::bail!("{noninteractive_error}");
        }
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        let length = std::io::stdin().read_line(&mut line)?;
        if length == 0 || !line.trim_end_matches(['\r', '\n']).is_empty() {
            anyhow::bail!("{cancellation_error}");
        }
        Ok(())
    }

    fn web_probe(&mut self, probe: &InstalledWebProbe) -> anyhow::Result<()> {
        probe_installed_web(probe)
    }
}

/// Removes pnpm's inherited npm user-agent override from a child environment.
#[must_use]
pub fn npm_client_environment(
    parent: &BTreeMap<OsString, OsString>,
) -> BTreeMap<OsString, OsString> {
    let mut environment = parent.clone();
    environment.remove(std::ffi::OsStr::new("npm_config_user_agent"));
    environment.remove(std::ffi::OsStr::new("NPM_CONFIG_USER_AGENT"));
    environment
}

fn run_options(root: &Path) -> ReleaseRunOptions {
    ReleaseRunOptions {
        cwd: Some(root.to_owned()),
        env: None,
    }
}

fn strings(arguments: &[&str]) -> Vec<String> {
    arguments.iter().map(|value| (*value).to_owned()).collect()
}

fn format_command(command: &str, arguments: &[String]) -> String {
    std::iter::once(command)
        .chain(arguments.iter().map(String::as_str))
        .map(|value| serde_json::Value::String(value.to_owned()).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_failure(
    command: &str,
    arguments: &[String],
    result: &ReleaseCommandResult,
) -> anyhow::Error {
    let detail = [result.stdout.trim(), result.stderr.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!(
        "{} exited with status {}{}",
        format_command(command, arguments),
        result.status.unwrap_or(1),
        if detail.is_empty() {
            String::new()
        } else {
            format!("\n{detail}")
        }
    )
}
