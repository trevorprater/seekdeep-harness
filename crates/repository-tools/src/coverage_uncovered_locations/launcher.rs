//! Public Vitest coverage entry with a compiled Rust reporter prerequisite.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use anyhow::Context as _;

const REPORT_BINARY: &str = "coverage-uncovered-locations";

fn configuration_selected(root: &Path, arguments: &[OsString]) -> bool {
    let explicit = arguments
        .iter()
        .take_while(|argument| *argument != "--")
        .any(|argument| {
            let argument = argument.to_string_lossy();
            matches!(
                argument.as_ref(),
                "--config" | "--no-config" | "-c" | "--root" | "-r"
            ) || argument.starts_with("--config=")
                || argument.starts_with("--root=")
                || argument.starts_with("--coverage.reporter")
        });
    explicit
        || ["vitest.config", "vite.config"].iter().any(|name| {
            ["ts", "mts", "cts", "js", "mjs", "cjs"]
                .iter()
                .any(|extension| root.join(format!("{name}.{extension}")).is_file())
        })
}

/// Preserve caller arguments and select the source reporter defaults in the default context.
#[must_use]
pub fn coverage_arguments(
    root: &Path,
    reporter: &Path,
    arguments: &[OsString],
    ci: bool,
) -> Vec<OsString> {
    let mut output = vec![OsString::from("run"), OsString::from("--coverage")];
    if !configuration_selected(root, arguments) {
        output.push(OsString::from("--coverage.reporter=text"));
        if !ci {
            output.push(OsString::from("--coverage.reporter=html"));
        }
        let mut reporter_argument = OsString::from("--coverage.reporter=");
        reporter_argument.push(reporter);
        output.push(reporter_argument);
    }
    output.extend_from_slice(arguments);
    output
}

fn compiled_reporter(repository: &Path) -> anyhow::Result<PathBuf> {
    if let Some(binary) = std::env::var_os("SEEKDEEP_COVERAGE_REPORT_BIN") {
        let binary = PathBuf::from(binary);
        anyhow::ensure!(
            binary.is_file(),
            "run-coverage: compiled reporter does not exist: {}",
            binary.display()
        );
        return binary
            .canonicalize()
            .context("run-coverage: resolve compiled reporter");
    }
    let output = Command::new("cargo")
        .current_dir(repository)
        .args([
            "build",
            "--quiet",
            "--package",
            "seekdeep-repository-tools",
            "--bin",
            REPORT_BINARY,
            "--message-format=json-render-diagnostics",
        ])
        .stderr(Stdio::inherit())
        .output()
        .context("run-coverage: build compiled Rust coverage reporter")?;
    anyhow::ensure!(
        output.status.success(),
        "run-coverage: Rust coverage reporter build failed ({})",
        output.status
    );
    String::from_utf8(output.stdout)?
        .lines()
        .rev()
        .find_map(|line| {
            let message: serde_json::Value = serde_json::from_str(line).ok()?;
            if message["reason"] != "compiler-artifact"
                || message["target"]["name"] != REPORT_BINARY
            {
                return None;
            }
            message["executable"].as_str().map(PathBuf::from)
        })
        .filter(|path| path.is_file())
        .context("run-coverage: Cargo did not emit its native coverage reporter executable")
}

fn vitest_entry(node: &OsStr, root: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new(node)
        .current_dir(root)
        .args(["-e", "process.stdout.write(require.resolve('vitest/package.json', { paths: [process.cwd()] }));"])
        .output()
        .context("run-coverage: locate installed Vitest")?;
    anyhow::ensure!(
        output.status.success(),
        "run-coverage: Vitest is not installed for {}; run pnpm install before test:coverage",
        root.display()
    );
    let manifest = PathBuf::from(String::from_utf8(output.stdout)?);
    let package: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    let executable = package
        .pointer("/bin/vitest")
        .and_then(serde_json::Value::as_str)
        .or_else(|| package.get("bin").and_then(serde_json::Value::as_str))
        .context("run-coverage: installed Vitest package declares no CLI executable")?;
    let entry = manifest
        .parent()
        .context("run-coverage: Vitest manifest has no parent")?
        .join(executable);
    anyhow::ensure!(
        entry.is_file(),
        "run-coverage: installed Vitest CLI is missing: {}",
        entry.display()
    );
    Ok(entry)
}

/// Build the native reporter and run the installed Vitest CLI with its ordinary argument/config handling.
///
/// # Errors
/// Returns prerequisite, executable resolution, environment, or process launch failures.
pub fn run_coverage(repository: &Path, arguments: &[OsString]) -> anyhow::Result<ExitStatus> {
    let root = std::env::current_dir()?;
    let node = std::env::var_os("npm_node_execpath").unwrap_or_else(|| OsString::from("node"));
    let entry = vitest_entry(&node, &root)?;
    let binary = compiled_reporter(repository)?;
    let reporter = repository.join("scripts/coverage-uncovered-locations.cjs");
    anyhow::ensure!(
        reporter.is_file(),
        "run-coverage: coverage reporter adapter is missing: {}",
        reporter.display()
    );
    let ci = std::env::var_os("CI").is_some_and(|value| !value.is_empty());
    let mut node_paths = vec![root.join("node_modules")];
    if let Some(paths) = std::env::var_os("NODE_PATH") {
        node_paths.extend(std::env::split_paths(&paths));
    }
    Command::new(node)
        .arg(entry)
        .args(coverage_arguments(&root, &reporter, arguments, ci))
        .env("SEEKDEEP_COVERAGE_REPORT_BIN", binary)
        .env("NODE_PATH", std::env::join_paths(node_paths)?)
        .status()
        .context("run-coverage: execute Vitest")
}
