//! Bounded dependency-graph scheduling for local and CI repository gates.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use indexmap::IndexMap;

use crate::coverage_exempt::{COVERAGE_EXEMPT_ENV, COVERAGE_EXEMPT_HEAVY_SUITES};

const MODES: &str = "ci-primary | ci-linux-primary | ci-static | ci-lint-contracts-ready | ci-coverage | ci-snapshot | ci-artifacts | ci-consumers | ci-windows-blocking | ci-windows-complete | ci-windows-observational | node-compat | check-all | doc-sync";

/// Named aggregate exposed by the gate runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateMode {
    /// Complete primary CI lane.
    CiPrimary,
    /// Primary Linux lane plus built Web comparison.
    CiLinuxPrimary,
    /// Source-only static lane.
    CiStatic,
    /// Lint and duplication after external contract preparation.
    CiLintContractsReady,
    /// Instrumented and exempt coverage lanes.
    CiCoverage,
    /// Build plus snapshot lane.
    CiSnapshot,
    /// Built-artifact validation lane.
    CiArtifacts,
    /// Validated-build consumer lane.
    CiConsumers,
    /// Required Windows build outputs.
    CiWindowsBlocking,
    /// Required Windows outputs plus complete observational inventory.
    CiWindowsComplete,
    /// Non-blocking Windows portability inventory.
    CiWindowsObservational,
    /// Advertised Node-version compatibility checks.
    NodeCompat,
    /// Comprehensive local aggregate.
    CheckAll,
    /// Documentation synchronization aggregate.
    DocSync,
}

impl GateMode {
    /// Parses one exact CLI mode.
    ///
    /// # Errors
    ///
    /// Returns the complete accepted-mode diagnostic for any other value.
    pub fn parse(raw: Option<&str>) -> anyhow::Result<Self> {
        match raw {
            Some("ci-primary") => Ok(Self::CiPrimary),
            Some("ci-linux-primary") => Ok(Self::CiLinuxPrimary),
            Some("ci-static") => Ok(Self::CiStatic),
            Some("ci-lint-contracts-ready") => Ok(Self::CiLintContractsReady),
            Some("ci-coverage") => Ok(Self::CiCoverage),
            Some("ci-snapshot") => Ok(Self::CiSnapshot),
            Some("ci-artifacts") => Ok(Self::CiArtifacts),
            Some("ci-consumers") => Ok(Self::CiConsumers),
            Some("ci-windows-blocking") => Ok(Self::CiWindowsBlocking),
            Some("ci-windows-complete") => Ok(Self::CiWindowsComplete),
            Some("ci-windows-observational") => Ok(Self::CiWindowsObservational),
            Some("node-compat") => Ok(Self::NodeCompat),
            Some("check-all") => Ok(Self::CheckAll),
            Some("doc-sync") => Ok(Self::DocSync),
            value => {
                let shown = value.map_or_else(
                    || "undefined".to_owned(),
                    |value| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned()),
                );
                anyhow::bail!("run-gates: expected mode {MODES}, got {shown}.")
            }
        }
    }

    /// Exact CLI spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CiPrimary => "ci-primary",
            Self::CiLinuxPrimary => "ci-linux-primary",
            Self::CiStatic => "ci-static",
            Self::CiLintContractsReady => "ci-lint-contracts-ready",
            Self::CiCoverage => "ci-coverage",
            Self::CiSnapshot => "ci-snapshot",
            Self::CiArtifacts => "ci-artifacts",
            Self::CiConsumers => "ci-consumers",
            Self::CiWindowsBlocking => "ci-windows-blocking",
            Self::CiWindowsComplete => "ci-windows-complete",
            Self::CiWindowsObservational => "ci-windows-observational",
            Self::NodeCompat => "node-compat",
            Self::CheckAll => "check-all",
            Self::DocSync => "doc-sync",
        }
    }
}

/// One command and its dependency metadata inside an aggregate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate {
    /// Stable graph identifier.
    pub id: String,
    /// Human-readable progress label.
    pub label: String,
    /// Shell-like command shown in diagnostics.
    pub display_command: String,
    /// Exact shell-free executable.
    pub command: PathBuf,
    /// Exact child arguments.
    pub args: Vec<OsString>,
    /// Prerequisite gate identifiers.
    pub needs: Vec<String>,
    /// Environment overlays; `None` removes an inherited value.
    pub environment: IndexMap<String, Option<String>>,
    /// Whether failure is reported but does not fail the aggregate.
    pub allow_failure: bool,
    /// Optional mutually exclusive runtime resource group.
    pub serial_group: Option<String>,
}

/// Gate process status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateResultStatus {
    /// Process completed successfully.
    Passed,
    /// Process or spawn failed.
    Failed,
    /// A prerequisite failed or was skipped.
    Skipped,
}

/// Output stream for one captured chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateOutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// One captured child-output chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateOutputChunk {
    /// Originating stream.
    pub stream: GateOutputStream,
    /// Lossy UTF-8 text, matching Node child stream decoding.
    pub text: String,
}

/// Observed result of one gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateResult {
    /// Executed or skipped gate.
    pub gate: Gate,
    /// Terminal status.
    pub status: GateResultStatus,
    /// Wall-clock duration.
    pub duration: Duration,
    /// Captured output in observed chunk order.
    pub output: Vec<GateOutputChunk>,
    /// Numeric process status when exited normally.
    pub exit_code: Option<i32>,
    /// Platform signal name when terminated by a signal.
    pub signal_code: Option<String>,
    /// Independent spawn or scheduler diagnostic.
    pub error: Option<String>,
}

/// Default concurrency plus the human-readable source of that choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConcurrencyDefault {
    /// Worker count.
    pub workers: usize,
    /// Diagnostic source.
    pub source: String,
}

/// Process facts used to construct deterministic gate graphs.
#[derive(Clone, Debug)]
pub struct GateEnvironment {
    /// Complete inherited environment.
    pub variables: BTreeMap<OsString, OsString>,
    /// Node executable used to invoke pnpm's JavaScript entrypoint.
    pub node_executable: PathBuf,
    /// Pnpm JavaScript entrypoint from `npm_execpath`.
    pub pnpm_entrypoint: PathBuf,
    /// Active Node major.
    pub node_major: u32,
    /// Host parallelism advertised to defaults.
    pub available_parallelism: usize,
}

