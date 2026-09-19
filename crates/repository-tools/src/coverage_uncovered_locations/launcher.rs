//! Public coverage entry: the instrumented Rust lane behind `pnpm run test:coverage`.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Context as _;

use super::lane::{
    CARGO_LLVM_COV_VERSION, Host, MeasuredSet, ROSTER_PATH, Roster, evaluate, regenerate_roster,
    roster_additions, translate_export,
};
use crate::coverage_exempt::{COVERAGE_EXEMPT_ENV, INSTRUMENTED_LANE_EXCLUDED_PACKAGES};

/// Caller arguments the lane understands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoverageArguments {
    /// `--maxWorkers=N`: the test-thread ceiling, as the gate's worker budget names it.
    pub workers: Option<NonZeroUsize>,
    /// `--from-json <path>`: evaluate an existing llvm-cov export instead of running the suites.
    pub from_json: Option<PathBuf>,
    /// `--write-roster`: regenerate `scripts/coverage-roster.json` from this run.
    pub write_roster: bool,
    /// `--repository <path>`: the repository to measure instead of the compiled one.
    pub repository: Option<PathBuf>,
    /// Further `cargo llvm-cov` arguments, such as `-p <crate>`.
    pub cargo: Vec<OsString>,
    /// Arguments after `--`, handed to every test harness.
    pub harness: Vec<OsString>,
}

/// Parses the public entry's arguments.
///
/// # Errors
///
/// Returns an invalid worker count or a flag without its value.
pub fn parse_arguments(arguments: &[OsString]) -> anyhow::Result<CoverageArguments> {
    let mut parsed = CoverageArguments::default();
    let mut iterator = arguments.iter();
    while let Some(argument) = iterator.next() {
        if argument == "--" {
            parsed.harness.extend(iterator.cloned());
            break;
        }
        let text = argument.to_string_lossy();
        if let Some(value) = text
            .strip_prefix("--maxWorkers=")
            .or_else(|| text.strip_prefix("--max-workers="))
        {
            parsed.workers = Some(workers(value)?);
        } else if text == "--maxWorkers" || text == "--max-workers" {
            parsed.workers = Some(workers(&value_of(&mut iterator, &text)?)?);
        } else if let Some(value) = text.strip_prefix("--from-json=") {
            parsed.from_json = Some(PathBuf::from(value));
        } else if text == "--from-json" {
            parsed.from_json = Some(PathBuf::from(value_of(&mut iterator, &text)?));
        } else if let Some(value) = text.strip_prefix("--repository=") {
            parsed.repository = Some(PathBuf::from(value));
        } else if text == "--repository" {
            parsed.repository = Some(PathBuf::from(value_of(&mut iterator, &text)?));
        } else if text == "--write-roster" {
            parsed.write_roster = true;
        } else {
            parsed.cargo.push(argument.clone());
        }
    }
    Ok(parsed)
}

fn value_of(iterator: &mut std::slice::Iter<'_, OsString>, flag: &str) -> anyhow::Result<String> {
    iterator
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("run-coverage: {flag} needs a value"))
}

fn workers(value: &str) -> anyhow::Result<NonZeroUsize> {
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("run-coverage: invalid worker count {value:?}"))
}

/// The instrumented test run: every workspace suite under instrumentation except `excluded`
/// (see [`instrumented_exclusions`]), with no fail-fast so one failing suite leaves the others
/// measured.
///
/// Each test target runs in its own process, as `cargo test` always does, on every platform:
/// the port's counterpart of the source keeping every Vitest project on the forks pool.
#[must_use]
pub fn test_command(arguments: &CoverageArguments, excluded: &[&str]) -> Vec<OsString> {
    let mut command = [
        "llvm-cov",
        "--no-report",
        "--locked",
        "--workspace",
        "--all-features",
        "--no-fail-fast",
    ]
    .map(OsString::from)
    .to_vec();
    for package in excluded {
        command.push("--exclude-from-test".into());
        command.push((*package).into());
    }
    command.extend(arguments.cargo.iter().cloned());
    if arguments.workers.is_some() || !arguments.harness.is_empty() {
        command.push("--".into());
        if let Some(workers) = arguments.workers {
            command.push(format!("--test-threads={workers}").into());
        }
        command.extend(arguments.harness.iter().cloned());
    }
    command
}

/// The export step after the instrumented run.
#[must_use]
pub fn report_command(output: &Path) -> Vec<OsString> {
    ["llvm-cov", "report", "--json", "--output-path"]
        .map(OsString::from)
        .into_iter()
        .chain(std::iter::once(output.as_os_str().to_owned()))
        .collect()
}

/// The Cargo target directory the lane writes its export under.
#[must_use]
pub fn coverage_target_dir(repository: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| repository.join("target"), PathBuf::from)
}

