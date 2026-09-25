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

/// The Cargo profile override the instrumented build receives unless the caller set one.
pub const DEBUGINFO_ENV: &str = "CARGO_PROFILE_DEV_DEBUG";

/// Line tables only: enough for readable panics, a fraction of full debug info's size.
pub const INSTRUMENTED_DEBUGINFO: &str = "line-tables-only";

/// Caller arguments the lane understands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoverageArguments {
    /// `--maxWorkers=N`: the test-thread ceiling, as the gate's worker budget names it.
    pub workers: Option<NonZeroUsize>,
    /// `--from-json <path>`: evaluate an existing llvm-cov export instead of running the suites.
    pub from_json: Option<PathBuf>,
    /// `--write-roster`: regenerate `scripts/coverage-roster.json` from this run.
    pub write_roster: bool,
    /// `--build-only`: compile the instrumented suites without running them, streaming the
    /// build so a CI log shows what the buffered gate would hide.
    pub build_only: bool,
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
        } else if text == "--build-only" {
            parsed.build_only = true;
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

/// The build-only counterpart of [`test_command`]: the same package selection compiled with
/// `cargo test --no-run` under cargo-llvm-cov's exported build environment, so the artifacts
/// serve the instrumented run afterwards.
#[must_use]
pub fn build_command(arguments: &CoverageArguments, excluded: &[&str]) -> Vec<OsString> {
    let mut command = [
        "test",
        "--locked",
        "--workspace",
        "--all-features",
        "--no-run",
    ]
    .map(OsString::from)
    .to_vec();
    for package in excluded {
        command.push("--exclude".into());
        command.push((*package).into());
    }
    command.extend(arguments.cargo.iter().cloned());
    command
}

/// Parses `cargo llvm-cov show-env --sh` output: `export NAME=value` lines whose values may be
/// single-quoted with `'\''` escapes.
///
/// # Errors
///
/// Returns a line that is not an export statement.
pub fn parse_coverage_environment(output: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let assignment = line
            .strip_prefix("export ")
            .ok_or_else(|| anyhow::anyhow!("run-coverage: unexpected show-env line {line:?}"))?;
        let (name, value) = assignment
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("run-coverage: unexpected show-env line {line:?}"))?;
        let value = value
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .map_or_else(|| value.to_owned(), |quoted| quoted.replace("'\\''", "'"));
        environment.insert(name.to_owned(), value);
    }
    Ok(environment)
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
    if arguments.build_only {
        return build_instrumented(&repository, &arguments, &measured);
    }
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

/// Compiles the instrumented suites without running them, under the environment
/// `cargo llvm-cov show-env` exports, streaming cargo's output; returns the process exit code.
fn build_instrumented(
    repository: &Path,
    arguments: &CoverageArguments,
    measured: &MeasuredSet,
) -> anyhow::Result<i32> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let exported = Command::new(&cargo)
        .args(["llvm-cov", "show-env", "--sh"])
        .current_dir(repository)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("run-coverage: read the instrumented build environment")?;
    anyhow::ensure!(
        exported.status.success(),
        "run-coverage: cargo llvm-cov show-env exited with {}; run `cargo install --locked cargo-llvm-cov --version {CARGO_LLVM_COV_VERSION}` and `rustup component add llvm-tools-preview`",
        exported.status
    );
    let environment = parse_coverage_environment(&String::from_utf8(exported.stdout)?)?;
    let workspace = workspace_metadata(&cargo, repository)?;
    prepare_runtime_assets(&cargo, repository, &workspace)?;
    let excluded = instrumented_exclusions(&workspace.packages, repository, measured);
    let excluded = excluded.iter().map(String::as_str).collect::<Vec<_>>();
    let build = build_command(arguments, &excluded);
    println!(
        "run-coverage: cargo {} (instrumented, build only)",
        build
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    // The exported environment names the target root; cargo-llvm-cov itself builds under its
    // `llvm-cov-target` subdirectory, which is where the instrumented run must find these
    // artifacts.
    let root = environment
        .get("CARGO_LLVM_COV_TARGET_DIR")
        .map_or_else(|| coverage_target_dir(repository), PathBuf::from);
    let mut command = Command::new(&cargo);
    command
        .args(&build)
        .current_dir(repository)
        .envs(&environment)
        .env("CARGO_TARGET_DIR", root.join("llvm-cov-target"))
        .env(COVERAGE_EXEMPT_ENV, "1");
    if std::env::var_os(DEBUGINFO_ENV).is_none() {
        command.env(DEBUGINFO_ENV, INSTRUMENTED_DEBUGINFO);
    }
    let status = command
        .status()
        .context("run-coverage: compile the instrumented suites")?;
    Ok(i32::from(!status.success()))
}

struct WorkspaceMetadata {
    packages: BTreeMap<String, PathBuf>,
    target_directory: PathBuf,
}

fn workspace_metadata(cargo: &OsStr, repository: &Path) -> anyhow::Result<WorkspaceMetadata> {
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
    let packages = metadata["packages"]
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
        .collect();
    let target_directory = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .context("run-coverage: Cargo metadata has no target directory")?,
    );
    Ok(WorkspaceMetadata {
        packages,
        target_directory,
    })
}

fn prepare_runtime_assets(
    cargo: &OsStr,
    repository: &Path,
    workspace: &WorkspaceMetadata,
) -> anyhow::Result<Vec<(&'static str, OsString)>> {
    if !workspace
        .packages
        .contains_key("seekdeep-code-runtime-worker-thread")
    {
        return Ok(Vec::new());
    }
    let status = Command::new(cargo)
        .args(["xtask", "host-assets"])
        .current_dir(repository)
        .status()
        .context("run-coverage: prepare the compiled Host runtime")?;
    anyhow::ensure!(
        status.success(),
        "run-coverage: cargo xtask host-assets exited with {status}"
    );
    let mut search_path = vec![workspace.target_directory.join("debug")];
    if let Some(inherited) = std::env::var_os("PATH") {
        search_path.extend(std::env::split_paths(&inherited));
    }
    Ok(vec![
        ("PATH", std::env::join_paths(search_path)?),
        (
            "SEEKDEEP_CODE_RUNTIME_NODE_DIR",
            workspace
                .target_directory
                .join("debug/code-runtime-node")
                .into_os_string(),
        ),
        (
            "SEEKDEEP_NODE_WASM",
            workspace
                .target_directory
                .join("wasm32-unknown-unknown/release/seekdeep_code_runtime_node.wasm")
                .into_os_string(),
        ),
    ])
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
    let workspace = workspace_metadata(&cargo, repository)?;
    let runtime = prepare_runtime_assets(&cargo, repository, &workspace)?;
    let excluded = instrumented_exclusions(&workspace.packages, repository, measured);
    let excluded = excluded.iter().map(String::as_str).collect::<Vec<_>>();
    let test = test_command(arguments, &excluded);
    println!(
        "run-coverage: cargo {}",
        test.iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut instrumented = Command::new(&cargo);
    instrumented
        .args(&test)
        .current_dir(repository)
        .envs(runtime)
        .env(COVERAGE_EXEMPT_ENV, "1");
    // Coverage mapping does not need full DWARF, and full debug info makes the instrumented
    // build several times larger than a hosted runner's disk; line tables keep panics readable.
    if std::env::var_os(DEBUGINFO_ENV).is_none() {
        instrumented.env(DEBUGINFO_ENV, INSTRUMENTED_DEBUGINFO);
    }
    let status = instrumented
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