impl GateEnvironment {
    /// Discovers the environment supplied by a pnpm package-script invocation.
    ///
    /// # Errors
    ///
    /// Returns missing pnpm entrypoint, Node spawn/version, or parallelism errors.
    pub fn from_process() -> anyhow::Result<Self> {
        let variables = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let pnpm_entrypoint = variables
            .get(std::ffi::OsStr::new("npm_execpath"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "run-gates: npm_execpath is unavailable; invoke the runner through a pnpm package script."
                )
            })?;
        let node_executable = variables
            .get(std::ffi::OsStr::new("npm_node_execpath"))
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("node"), PathBuf::from);
        let version = Command::new(&node_executable).arg("--version").output()?;
        anyhow::ensure!(
            version.status.success(),
            "run-gates: cannot read Node version from {}",
            node_executable.display()
        );
        let version = String::from_utf8_lossy(&version.stdout)
            .trim()
            .trim_start_matches('v')
            .to_owned();
        let node_major = version
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .ok_or_else(|| anyhow::anyhow!("run-gates: cannot parse Node version {version:?}."))?;
        Ok(Self {
            variables,
            node_executable,
            pnpm_entrypoint,
            node_major,
            available_parallelism: std::thread::available_parallelism()?.get(),
        })
    }

    fn variable(&self, name: &str) -> Option<&str> {
        self.variables
            .get(std::ffi::OsStr::new(name))
            .and_then(|value| value.to_str())
    }
}

/// Computes one aggregate's default worker count.
#[must_use]
pub fn default_concurrency(mode: GateMode, total: usize, available: usize) -> ConcurrencyDefault {
    if mode == GateMode::CiConsumers {
        return ConcurrencyDefault {
            workers: total,
            source: "ci-consumers gate count".to_owned(),
        };
    }
    let local_cap = matches!(mode, GateMode::CheckAll | GateMode::DocSync);
    let mode_limit = if local_cap {
        available.min(4)
    } else {
        available
    };
    ConcurrencyDefault {
        workers: total.min(mode_limit),
        source: if local_cap {
            format!("{available} available CPU(s), {} cap 4", mode.as_str())
        } else {
            format!("{available} available CPU(s)")
        },
    }
}

/// Resolves a worker override with JavaScript `parseInt` compatibility.
///
/// # Errors
///
/// Returns a positive-integer diagnostic for invalid values.
pub fn concurrency_from_environment(
    environment: &GateEnvironment,
    fallback: usize,
) -> anyhow::Result<(usize, String)> {
    let name = "SEEKDEEP_GATE_CONCURRENCY";
    let Some(raw) = environment.variable(name).filter(|raw| !raw.is_empty()) else {
        return Ok((fallback, String::new()));
    };
    let value = parse_integer_prefix(raw).filter(|value| *value >= 1);
    let Some(value) = value.and_then(|value| usize::try_from(value).ok()) else {
        anyhow::bail!(
            "run-gates: {name} must be a positive integer, got {}.",
            serde_json::to_string(raw)?
        );
    };
    Ok((value, format!("${name}")))
}

fn parse_integer_prefix(raw: &str) -> Option<i64> {
    let raw = raw.trim_start();
    let (sign, digits) = match raw.as_bytes().first() {
        Some(b'-') => (-1_i64, &raw[1..]),
        Some(b'+') => (1_i64, &raw[1..]),
        _ => (1_i64, raw),
    };
    let digits = digits
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| {
        digits
            .parse::<i64>()
            .ok()
            .and_then(|value| value.checked_mul(sign))
    })?
}

/// Constructs the complete gate graph for one mode.
///
/// # Errors
///
/// Returns invalid environment-bound worker/flag diagnostics.
pub fn gates_for_mode(mode: GateMode, environment: &GateEnvironment) -> anyhow::Result<Vec<Gate>> {
    match mode {
        GateMode::CiPrimary => ci_primary_gates(environment),
        GateMode::CiLinuxPrimary => {
            let mut gates = ci_primary_gates(environment)?;
            gates.push(web_snapshot_gate(
                environment,
                &["built-package-invariants"],
            ));
            Ok(gates)
        }
        GateMode::CiStatic => Ok(ci_static_gates(environment, false)),
        GateMode::CiLintContractsReady => Ok(vec![
            lint_gate(environment, &[]),
            pnpm_script(environment, "duplication", "duplication"),
        ]),
        GateMode::CiCoverage => coverage_gates(environment),
        GateMode::CiSnapshot => Ok(vec![
            pnpm_script(environment, "build", "build"),
            snapshot_gate(environment, &["build"]),
        ]),
        GateMode::CiArtifacts => Ok(ci_artifact_gates(environment)),
        GateMode::CiConsumers => Ok(ci_consumer_gates(environment)),
        GateMode::CiWindowsBlocking => Ok(ci_windows_blocking_gates(environment)),
        GateMode::CiWindowsComplete => ci_windows_complete_gates(environment),
        GateMode::CiWindowsObservational => Ok(ci_windows_observational_gates(environment)),
        GateMode::NodeCompat => node_compat_gates(environment),
        GateMode::CheckAll => Ok(check_all_gates(environment)),
        GateMode::DocSync => Ok(doc_sync_leaf_gates(environment, DocSyncOptions::default())),
    }
}

fn check_all_gates(environment: &GateEnvironment) -> Vec<Gate> {
    let mut gates = vec![
        labeled_script(
            environment,
            "runtime-closure",
            "verify-runtime-closure",
            "runtime closure",
        ),
        labeled_script(
            environment,
            "cordis-config",
            "verify-cordis-config",
            "Cordis config",
        ),
        labeled_script(
            environment,
            "client-domain-graph",
            "verify-client-domain-graph",
            "client domain graph",
        ),
        pnpm_script(environment, "test", "test"),
        labeled_script(
            environment,
            "issue-management",
            "test:issue-management",
            "Issue management policy",
        ),
        pnpm_script(environment, "duplication", "duplication"),
        snapshot_gate(environment, &["build"]),
        pnpm_script(environment, "build", "build"),
        pnpm_script(environment, "build:web", "build:web"),
    ];
    gates.extend(hygiene_leaf_gates(environment, &["build"]));
    gates.extend(doc_sync_leaf_gates(
        environment,
        DocSyncOptions {
            doc_typecheck_needs: vec!["build"],
            doc_typecheck_environment: environment_map(&[(
                "SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT",
                Some("1"),
            )]),
            doc_typecheck_script: "doc-typecheck:contracts-ready",
            ..DocSyncOptions::default()
        },
    ));
    gates.push(labeled_script(
        environment,
        "module-graph",
        "verify-module-graph",
        "module graph",
    ));
    gates
}