/// Runs the lane and returns the process exit code: 1 when a suite failed, a measured file is
/// below the bar without a roster entry in force, or the roster is stale.
///
/// # Errors
///
/// Returns prerequisite, process launch, export, roster, or manifest failures.
pub fn run_coverage(repository: &Path, arguments: &[OsString]) -> anyhow::Result<i32> {
    let arguments = parse_arguments(arguments)?;
    let repository = arguments
        .repository
        .clone()
        .unwrap_or_else(|| repository.to_path_buf());
    let repository = dunce::canonicalize(&repository)
        .with_context(|| format!("run-coverage: resolve {}", repository.display()))?;
    let measured = MeasuredSet::load(&repository)?;
    let roster_path = repository.join(ROSTER_PATH);
    let roster = Roster::load(&roster_path)?;
    let (export_path, suites_passed) = match &arguments.from_json {
        Some(path) => (path.clone(), true),
        None => run_instrumented(&repository, &arguments, &measured)?,
    };
    let export: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&export_path)
            .with_context(|| format!("run-coverage: read {}", export_path.display()))?,
    )?;
    let files = translate_export(&export, &repository, &measured)?;
    let host = Host::detect();
    let evaluation = evaluate(&files, &roster, &host, &measured, &repository)?;
    for line in &evaluation.summary {
        println!("run-coverage: {line}");
    }
    for line in &evaluation.report {
        println!("{line}");
    }
    for line in &evaluation.notices {
        println!("run-coverage: {line}");
    }
    if arguments.write_roster {
        let regenerated = regenerate_roster(&roster, &evaluation, &host);
        regenerated.save(&roster_path)?;
        println!(
            "run-coverage: wrote {ROSTER_PATH} with {} entries",
            regenerated.files.len()
        );
        return Ok(i32::from(!suites_passed));
    }
    for line in &evaluation.errors {
        eprintln!("{line}");
    }
    let additions = roster_additions(&evaluation, &roster, &host);
    if !additions.is_empty() {
        println!(
            "run-coverage: roster entries this host would need: {}",
            serde_json::to_string(&additions)?
        );
    }
    Ok(i32::from(!suites_passed || !evaluation.errors.is_empty()))
}

/// The packages the instrumented run leaves to the uninstrumented lanes: the exempt heavy-suite
/// hosts the workspace defines, and every package outside the measured set (tooling, native
/// helpers, vendor ports), whose suites are the heaviest and measure nothing the bar applies to.
#[must_use]
pub fn instrumented_exclusions(
    members: &BTreeMap<String, PathBuf>,
    repository: &Path,
    measured: &MeasuredSet,
) -> Vec<String> {
    let mut excluded = BTreeSet::new();
    for (name, directory) in members {
        let relative = directory.strip_prefix(repository).ok().map(|relative| {
            relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        });
        let measured_crate = relative
            .as_deref()
            .is_some_and(|relative| measured.crates().any(|directory| directory == relative));
        if !measured_crate || INSTRUMENTED_LANE_EXCLUDED_PACKAGES.contains(&name.as_str()) {
            excluded.insert(name.clone());
        }
    }
    excluded.into_iter().collect()
}

/// The packages the repository's workspace defines: name to manifest directory.
fn workspace_packages(
    cargo: &OsStr,
    repository: &Path,
) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repository)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("run-coverage: read the workspace metadata")?;
    anyhow::ensure!(
        output.status.success(),
        "run-coverage: cargo metadata exited with {}",
        output.status
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let name = package["name"].as_str()?.to_owned();
            let manifest = Path::new(package["manifest_path"].as_str()?);
            let directory = dunce::canonicalize(manifest.parent()?)
                .unwrap_or_else(|_| manifest.parent().map(Path::to_path_buf).unwrap_or_default());
            Some((name, directory))
        })
        .collect())
}

fn run_instrumented(
    repository: &Path,
    arguments: &CoverageArguments,
    measured: &MeasuredSet,
) -> anyhow::Result<(PathBuf, bool)> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let installed = Command::new(&cargo)
        .args(["llvm-cov", "--version"])
        .current_dir(repository)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    anyhow::ensure!(
        installed,
        "run-coverage: cargo-llvm-cov is not installed; run `cargo install --locked cargo-llvm-cov --version {CARGO_LLVM_COV_VERSION}` and `rustup component add llvm-tools-preview`"
    );
    let members = workspace_packages(&cargo, repository)?;
    let excluded = instrumented_exclusions(&members, repository, measured);
    let excluded = excluded.iter().map(String::as_str).collect::<Vec<_>>();
    let test = test_command(arguments, &excluded);
    println!(
        "run-coverage: cargo {}",
        test.iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let status = Command::new(&cargo)
        .args(&test)
        .current_dir(repository)
        .env(COVERAGE_EXEMPT_ENV, "1")
        .status()
        .context("run-coverage: execute the instrumented suites")?;
    let output = coverage_target_dir(repository)
        .join("coverage")
        .join("llvm-cov.json");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let report = Command::new(&cargo)
        .args(report_command(&output))
        .current_dir(repository)
        .status()
        .context("run-coverage: export the coverage report")?;
    anyhow::ensure!(
        report.success(),
        "run-coverage: cargo llvm-cov report exited with {report}"
    );
    Ok((output, status.success()))
}
