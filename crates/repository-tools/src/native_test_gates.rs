//! Executes the source-selected smoke and heavy suites through compiled Rust entrypoints.

use std::{
    ffi::OsString,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Context as _;
use clap::ValueEnum;
use serde_json::Value;

/// Repository suite inventory selected by a public aggregate command.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum NativeTestGate {
    /// Compiled CLI, worker, SDK, Remotes, and LSP smoke tests.
    BuiltBin,
    /// Compiler and process suites excluded from instrumentation, but still required.
    CoverageExemptHeavy,
}

/// One source suite and its executable Rust replacement.
#[derive(Clone, Debug)]
pub struct NativeSuite {
    /// Original suite path or source-defined suite directory.
    pub source: &'static str,
    /// Command exercising the source suite.
    pub command: NativeTestCommand,
}

/// Compiled entrypoint used by a suite.
#[derive(Clone, Debug)]
pub enum NativeTestCommand {
    /// Cargo arguments, excluding the executable name.
    Cargo(Vec<&'static str>),
    /// Build and execute the downstream LSP consumer with its real fixture server.
    LspBuiltConsumer,
}

fn tests(package: &'static str, targets: &[&'static str]) -> NativeTestCommand {
    let mut args = vec![
        "test",
        "--locked",
        "--no-fail-fast",
        "--all-features",
        "--package",
        package,
    ];
    for target in targets {
        args.extend(["--test", target]);
    }
    NativeTestCommand::Cargo(args)
}

fn suite(source: &'static str, command: NativeTestCommand) -> NativeSuite {
    NativeSuite { source, command }
}

/// Every source-selected suite remains represented, including explicit platform guards in its tests.
#[must_use]
pub fn native_suites(gate: NativeTestGate) -> Vec<NativeSuite> {
    match gate {
        NativeTestGate::BuiltBin => vec![
            suite(
                "examples/headless-agent/tests/keyless-smoke.e2e.ts",
                tests("seekdeep-headless", &["keyless_loader_smoke"]),
            ),
            suite(
                "apps/cli/tests/built-bin.e2e.ts",
                tests(
                    "seekdeep",
                    &[
                        "dump_config_process",
                        "headless_process",
                        "layered_env_process",
                        "plugin_process",
                        "source_launch_compat",
                    ],
                ),
            ),
            suite(
                "packages/examples/acp-demo/tests/built-bin.e2e.ts",
                tests("seekdeep-acp-demo", &["acp_demo_parity"]),
            ),
            suite(
                "packages/host/directory-picker-native/tests/built-worker.e2e.ts",
                tests(
                    "seekdeep-host-directory-picker-native",
                    &["win32_dialog_parity"],
                ),
            ),
            suite(
                "packages/sdk/server/tests/built-scope-carrier.e2e.ts",
                tests("seekdeep-sdk-server", &["server_parity"]),
            ),
            suite(
                "packages/subagent/subagent-codex/tests/loader-composition.e2e.ts",
                tests("seekdeep-subagent-codex", &["loader_composition"]),
            ),
            suite(
                "packages/subagent/subagent-claude-code/tests/loader-composition.e2e.ts",
                tests("seekdeep-subagent-claude-code", &["loader_composition"]),
            ),
            suite(
                "packages/api/remotes/tests/built-lib.e2e.ts",
                NativeTestCommand::Cargo(vec!["xtask", "remote-built-smoke"]),
            ),
            suite(
                "packages/workflow/workflow-worker-thread/tests/built-worker.e2e.ts",
                tests("seekdeep-workflow-worker-thread", &["built_worker_e2e"]),
            ),
            suite(
                "packages/code-runtime/code-runtime-worker-thread/tests/built-lib.e2e.ts",
                tests(
                    "seekdeep-code-runtime-worker-thread",
                    &["packaged_node_runtime", "node_api_parity"],
                ),
            ),
            suite(
                "packages/lsp/lsp-stdio/tests/built-lib.e2e.ts",
                NativeTestCommand::LspBuiltConsumer,
            ),
        ],
        NativeTestGate::CoverageExemptHeavy => vec![
            suite(
                "packages/typert/generator/tests/",
                NativeTestCommand::Cargo(vec!["xtask", "typert-corpus"]),
            ),
            suite(
                "scripts/install-lefthook.spec.ts",
                tests("seekdeep-repository-tools", &["lefthook_installer_parity"]),
            ),
            suite(
                "scripts/oxlint-contract.spec.ts",
                tests("seekdeep-repository-tools", &["run_oxlint_parity"]),
            ),
            suite(
                "scripts/change-scope.spec.ts",
                tests("seekdeep-change-scope", &["change_scope_parity"]),
            ),
        ],
    }
}