fn ci_shared_static_gates(environment: &GateEnvironment) -> Vec<Gate> {
    vec![
        labeled_script(
            environment,
            "runtime-closure",
            "verify-runtime-closure",
            "runtime closure",
        ),
        pnpm_script(environment, "constraints", "constraints"),
        labeled_script(
            environment,
            "seekdeep-package-licenses",
            "verify-seekdeep-package-licenses",
            "SeekDeep package licenses",
        ),
        labeled_script(
            environment,
            "package-invariants",
            "verify-package-invariants",
            "package invariants",
        ),
        labeled_script(
            environment,
            "cordis-config",
            "verify-cordis-config",
            "Cordis config",
        ),
        labeled_script(
            environment,
            "issue-management",
            "test:issue-management",
            "Issue management policy",
        ),
    ]
}

fn ci_primary_gates(environment: &GateEnvironment) -> anyhow::Result<Vec<Gate>> {
    let mut gates = ci_shared_static_gates(environment);
    gates.push(typert_contracts_gate(environment));
    gates.push(script_with(
        environment,
        "typecheck",
        "typecheck:contracts-ready",
        None,
        &["typert-contracts"],
        IndexMap::new(),
    ));
    gates.push(lint_gate(environment, &["typert-contracts"]));
    gates.push(pnpm_script(environment, "duplication", "duplication"));
    gates.extend(coverage_gates(environment)?);
    gates.extend(node_compat_smoke_gates(environment, false));
    gates.push(snapshot_gate(environment, &["build"]));
    gates.extend(doc_sync_leaf_gates(
        environment,
        DocSyncOptions {
            doc_typecheck_needs: vec!["typert-contracts"],
            doc_typecheck_script: "doc-typecheck:contracts-ready",
            ..DocSyncOptions::default()
        },
    ));
    gates.push(labeled_script(
        environment,
        "module-graph",
        "verify-module-graph",
        "module graph",
    ));
    gates.push(pnpm_script(environment, "knip", "knip"));
    gates.push(script_with(
        environment,
        "build",
        "build",
        None,
        &["typecheck", "lint", "doc-typecheck"],
        IndexMap::new(),
    ));
    gates.push(script_with(
        environment,
        "publint",
        "publint",
        None,
        &["build"],
        IndexMap::new(),
    ));
    gates.push(script_with(
        environment,
        "node-next-types",
        "verify-node-next-types",
        Some("node-next types"),
        &["build"],
        IndexMap::new(),
    ));
    gates.push(built_package_invariants_gate(environment, &["build"]));
    gates.push(built_bin_smoke_gate(environment, &["build"]));
    Ok(gates)
}

fn node_compat_gates(environment: &GateEnvironment) -> anyhow::Result<Vec<Gate>> {
    let include_typecheck = !flag_enabled(environment, "SEEKDEEP_NODE_COMPAT_SKIP_TYPECHECK")?;
    let mut gates = if include_typecheck {
        vec![pnpm_script(environment, "typecheck", "typecheck")]
    } else {
        Vec::new()
    };
    if environment.node_major != 22 {
        gates.extend(node_compat_smoke_gates(environment, false));
        return Ok(gates);
    }
    gates.push(script_with(
        environment,
        "build",
        "build",
        None,
        if include_typecheck {
            &["typecheck"]
        } else {
            &[]
        },
        IndexMap::new(),
    ));
    gates.push(script_with(
        environment,
        "build:web",
        "build:web",
        Some("Web frontend build"),
        &["build"],
        IndexMap::new(),
    ));
    gates.extend(node_compat_smoke_gates(environment, true));
    Ok(gates)
}

fn node_compat_smoke_gates(environment: &GateEnvironment, cli_smoke: bool) -> Vec<Gate> {
    let mut gates = vec![
        pnpm_exec(
            environment,
            "source-worker-smoke",
            &[
                "vitest",
                "run",
                "packages/workflow/workflow-worker-thread/tests/source-worker.compat.spec.ts",
            ],
            Some("source worker smoke"),
            &[],
            IndexMap::new(),
        ),
        pnpm_exec(
            environment,
            "jsonl-zstd-smoke",
            &[
                "vitest",
                "run",
                "packages/session/session-persistence-jsonl/tests/zstd.compat.spec.ts",
            ],
            Some("JSONL Zstandard smoke"),
            &[],
            IndexMap::new(),
        ),
        pnpm_exec(
            environment,
            "seekdeep-source-launch-smoke",
            &[
                "vitest",
                "run",
                "apps/cli/tests/source-launch.compat.spec.ts",
            ],
            Some("seekdeep source-launch smoke"),
            &[],
            IndexMap::new(),
        ),
        pnpm_exec(
            environment,
            "vitest-jsdom-smoke",
            &["vitest", "run", "scripts/vitest-environment.compat.spec.ts"],
            Some("Vitest jsdom smoke"),
            &[],
            IndexMap::new(),
        ),
    ];
    if cli_smoke {
        gates.push(pnpm_exec(
            environment,
            "cli-lazy-search-startup-smoke",
            &[
                "vitest",
                "run",
                "apps/cli/tests/lazy-search-startup.compat.spec.ts",
            ],
            Some("CLI lazy-search startup smoke"),
            &["build:web"],
            environment_map(&[("SEEKDEEP_REQUIRE_BUILT_CLI_SMOKE", Some("1"))]),
        ));
    }
    gates
}

fn ci_static_gates(environment: &GateEnvironment, owns_build: bool) -> Vec<Gate> {
    let mut gates = ci_shared_static_gates(environment);
    if owns_build {
        gates.push(pnpm_script(environment, "build", "build"));
    }
    gates.extend(doc_sync_leaf_gates(
        environment,
        DocSyncOptions {
            include_doc_typecheck: owns_build,
            doc_typecheck_needs: if owns_build {
                vec!["build"]
            } else {
                Vec::new()
            },
            doc_typecheck_environment: if owns_build {
                environment_map(&[("SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT", Some("1"))])
            } else {
                IndexMap::new()
            },
            doc_typecheck_script: if owns_build {
                "doc-typecheck:contracts-ready"
            } else {
                "doc-typecheck"
            },
            docs_build_script: "docs:build:mpa",
        },
    ));
    gates.push(labeled_script(
        environment,
        "module-graph",
        "verify-module-graph",
        "module graph",
    ));
    gates.push(pnpm_script(environment, "knip", "knip"));
    gates
}

fn ci_artifact_gates(environment: &GateEnvironment) -> Vec<Gate> {
    vec![
        pnpm_script(environment, "build", "build"),
        script_with(
            environment,
            "publint",
            "publint",
            None,
            &["build"],
            IndexMap::new(),
        ),
        script_with(
            environment,
            "node-next-types",
            "verify-node-next-types",
            Some("node-next types"),
            &["build"],
            IndexMap::new(),
        ),
        built_package_invariants_gate(environment, &["build"]),
        built_bin_smoke_gate(environment, &["build"]),
    ]
}

fn ci_consumer_gates(environment: &GateEnvironment) -> Vec<Gate> {
    let validated = &["built-package-invariants"];
    vec![
        pnpm_script(environment, "build", "build"),
        labeled_script(
            environment,
            "node-compat",
            "check:node-compat",
            "Node compatibility",
        ),
        script_with(
            environment,
            "publint",
            "publint",
            None,
            &["build"],
            IndexMap::new(),
        ),
        built_package_invariants_gate(environment, &["publint"]),
        script_with(
            environment,
            "lint-and-duplication",
            "check:ci:lint:contracts-ready",
            Some("lint and duplication"),
            validated,
            IndexMap::new(),
        ),
        snapshot_gate(environment, validated),
        web_snapshot_gate(environment, validated),
        script_with(
            environment,
            "doc-typecheck",
            "doc-typecheck:contracts-ready",
            None,
            validated,
            environment_map(&[("SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT", Some("1"))]),
        ),
        script_with(
            environment,
            "node-next-types",
            "verify-node-next-types",
            Some("node-next types"),
            validated,
            IndexMap::new(),
        ),
        built_bin_smoke_gate(environment, validated),
    ]
}

fn web_snapshot_gate(environment: &GateEnvironment, needs: &[&str]) -> Gate {
    let mut gate = script_with(
        environment,
        "web-snapshot",
        "test:web:built",
        Some("web browser snapshot"),
        needs,
        environment_map(&[("SEEKDEEP_SNAPSHOT", Some("replay"))]),
    );
    "SEEKDEEP_SNAPSHOT=replay pnpm run test:web:built".clone_into(&mut gate.display_command);
    gate
}

fn ci_windows_blocking_gates(environment: &GateEnvironment) -> Vec<Gate> {
    vec![
        labeled_script(environment, "windows-build", "build", "build"),
        labeled_script(environment, "windows-site", "docs:build", "production site"),
    ]
}

fn ci_windows_complete_gates(environment: &GateEnvironment) -> anyhow::Result<Vec<Gate>> {
    let observational = ci_windows_observational_gates(environment)
        .into_iter()
        .filter(|gate| !matches!(gate.id.as_str(), "build" | "docs-site-build"))
        .map(|mut gate| {
            gate.allow_failure = true;
            gate
        });
    let mut gates = vec![
        pnpm_script(environment, "build", "build"),
        labeled_script(environment, "windows-site", "docs:build", "production site"),
    ];
    gates.extend(coverage_gates(environment)?);
    gates.extend(observational);
    Ok(gates)
}

fn ci_windows_observational_gates(environment: &GateEnvironment) -> Vec<Gate> {
    let mut gates = ci_static_gates(environment, true);
    gates.push(pnpm_script(environment, "duplication", "duplication"));
    gates.push(script_with(
        environment,
        "publint",
        "publint",
        None,
        &["build"],
        IndexMap::new(),
    ));
    gates.push(script_with(
        environment,
        "node-next-types",
        "verify-node-next-types",
        Some("node-next types"),
        &["build"],
        IndexMap::new(),
    ));
    gates.push(built_package_invariants_gate(environment, &["build"]));
    gates.push(built_bin_smoke_gate(environment, &["build"]));
    gates
}

fn typert_contracts_gate(environment: &GateEnvironment) -> Gate {
    labeled_script(
        environment,
        "typert-contracts",
        "build:lib:host",
        "Typert contracts",
    )
}

fn lint_gate(environment: &GateEnvironment, needs: &[&str]) -> Gate {
    let script = "lint:contracts-ready";
    let mut gate = script_with(environment, "lint", script, None, needs, IndexMap::new());
    if let Some(raw) = environment
        .variable("SEEKDEEP_OXLINT_THREADS")
        .filter(|raw| !raw.is_empty())
    {
        gate.display_command = format!("SEEKDEEP_OXLINT_THREADS={raw} pnpm run {script}");
    }
    gate
}

fn coverage_worker_args(
    environment: &GateEnvironment,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let Some(flag) =
        positive_int_arg(environment, "SEEKDEEP_COVERAGE_MAX_WORKERS", "--maxWorkers")?
    else {
        return Ok((Vec::new(), Vec::new()));
    };
    let total = flag
        .split_once('=')
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .ok_or_else(|| anyhow::anyhow!("run-gates: invalid generated coverage worker flag"))?;
    let exempt = (total / 3).max(1);
    let instrumented = total.saturating_sub(exempt).max(1);
    Ok((
        vec![format!("--maxWorkers={instrumented}")],
        vec![format!("--maxWorkers={exempt}")],
    ))
}

fn coverage_gates(environment: &GateEnvironment) -> anyhow::Result<Vec<Gate>> {
    let (instrumented, exempt_workers) = coverage_worker_args(environment)?;
    let mut coverage_args = vec![
        "vitest".to_owned(),
        "run".to_owned(),
        "--coverage".to_owned(),
    ];
    coverage_args.extend(instrumented);
    let mut exempt_args = vec!["vitest".to_owned(), "run".to_owned()];
    exempt_args.extend(
        COVERAGE_EXEMPT_HEAVY_SUITES
            .iter()
            .map(|suite| suite.filter.to_owned()),
    );
    exempt_args.extend(exempt_workers);
    Ok(vec![
        pnpm_exec_owned(
            environment,
            "coverage",
            coverage_args,
            Some("test:coverage"),
            &[],
            environment_map(&[(COVERAGE_EXEMPT_ENV, Some("1"))]),
        ),
        pnpm_exec_owned(
            environment,
            "coverage-exempt-heavy",
            exempt_args,
            Some("test:coverage-exempt-heavy"),
            &[],
            IndexMap::new(),
        ),
    ])
}

fn snapshot_gate(environment: &GateEnvironment, needs: &[&str]) -> Gate {
    script_with(
        environment,
        "snapshot",
        "test:snapshot",
        None,
        needs,
        environment_map(&[("SEEKDEEP_EXAMPLE_MODE", Some("lib"))]),
    )
}