fn cargo(root: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root);
    command
}

/// Execute all selected suites, retaining failures while the remaining independent suites run.
///
/// # Errors
/// Returns failed compiler, test, process, or artifact checks after the inventory finishes.
pub fn run_native_test_gate(
    root: &Path,
    gate: NativeTestGate,
    workers: Option<NonZeroUsize>,
) -> anyhow::Result<()> {
    let suites = native_suites(gate);
    let mut failures = Vec::new();
    for suite in &suites {
        println!("native test gate: {}", suite.source);
        if let Err(error) = run_native_test_command(root, &suite.command, workers) {
            eprintln!("native test gate: {} failed: {error:#}", suite.source);
            failures.push(format!("{}: {error:#}", suite.source));
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "native test gate: {} of {} suites failed:\n{}",
        failures.len(),
        suites.len(),
        failures.join("\n")
    );
    println!("native test gate: {} source suites passed", suites.len());
    Ok(())
}

/// Run a compiled suite command with an optional test-thread ceiling.
///
/// # Errors
/// Rejects failed commands, missing emitted executables, and invalid smoke results.
pub fn run_native_test_command(
    root: &Path,
    command: &NativeTestCommand,
    workers: Option<NonZeroUsize>,
) -> anyhow::Result<()> {
    match command {
        NativeTestCommand::Cargo(arguments) => {
            let mut process = cargo(root);
            process.args(arguments);
            if arguments.first() == Some(&"test")
                && let Some(workers) = workers
            {
                process.arg("--").arg(format!("--test-threads={workers}"));
            }
            let status = process.status().context("execute compiled suite")?;
            anyhow::ensure!(
                status.success(),
                "cargo {} exited with {status}",
                arguments.join(" ")
            );
            Ok(())
        }
        NativeTestCommand::LspBuiltConsumer => run_lsp_built_consumer(root),
    }
}

fn run_lsp_built_consumer(root: &Path) -> anyhow::Result<()> {
    let output = cargo(root)
        .args([
            "build",
            "--locked",
            "--all-features",
            "--package",
            "seekdeep-lsp-stdio",
            "--bin",
            "seekdeep-lsp-stdio-fixture",
            "--example",
            "built_smoke",
            "--message-format=json-render-diagnostics",
        ])
        .stderr(Stdio::inherit())
        .output()
        .context("build LSP consumer and fixture")?;
    anyhow::ensure!(
        output.status.success(),
        "LSP artifact build failed ({})",
        output.status
    );
    let messages = String::from_utf8(output.stdout)?;
    let executable = |name: &str| -> anyhow::Result<PathBuf> {
        messages
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|message| {
                (message["reason"] == "compiler-artifact" && message["target"]["name"] == name)
                    .then(|| message["executable"].as_str().map(PathBuf::from))
                    .flatten()
            })
            .filter(|path| path.is_file())
            .with_context(|| format!("Cargo did not emit the {name} executable"))
    };
    let fixture = executable("seekdeep-lsp-stdio-fixture")?;
    let consumer = executable("built_smoke")?;
    let output = Command::new(consumer)
        .arg(fixture)
        .current_dir(root)
        .env_remove("NODE_PATH")
        .env_remove("NODE_OPTIONS")
        .output()
        .context("execute built LSP consumer")?;
    anyhow::ensure!(
        output.status.success(),
        "built LSP consumer failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let result: Value = serde_json::from_str(
        stdout
            .lines()
            .last()
            .context("built LSP consumer emitted no result")?,
    )?;
    anyhow::ensure!(
        result["kind"] == "locations"
            && result["locations"]
                .as_array()
                .is_some_and(|locations| locations.len() == 1),
        "built LSP consumer did not return one normalized location: {result}"
    );
    print!("{stdout}");
    Ok(())
}

/// Arguments accepted by the public native gate entrypoint.
#[derive(Debug, clap::Parser)]
pub struct NativeTestArguments {
    /// Source suite inventory to execute.
    #[arg(value_enum)]
    pub gate: NativeTestGate,
    /// Maximum Rust test threads; the Typert corpus remains serialized.
    #[arg(long = "maxWorkers", alias = "max-workers")]
    pub workers: Option<NonZeroUsize>,
}

/// Parse a native gate command without starting any process.
///
/// # Errors
/// Rejects unknown inventories and non-positive worker limits.
pub fn parse_native_test_arguments(
    arguments: &[OsString],
) -> Result<NativeTestArguments, clap::Error> {
    use clap::Parser as _;
    NativeTestArguments::try_parse_from(
        std::iter::once(OsString::from("run-native-test-gate")).chain(arguments.iter().cloned()),
    )
}