fn built_package_invariants_gate(environment: &GateEnvironment, needs: &[&str]) -> Gate {
    script_with(
        environment,
        "built-package-invariants",
        "verify-built-package-invariants",
        Some("built package invariants"),
        needs,
        IndexMap::new(),
    )
}

fn positive_int_arg(
    environment: &GateEnvironment,
    name: &str,
    flag: &str,
) -> anyhow::Result<Option<String>> {
    let Some(raw) = environment.variable(name).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let parsed = raw.parse::<usize>().ok().filter(|value| *value >= 1);
    if parsed.is_none() || parsed.is_some_and(|value| value.to_string() != raw) {
        anyhow::bail!(
            "run-gates: {name} must be a positive integer, got {}.",
            serde_json::to_string(raw)?
        );
    }
    Ok(Some(format!("{flag}={raw}")))
}

fn flag_enabled(environment: &GateEnvironment, name: &str) -> anyhow::Result<bool> {
    let Some(raw) = environment.variable(name).filter(|raw| !raw.is_empty()) else {
        return Ok(false);
    };
    if raw != "1" {
        anyhow::bail!(
            "run-gates: {name} must be 1 when set, got {}.",
            serde_json::to_string(raw)?
        );
    }
    Ok(true)
}

fn hygiene_leaf_gates(environment: &GateEnvironment, artifact_needs: &[&str]) -> Vec<Gate> {
    vec![
        labeled_script(
            environment,
            "rescope-vendor",
            "rescope-vendor:check",
            "vendor rescope",
        ),
        pnpm_script(environment, "knip", "knip"),
        script_with(
            environment,
            "publint",
            "publint",
            None,
            artifact_needs,
            IndexMap::new(),
        ),
        pnpm_script(environment, "constraints", "constraints"),
        labeled_script(
            environment,
            "seekdeep-package-licenses",
            "verify-seekdeep-package-licenses",
            "SeekDeep package licenses",
        ),
        labeled_script(
            environment,
            "package-invariants",
            "verify-package-invariants",
            "package invariants",
        ),
        built_package_invariants_gate(environment, artifact_needs),
        script_with(
            environment,
            "node-next-types",
            "verify-node-next-types",
            Some("node-next types"),
            artifact_needs,
            IndexMap::new(),
        ),
    ]
}

#[derive(Clone, Debug)]
struct DocSyncOptions {
    include_doc_typecheck: bool,
    doc_typecheck_needs: Vec<&'static str>,
    doc_typecheck_environment: IndexMap<String, Option<String>>,
    doc_typecheck_script: &'static str,
    docs_build_script: &'static str,
}

impl Default for DocSyncOptions {
    fn default() -> Self {
        Self {
            include_doc_typecheck: true,
            doc_typecheck_needs: Vec::new(),
            doc_typecheck_environment: IndexMap::new(),
            doc_typecheck_script: "doc-typecheck",
            docs_build_script: "docs:build",
        }
    }
}

const DOC_SYNC_SCRIPTS: &[(&str, &str, &str)] = &[
    ("cordis-catalog", "verify-cordis-catalog", "cordis catalog"),
    ("client-catalog", "verify-client-catalog", "client catalog"),
    ("export-jsdoc", "verify-export-jsdoc", "export jsdoc"),
    ("tool-catalog", "verify-tool-catalog", "tool catalog"),
    ("config-catalog", "verify-config-catalog", "config catalog"),
    (
        "persistence-catalog",
        "verify-persistence-catalog",
        "persistence catalog",
    ),
    ("doc-graphs", "verify-doc-graphs", "doc graphs"),
    ("scoped-events", "verify-scoped-events", "scoped events"),
    ("markdown-wrap", "verify-md-wrap", "markdown wrap"),
    ("markdown-links", "verify-md-links", "markdown links"),
    (
        "public-repository-links",
        "verify-public-repository-links",
        "public repository links",
    ),
    ("doc-refs", "verify-doc-refs", "doc refs"),
    ("package-paths", "verify-package-paths", "package paths"),
    (
        "config-source-ownership",
        "verify-config-source-ownership",
        "config source ownership",
    ),
    (
        "package-readme-model-experience",
        "verify-package-readme-model-experience",
        "package README model experience",
    ),
    ("mermaid", "verify-mermaid", "verify-mermaid"),
    (
        "agent-note-classification",
        "verify-agent-note-classification",
        "agent note classification",
    ),
    (
        "agent-note-format",
        "verify-agent-note-format",
        "agent note format",
    ),
    (
        "archived-agent-notes",
        "verify-archived-agent-notes",
        "archived agent notes",
    ),
    ("type-equivalence", "verify-type-equiv", "type equivalence"),
    (
        "skill-invocation-metadata",
        "verify-skill-invocation-metadata",
        "skill invocation metadata",
    ),
    (
        "translation-prompt",
        "verify-translation-prompt",
        "translation prompt",
    ),
    (
        "translation-pairing",
        "verify-translation-pairing",
        "translation pairing",
    ),
    ("doc-budgets", "verify-doc-budgets", "doc budgets"),
];

fn doc_sync_leaf_gates(environment: &GateEnvironment, options: DocSyncOptions) -> Vec<Gate> {
    let mut gates = Vec::new();
    if options.include_doc_typecheck {
        gates.push(script_with(
            environment,
            "doc-typecheck",
            options.doc_typecheck_script,
            None,
            &options.doc_typecheck_needs,
            options.doc_typecheck_environment,
        ));
    }
    for &(id, script, label) in DOC_SYNC_SCRIPTS {
        gates.push(labeled_script(environment, id, script, label));
    }
    gates.push(pnpm_exec(
        environment,
        "docs-site-projection",
        &[
            "vitest",
            "run",
            "scripts/project-doc-site.spec.ts",
            "scripts/verify-doc-site-fragments.spec.ts",
        ],
        Some("documentation site checks"),
        &[],
        IndexMap::new(),
    ));
    gates.push(labeled_script(
        environment,
        "docs-site-build",
        options.docs_build_script,
        "documentation build",
    ));
    gates.push(labeled_script(
        environment,
        "package-readme-limitations",
        "verify-package-readme-limitations",
        "package README limitations",
    ));
    gates
}

fn built_bin_smoke_gate(environment: &GateEnvironment, needs: &[&str]) -> Gate {
    pnpm_exec(
        environment,
        "built-bin-smoke",
        &[
            "vitest",
            "run",
            "--config",
            "vitest.e2e.config.ts",
            "examples/headless-agent/tests/keyless-smoke.e2e.ts",
            "apps/cli/tests/built-bin.e2e.ts",
            "packages/examples/acp-demo/tests/built-bin.e2e.ts",
            "packages/host/directory-picker-native/tests/built-worker.e2e.ts",
            "packages/sdk/server/tests/built-scope-carrier.e2e.ts",
            "packages/subagent/subagent-codex/tests/loader-composition.e2e.ts",
            "packages/subagent/subagent-claude-code/tests/loader-composition.e2e.ts",
            "packages/api/remotes/tests/built-lib.e2e.ts",
            "packages/workflow/workflow-worker-thread/tests/built-worker.e2e.ts",
            "packages/code-runtime/code-runtime-worker-thread/tests/built-lib.e2e.ts",
            "packages/lsp/lsp-stdio/tests/built-lib.e2e.ts",
        ],
        Some("built-bin smoke"),
        needs,
        environment_map(&[("SEEKDEEP_EXAMPLE_MODE", Some("lib"))]),
    )
}

fn pnpm_script(environment: &GateEnvironment, id: &str, script: &str) -> Gate {
    script_with(environment, id, script, None, &[], IndexMap::new())
}

fn labeled_script(environment: &GateEnvironment, id: &str, script: &str, label: &str) -> Gate {
    script_with(environment, id, script, Some(label), &[], IndexMap::new())
}

fn script_with(
    environment: &GateEnvironment,
    id: &str,
    script: &str,
    label: Option<&str>,
    needs: &[&str],
    gate_environment: IndexMap<String, Option<String>>,
) -> Gate {
    let mut args = vec![environment.pnpm_entrypoint.as_os_str().to_owned()];
    args.extend([OsString::from("run"), OsString::from(script)]);
    Gate {
        id: id.to_owned(),
        label: label.unwrap_or(script).to_owned(),
        display_command: format!("pnpm run {script}"),
        command: environment.node_executable.clone(),
        args,
        needs: needs.iter().map(|need| (*need).to_owned()).collect(),
        environment: gate_environment,
        allow_failure: false,
        serial_group: None,
    }
}

fn pnpm_exec(
    environment: &GateEnvironment,
    id: &str,
    arguments: &[&str],
    label: Option<&str>,
    needs: &[&str],
    gate_environment: IndexMap<String, Option<String>>,
) -> Gate {
    pnpm_exec_owned(
        environment,
        id,
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        label,
        needs,
        gate_environment,
    )
}

fn pnpm_exec_owned(
    environment: &GateEnvironment,
    id: &str,
    arguments: Vec<String>,
    label: Option<&str>,
    needs: &[&str],
    gate_environment: IndexMap<String, Option<String>>,
) -> Gate {
    let displayed_arguments = arguments.join(" ");
    let label = label.map_or_else(|| format!("pnpm exec {displayed_arguments}"), str::to_owned);
    let mut args = vec![environment.pnpm_entrypoint.as_os_str().to_owned()];
    args.push(OsString::from("exec"));
    args.extend(arguments.into_iter().map(OsString::from));
    Gate {
        id: id.to_owned(),
        label,
        display_command: format!("pnpm exec {displayed_arguments}"),
        command: environment.node_executable.clone(),
        args,
        needs: needs.iter().map(|need| (*need).to_owned()).collect(),
        environment: gate_environment,
        allow_failure: false,
        serial_group: None,
    }
}

fn environment_map(entries: &[(&str, Option<&str>)]) -> IndexMap<String, Option<String>> {
    entries
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
        .collect()
}

/// Marks Cargo-backed package-script gates as mutually exclusive on `target/`.
///
/// The source scheduler's independent TypeScript leaves could overlap freely.
/// During the Rust port, package scripts progressively become Cargo commands;
/// this guard preserves graph parallelism without launching multiple Cargo
/// clients against the same build directory.
///
/// # Errors
///
/// Returns package-manifest read, JSON, or script-shape failures.
pub fn assign_cargo_serial_group(root: &Path, gates: &mut [Gate]) -> anyhow::Result<()> {
    let manifest = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(
        root.join("package.json"),
    )?)?;
    let scripts = manifest
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("package.json: scripts must be an object"))?;
    for gate in gates {
        let script = gate
            .args
            .get(1)
            .filter(|argument| argument.as_os_str() == std::ffi::OsStr::new("run"))
            .and_then(|_| gate.args.get(2))
            .and_then(|argument| argument.to_str());
        if script.is_some_and(|script| script_uses_cargo(script, scripts, &mut HashSet::new())) {
            gate.serial_group = Some("cargo-target".to_owned());
        }
    }
    Ok(())
}

fn script_uses_cargo(
    name: &str,
    scripts: &serde_json::Map<String, serde_json::Value>,
    seen: &mut HashSet<String>,
) -> bool {
    static CARGO: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:^|[;&|()\s])cargo(?:[.](?:exe|cmd))?(?:$|\s)")
            .expect("static Cargo command regex")
    });
    static NESTED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:^|[;&|()\s])(?:pnpm|npm)\s+run\s+([A-Za-z0-9:_-]+)")
            .expect("static nested package-script regex")
    });
    if !seen.insert(name.to_owned()) {
        return false;
    }
    let Some(command) = scripts.get(name).and_then(serde_json::Value::as_str) else {
        return false;
    };
    CARGO.is_match(command)
        || NESTED.captures_iter(command).any(|captures| {
            captures
                .get(1)
                .is_some_and(|nested| script_uses_cargo(nested.as_str(), scripts, seen))
        })
}

/// Validates a graph before any executor can start.
///
/// # Errors
///
/// Returns empty, duplicate, unknown-dependency, or cycle diagnostics.
pub fn validate_gate_graph(gates: &[Gate]) -> anyhow::Result<()> {
    if gates.is_empty() {
        anyhow::bail!("run-gates: gate graph has no gates.");
    }
    let mut ids = HashSet::new();
    for gate in gates {
        if !ids.insert(gate.id.clone()) {
            anyhow::bail!(
                "run-gates: duplicate gate id {}.",
                serde_json::to_string(&gate.id)?
            );
        }
    }
    for gate in gates {
        for dependency in &gate.needs {
            if !ids.contains(dependency) {
                anyhow::bail!(
                    "run-gates: gate {} depends on unknown gate {}.",
                    serde_json::to_string(&gate.id)?,
                    serde_json::to_string(dependency)?
                );
            }
        }
    }
    if let Some(cycle) = find_dependency_cycle(gates) {
        anyhow::bail!("run-gates: dependency cycle: {}.", cycle.join(" -> "));
    }
    Ok(())
}

fn find_dependency_cycle(gates: &[Gate]) -> Option<Vec<String>> {
    fn visit(
        id: &str,
        by_id: &HashMap<&str, &Gate>,
        complete: &mut HashSet<String>,
        active: &mut HashMap<String, usize>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if complete.contains(id) {
            return None;
        }
        if let Some(start) = active.get(id) {
            let mut cycle = path[*start..].to_vec();
            cycle.push(id.to_owned());
            return Some(cycle);
        }
        let gate = by_id.get(id)?;
        active.insert(id.to_owned(), path.len());
        path.push(id.to_owned());
        for dependency in &gate.needs {
            if let Some(cycle) = visit(dependency, by_id, complete, active, path) {
                return Some(cycle);
            }
        }
        path.pop();
        active.remove(id);
        complete.insert(id.to_owned());
        None
    }

    let by_id = gates
        .iter()
        .map(|gate| (gate.id.as_str(), gate))
        .collect::<HashMap<_, _>>();
    let mut complete = HashSet::new();
    let mut active = HashMap::new();
    let mut path = Vec::new();
    for gate in gates {
        if let Some(cycle) = visit(&gate.id, &by_id, &mut complete, &mut active, &mut path) {
            return Some(cycle);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateState {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// Validates and executes a graph with bounded concurrency.
///
/// Results retain aggregate order even though observers see completion order.
///
/// # Errors
///
/// Returns graph, concurrency, executor-channel, stall, or missing-result errors.
pub fn run_gates<E, S, O>(
    gates: Vec<Gate>,
    max_active: usize,
    execute: E,
    mut started: S,
    mut observe: O,
) -> anyhow::Result<Vec<GateResult>>
where
    E: Fn(Gate) -> GateResult + Send + Sync + 'static,
    S: FnMut(&Gate),
    O: FnMut(&GateResult),
{
    validate_gate_graph(&gates)?;
    if max_active < 1 {
        anyhow::bail!(
            "run-gates: max concurrency must be a positive integer, got {}.",
            serde_json::to_string(&max_active)?
        );
    }
    let gates = Arc::<[Gate]>::from(gates);
    let execute = Arc::new(execute);
    let (sender, receiver) = mpsc::channel::<(usize, GateResult)>();
    let mut states = vec![GateState::Pending; gates.len()];
    let mut results = (0..gates.len()).map(|_| None).collect::<Vec<_>>();
    let mut running = 0_usize;
    let mut running_groups = HashSet::<String>::new();
    loop {
        while running < max_active {
            let ready = gates.iter().enumerate().find(|(index, gate)| {
                states[*index] == GateState::Pending
                    && gate
                        .serial_group
                        .as_ref()
                        .is_none_or(|group| !running_groups.contains(group))
                    && gate.needs.iter().all(|dependency| {
                        gate_index(&gates, dependency)
                            .is_some_and(|dependency| states[dependency] == GateState::Passed)
                    })
            });
            let Some((index, gate)) = ready else {
                break;
            };
            states[index] = GateState::Running;
            running += 1;
            if let Some(group) = &gate.serial_group {
                running_groups.insert(group.clone());
            }
            started(gate);
            let gate = gate.clone();
            let execute = Arc::clone(&execute);
            let sender = sender.clone();
            std::thread::spawn(move || {
                let result = execute(gate);
                let _ = sender.send((index, result));
            });
        }
        if running > 0 {
            let (index, result) = receiver
                .recv()
                .map_err(|_| anyhow::anyhow!("run-gates: executor channel closed"))?;
            running -= 1;
            if let Some(group) = &gates[index].serial_group {
                running_groups.remove(group);
            }
            states[index] = match result.status {
                GateResultStatus::Passed => GateState::Passed,
                GateResultStatus::Failed => GateState::Failed,
                GateResultStatus::Skipped => GateState::Skipped,
            };
            observe(&result);
            results[index] = Some(result);
            continue;
        }
        skip_failed_dependents(&gates, &mut states, &mut results, &mut observe)?;
        break;
    }
    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.ok_or_else(|| {
                anyhow::anyhow!("run-gates: missing result for {}.", gates[index].id)
            })
        })
        .collect()
}

fn skip_failed_dependents<O>(
    gates: &[Gate],
    states: &mut [GateState],
    results: &mut [Option<GateResult>],
    observe: &mut O,
) -> anyhow::Result<()>
where
    O: FnMut(&GateResult),
{
    let mut pending = states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| (*state == GateState::Pending).then_some(index))
        .collect::<Vec<_>>();
    while !pending.is_empty() {
        let skipped = pending.iter().position(|index| {
            gates[*index].needs.iter().any(|dependency| {
                gate_index(gates, dependency).is_some_and(|dependency| {
                    matches!(states[dependency], GateState::Failed | GateState::Skipped)
                })
            })
        });
        let Some(position) = skipped else {
            anyhow::bail!("run-gates: validated graph stalled without a failed dependency.");
        };
        let index = pending.remove(position);
        let failed = gates[index]
            .needs
            .iter()
            .filter(|dependency| {
                gate_index(gates, dependency).is_some_and(|dependency| {
                    matches!(states[dependency], GateState::Failed | GateState::Skipped)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let result = GateResult {
            gate: gates[index].clone(),
            status: GateResultStatus::Skipped,
            duration: Duration::ZERO,
            output: Vec::new(),
            exit_code: None,
            signal_code: None,
            error: Some(format!(
                "dependency failed or skipped: {}",
                failed.join(", ")
            )),
        };
        states[index] = GateState::Skipped;
        observe(&result);
        results[index] = Some(result);
    }
    Ok(())
}

fn gate_index(gates: &[Gate], id: &str) -> Option<usize> {
    gates.iter().position(|gate| gate.id == id)
}

/// Executes one gate through a shell-free child-process boundary.
#[must_use]
pub fn run_gate(root: &Path, inherited: &BTreeMap<OsString, OsString>, gate: Gate) -> GateResult {
    let started = Instant::now();
    let mut command = Command::new(&gate.command);
    command
        .args(&gate.args)
        .current_dir(root)
        .env_clear()
        .envs(inherited)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in &gate.environment {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return GateResult {
                gate,
                status: GateResultStatus::Failed,
                duration: started.elapsed(),
                output: Vec::new(),
                exit_code: None,
                signal_code: None,
                error: Some(format!("failed to start command: {error}")),
            };
        }
    };
    let (sender, receiver) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        spawn_output_reader(stdout, GateOutputStream::Stdout, sender.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_output_reader(stderr, GateOutputStream::Stderr, sender.clone());
    }
    drop(sender);
    let status = child.wait();
    let output = receiver.into_iter().collect::<Vec<_>>();
    match status {
        Ok(status) => {
            let exit_code = status.code();
            let signal_code = signal_name(status);
            GateResult {
                gate,
                status: if exit_code == Some(0) && signal_code.is_none() {
                    GateResultStatus::Passed
                } else {
                    GateResultStatus::Failed
                },
                duration: started.elapsed(),
                output,
                exit_code,
                signal_code,
                error: None,
            }
        }
        Err(error) => GateResult {
            gate,
            status: GateResultStatus::Failed,
            duration: started.elapsed(),
            output,
            exit_code: None,
            signal_code: None,
            error: Some(format!("failed to wait for command: {error}")),
        },
    }
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    stream: GateOutputStream,
    sender: mpsc::Sender<GateOutputChunk>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(length) => {
                    if sender
                        .send(GateOutputChunk {
                            stream,
                            text: String::from_utf8_lossy(&buffer[..length]).into_owned(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(unix)]
fn signal_name(status: ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt as _;

    let signal = status.signal()?;
    nix::sys::signal::Signal::try_from(signal)
        .ok()
        .map(|signal| format!("{signal:?}"))
        .or_else(|| Some(format!("SIG{signal}")))
}

#[cfg(not(unix))]
fn signal_name(_status: ExitStatus) -> Option<String> {
    None
}

/// Formats every independently observed failure fact.
#[must_use]
pub fn format_gate_result_reason(result: &GateResult) -> String {
    let mut facts = Vec::new();
    if let Some(error) = &result.error {
        facts.push(error.clone());
    }
    if let Some(exit) = result.exit_code {
        facts.push(format!("exit {exit}"));
    }
    if let Some(signal) = &result.signal_code {
        facts.push(format!("signal {signal}"));
    }
    if facts.is_empty() {
        "no exit code or signal".to_owned()
    } else {
        facts.join(", ")
    }
}

/// Prints one settled result with source-compatible concise/verbose behavior.
pub fn print_gate_result(result: &GateResult, verbose: bool) {
    let seconds = result.duration.as_secs_f64();
    if result.status == GateResultStatus::Passed && !verbose {
        println!("run-gates: PASS {} ({seconds:.2}s)", result.gate.label);
        return;
    }
    let status = status_label(result.status);
    let heading = format!("{status} {} ({seconds:.2}s)", result.gate.label);
    if result.status == GateResultStatus::Passed {
        println!("\n== {heading} ==");
    } else {
        eprintln!("\n== {heading} ==");
        eprintln!("command: {}", result.gate.display_command);
        eprintln!("outcome: {}", format_gate_result_reason(result));
    }
    print_output(&result.output);
}

/// Prints aggregate counts and unsuccessful-gate details.
pub fn print_gate_summary(results: &[GateResult], duration: Duration) {
    let passed = results
        .iter()
        .filter(|result| result.status == GateResultStatus::Passed)
        .count();
    let failed = results
        .iter()
        .filter(|result| result.status == GateResultStatus::Failed)
        .count();
    let skipped = results
        .iter()
        .filter(|result| result.status == GateResultStatus::Skipped)
        .count();
    println!(
        "\nrun-gates: {passed} passed, {failed} failed, {skipped} skipped in {:.2}s.",
        duration.as_secs_f64()
    );
    let unsuccessful = results
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                GateResultStatus::Failed | GateResultStatus::Skipped
            )
        })
        .collect::<Vec<_>>();
    if unsuccessful.is_empty() {
        return;
    }
    eprintln!("run-gates: unsuccessful gates:");
    for result in unsuccessful {
        let disposition = if result.gate.allow_failure {
            "NON-BLOCKING "
        } else {
            ""
        };
        eprintln!(
            "  - {disposition}{} {} ({:.2}s, {})",
            status_label(result.status),
            result.gate.label,
            result.duration.as_secs_f64(),
            format_gate_result_reason(result)
        );
        eprintln!("    {}", result.gate.display_command);
    }
}

fn print_output(output: &[GateOutputChunk]) {
    use std::io::Write as _;

    for chunk in output {
        match chunk.stream {
            GateOutputStream::Stdout => {
                let _ = std::io::stdout().write_all(chunk.text.as_bytes());
            }
            GateOutputStream::Stderr => {
                let _ = std::io::stderr().write_all(chunk.text.as_bytes());
            }
        }
    }
}

fn status_label(status: GateResultStatus) -> &'static str {
    match status {
        GateResultStatus::Passed => "PASSED",
        GateResultStatus::Failed => "FAILED",
        GateResultStatus::Skipped => "SKIPPED",
    }
}

/// Runs one CLI aggregate and returns its process status.
///
/// # Errors
///
/// Returns mode, environment, graph, concurrency, or scheduler diagnostics.
pub fn run_gate_cli(root: &Path, arguments: &[String]) -> anyhow::Result<u8> {
    let mode = GateMode::parse(arguments.first().map(String::as_str))?;
    let environment = GateEnvironment::from_process()?;
    let mut gates = gates_for_mode(mode, &environment)?;
    assign_cargo_serial_group(root, &mut gates)?;
    let default = default_concurrency(mode, gates.len(), environment.available_parallelism);
    let (workers, override_source) = concurrency_from_environment(&environment, default.workers)?;
    let source = if override_source.is_empty() {
        default.source
    } else {
        override_source
    };
    println!(
        "run-gates: {} running {} gate(s) with {workers} worker(s) from {source}.",
        mode.as_str(),
        gates.len()
    );
    let started_at = Instant::now();
    let inherited = environment.variables.clone();
    let root = root.to_owned();
    let verbose = environment.variable("SEEKDEEP_GATE_VERBOSE") == Some("1");
    let results = run_gates(
        gates,
        workers,
        move |gate| run_gate(&root, &inherited, gate),
        |gate| println!("run-gates: start {}", gate.label),
        |result| print_gate_result(result, verbose),
    )?;
    print_gate_summary(&results, started_at.elapsed());
    Ok(u8::from(results.iter().any(|result| {
        !result.gate.allow_failure
            && matches!(
                result.status,
                GateResultStatus::Failed | GateResultStatus::Skipped
            )
    })))
}
