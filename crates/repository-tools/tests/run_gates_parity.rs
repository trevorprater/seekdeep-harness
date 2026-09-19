//! Graph membership, dependency scheduling, process outcomes, and target rename parity.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use indexmap::IndexMap;
use seekdeep_repository_tools::run_gates::{
    ConcurrencyDefault, Gate, GateEnvironment, GateMode, GateResult, GateResultStatus,
    assign_cargo_serial_group, default_concurrency, gates_for_mode, run_gates, validate_gate_graph,
};

fn environment() -> GateEnvironment {
    GateEnvironment {
        variables: BTreeMap::new(),
        node_executable: PathBuf::from("/node"),
        pnpm_entrypoint: PathBuf::from("/private/pnpm.cjs"),
        node_major: 24,
        available_parallelism: 4,
    }
}

fn gate(id: &str, needs: &[&str]) -> Gate {
    Gate {
        id: id.to_owned(),
        label: id.to_owned(),
        display_command: format!("run {id}"),
        command: PathBuf::from("/bin/true"),
        args: Vec::new(),
        needs: needs.iter().map(|need| (*need).to_owned()).collect(),
        environment: IndexMap::new(),
        allow_failure: false,
        serial_group: None,
    }
}

fn result(gate: Gate, status: GateResultStatus) -> GateResult {
    GateResult {
        gate,
        status,
        duration: Duration::from_millis(10),
        output: Vec::new(),
        exit_code: (status == GateResultStatus::Passed)
            .then_some(0)
            .or(Some(1)),
        signal_code: None,
        error: None,
    }
}

#[test]
fn every_mode_constructs_a_valid_nonempty_graph() {
    for (mode, expected) in [
        (GateMode::CiPrimary, 47),
        (GateMode::CiLinuxPrimary, 48),
        (GateMode::CiStatic, 34),
        (GateMode::CiLintContractsReady, 2),
        (GateMode::CiCoverage, 2),
        (GateMode::CiSnapshot, 2),
        (GateMode::CiArtifacts, 5),
        (GateMode::CiConsumers, 10),
        (GateMode::CiWindowsBlocking, 2),
        (GateMode::CiWindowsComplete, 43),
        (GateMode::CiWindowsObservational, 41),
        (GateMode::NodeCompat, 4),
        (GateMode::CheckAll, 45),
        (GateMode::DocSync, 28),
    ] {
        let gates = gates_for_mode(mode, &environment()).unwrap();
        assert_eq!(gates.len(), expected, "{}", mode.as_str());
        validate_gate_graph(&gates).unwrap();
        let total = gates.len();
        let results = run_gates(
            gates,
            total,
            |gate| result(gate, GateResultStatus::Passed),
            |_| {},
            |_| {},
        )
        .unwrap();
        assert_eq!(results.len(), total);
    }
}

#[test]
fn documentation_site_gate_runs_all_ported_suites_and_owns_the_cargo_target() {
    let gates = gates_for_mode(GateMode::DocSync, &environment()).unwrap();
    let gate = gates
        .iter()
        .find(|gate| gate.id == "docs-site-projection")
        .unwrap();
    assert_eq!(gate.command, PathBuf::from("cargo"));
    assert_eq!(
        gate.args,
        [
            "test",
            "--locked",
            "-p",
            "seekdeep-repository-tools",
            "--all-features",
            "--test",
            "doc_site_projection_parity",
            "--test",
            "doc_site_configuration_parity",
            "--test",
            "doc_site_fragments_parity",
        ]
        .map(OsString::from)
    );
    assert_eq!(gate.serial_group.as_deref(), Some("cargo-target"));
    assert!(!gate.allow_failure);
}

#[test]
fn graph_validation_rejects_empty_duplicates_unknown_dependencies_and_cycles() {
    assert!(
        validate_gate_graph(&[])
            .unwrap_err()
            .to_string()
            .contains("no gates")
    );
    let error = validate_gate_graph(&[gate("same", &[]), gate("same", &[])])
        .unwrap_err()
        .to_string();
    assert!(error.contains("duplicate gate id \"same\""));
    let error = validate_gate_graph(&[gate("subject", &["missing"])])
        .unwrap_err()
        .to_string();
    assert!(error.contains("depends on unknown gate \"missing\""));
    let error = validate_gate_graph(&[gate("first", &["second"]), gate("second", &["first"])])
        .unwrap_err()
        .to_string();
    assert!(error.contains("dependency cycle: first -> second -> first"));
}

#[test]
fn invalid_concurrency_starts_no_executor_and_failed_roots_skip_dependents() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_executor = Arc::clone(&calls);
    let error = run_gates(
        vec![gate("subject", &[])],
        0,
        move |gate| {
            calls_for_executor.fetch_add(1, Ordering::SeqCst);
            result(gate, GateResultStatus::Passed)
        },
        |_| {},
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("max concurrency must be a positive integer"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_executor = Arc::clone(&calls);
    let results = run_gates(
        vec![gate("dependent", &["root"]), gate("root", &[])],
        1,
        move |gate| {
            calls_for_executor.fetch_add(1, Ordering::SeqCst);
            result(gate, GateResultStatus::Failed)
        },
        |_| {},
        |_| {},
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(results[0].status, GateResultStatus::Skipped);
    assert_eq!(
        results[0].error.as_deref(),
        Some("dependency failed or skipped: root")
    );
}

#[test]
fn bounded_scheduler_never_exceeds_worker_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let active_for_executor = Arc::clone(&active);
    let peak_for_executor = Arc::clone(&peak);
    let gates = (0..8)
        .map(|index| gate(&format!("gate-{index}"), &[]))
        .collect::<Vec<_>>();
    let results = run_gates(
        gates,
        2,
        move |gate| {
            let current = active_for_executor.fetch_add(1, Ordering::SeqCst) + 1;
            peak_for_executor.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            active_for_executor.fetch_sub(1, Ordering::SeqCst);
            result(gate, GateResultStatus::Passed)
        },
        |_| {},
        |_| {},
    )
    .unwrap();
    assert_eq!(results.len(), 8);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}

#[test]
fn cargo_backed_package_scripts_share_one_runtime_resource_group() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("package.json"),
        r#"{"scripts":{"cargo-leaf":"cargo test","nested":"pnpm run cargo-leaf","plain":"node plain.js"}}
"#,
    )
    .unwrap();
    let mut gates = [gate("cargo", &[]), gate("nested", &[]), gate("plain", &[])];
    for (gate, script) in gates.iter_mut().zip(["cargo-leaf", "nested", "plain"]) {
        gate.args = vec![
            OsString::from("/private/pnpm.cjs"),
            OsString::from("run"),
            OsString::from(script),
        ];
    }
    assign_cargo_serial_group(root.path(), &mut gates).unwrap();
    assert_eq!(gates[0].serial_group.as_deref(), Some("cargo-target"));
    assert_eq!(gates[1].serial_group.as_deref(), Some("cargo-target"));
    assert_eq!(gates[2].serial_group, None);

    let cargo_active = Arc::new(AtomicUsize::new(0));
    let cargo_peak = Arc::new(AtomicUsize::new(0));
    let active = Arc::clone(&cargo_active);
    let peak = Arc::clone(&cargo_peak);
    let results = run_gates(
        gates.to_vec(),
        3,
        move |gate| {
            if gate.serial_group.is_some() {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                active.fetch_sub(1, Ordering::SeqCst);
            }
            result(gate, GateResultStatus::Passed)
        },
        |_| {},
        |_| {},
    )
    .unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(cargo_peak.load(Ordering::SeqCst), 1);
}

#[test]
fn documentation_and_license_policies_remain_in_all_owning_graphs() {
    let docs = gates_for_mode(GateMode::DocSync, &environment()).unwrap();
    assert!(docs.iter().any(|gate| gate.id == "public-repository-links"));
    for mode in [GateMode::CiPrimary, GateMode::CiStatic, GateMode::CheckAll] {
        let gates = gates_for_mode(mode, &environment()).unwrap();
        assert!(
            gates
                .iter()
                .any(|gate| gate.id == "seekdeep-package-licenses")
        );
    }
}

#[test]
fn windows_blocking_and_observational_dispositions_remain_distinct() {
    let gates = gates_for_mode(GateMode::CiWindowsComplete, &environment()).unwrap();
    let coverage = gates.iter().find(|gate| gate.id == "coverage").unwrap();
    let exempt = gates
        .iter()
        .find(|gate| gate.id == "coverage-exempt-heavy")
        .unwrap();
    let duplication = gates.iter().find(|gate| gate.id == "duplication").unwrap();
    assert!(!coverage.allow_failure);
    assert!(!exempt.allow_failure);
    assert!(duplication.allow_failure);
}

#[test]
fn coverage_gate_uses_the_public_compiled_reporter_entry_and_worker_budget() {
    let mut environment = environment();
    environment.variables.insert(
        OsString::from("SEEKDEEP_COVERAGE_MAX_WORKERS"),
        OsString::from("9"),
    );
    let gates = gates_for_mode(GateMode::CiCoverage, &environment).unwrap();
    let coverage = gates.iter().find(|gate| gate.id == "coverage").unwrap();
    assert_eq!(
        coverage.args,
        [
            "/private/pnpm.cjs",
            "run",
            "test:coverage",
            "--maxWorkers=6"
        ]
        .map(OsString::from)
    );
    assert_eq!(
        coverage.display_command,
        "pnpm run test:coverage --maxWorkers=6"
    );
    assert!(
        coverage
            .environment
            .values()
            .any(|value| value.as_deref() == Some("1"))
    );
    let exempt = gates
        .iter()
        .find(|gate| gate.id == "coverage-exempt-heavy")
        .unwrap();
    assert_eq!(
        exempt.args[..3],
        ["/private/pnpm.cjs", "run", "test:coverage-exempt-heavy"].map(OsString::from)
    );
    assert!(exempt.args.contains(&OsString::from("--maxWorkers=3")));
    assert!(!exempt.args.contains(&OsString::from("--coverage")));
}

#[test]
fn lint_and_typert_consumers_preserve_command_and_dependency_contracts() {
    let lint = gates_for_mode(GateMode::CiLintContractsReady, &environment())
        .unwrap()
        .remove(0);
    assert_eq!(lint.id, "lint");
    assert_eq!(lint.display_command, "pnpm run lint:contracts-ready");
    assert_eq!(lint.command, PathBuf::from("/node"));
    assert_eq!(
        lint.args,
        ["/private/pnpm.cjs", "run", "lint:contracts-ready"].map(OsString::from)
    );

    let mut bounded = environment();
    bounded
        .variables
        .insert("SEEKDEEP_OXLINT_THREADS".into(), "4".into());
    let lint = gates_for_mode(GateMode::CiLintContractsReady, &bounded)
        .unwrap()
        .remove(0);
    assert_eq!(
        lint.display_command,
        "SEEKDEEP_OXLINT_THREADS=4 pnpm run lint:contracts-ready"
    );
    assert_eq!(
        lint.args,
        ["/private/pnpm.cjs", "run", "lint:contracts-ready"].map(OsString::from)
    );

    let primary = gates_for_mode(GateMode::CiPrimary, &environment()).unwrap();
    let contracts = primary
        .iter()
        .find(|gate| gate.id == "typert-contracts")
        .unwrap();
    assert_eq!(contracts.display_command, "pnpm run build:lib:host");
    assert_eq!(
        contracts.args,
        ["/private/pnpm.cjs", "run", "build:lib:host"].map(OsString::from)
    );
    for (id, script) in [
        ("typecheck", "typecheck:contracts-ready"),
        ("lint", "lint:contracts-ready"),
        ("doc-typecheck", "doc-typecheck:contracts-ready"),
    ] {
        let gate = primary.iter().find(|gate| gate.id == id).unwrap();
        assert_eq!(gate.display_command, format!("pnpm run {script}"));
        assert_eq!(
            gate.args,
            ["/private/pnpm.cjs", "run", script].map(OsString::from)
        );
        assert_eq!(gate.needs, ["typert-contracts"]);
    }
    assert_eq!(
        primary
            .iter()
            .find(|gate| gate.id == "build")
            .unwrap()
            .needs,
        ["typecheck", "lint", "doc-typecheck"]
    );

    let consumers = gates_for_mode(GateMode::CiConsumers, &environment()).unwrap();
    for (id, script) in [
        ("lint-and-duplication", "check:ci:lint:contracts-ready"),
        ("doc-typecheck", "doc-typecheck:contracts-ready"),
    ] {
        let gate = consumers.iter().find(|gate| gate.id == id).unwrap();
        assert_eq!(gate.display_command, format!("pnpm run {script}"));
        assert_eq!(
            gate.args,
            ["/private/pnpm.cjs", "run", script].map(OsString::from)
        );
    }
}

#[test]
fn doc_sync_and_node_compat_keep_standalone_entrypoints() {
    let docs = gates_for_mode(GateMode::DocSync, &environment()).unwrap();
    assert_eq!(
        docs.iter()
            .find(|gate| gate.id == "doc-typecheck")
            .unwrap()
            .display_command,
        "pnpm run doc-typecheck"
    );
    for node_major in [22, 24, 26] {
        let mut environment = environment();
        environment.node_major = node_major;
        let node = gates_for_mode(GateMode::NodeCompat, &environment).unwrap();
        assert_eq!(node[0].id, "typecheck");
        assert_eq!(node[0].display_command, "pnpm run typecheck");
        assert!(node[0].needs.is_empty());
        let ids = node.iter().map(|gate| gate.id.as_str()).collect::<Vec<_>>();
        if node_major == 22 {
            assert_eq!(
                ids,
                [
                    "typecheck",
                    "build",
                    "build:web",
                    "source-worker-smoke",
                    "jsonl-zstd-smoke",
                    "seekdeep-source-launch-smoke",
                    "cli-lazy-search-startup-smoke",
                ]
            );
            assert_eq!(node[1].display_command, "pnpm run build");
            assert_eq!(node[1].needs, ["typecheck"]);
            assert_eq!(node[2].display_command, "pnpm run build:web");
            assert_eq!(node[2].needs, ["build"]);
        } else {
            assert_eq!(
                ids,
                [
                    "typecheck",
                    "source-worker-smoke",
                    "jsonl-zstd-smoke",
                    "seekdeep-source-launch-smoke",
                ]
            );
        }
    }
}

#[test]
fn node_compat_smokes_run_the_verified_suites_on_every_node_line() {
    for node_major in [22, 24, 26] {
        let mut environment = environment();
        environment.node_major = node_major;
        let node = gates_for_mode(GateMode::NodeCompat, &environment).unwrap();
        for (id, label, args) in [
            (
                "source-worker-smoke",
                "source worker smoke",
                "test --locked -p seekdeep-workflow-worker-thread --all-features --test start_validation_parity",
            ),
            (
                "jsonl-zstd-smoke",
                "JSONL Zstandard smoke",
                "test --locked -p seekdeep-session-persistence-jsonl --all-features --lib zstd::tests",
            ),
            (
                "seekdeep-source-launch-smoke",
                "seekdeep source-launch smoke",
                "test --locked -p seekdeep --all-features --test source_launch_compat",
            ),
        ] {
            let smoke = node.iter().find(|gate| gate.id == id).unwrap();
            assert_eq!(smoke.label, label);
            assert_eq!(smoke.command, PathBuf::from("cargo"));
            assert_eq!(
                smoke.args,
                args.split(' ').map(OsString::from).collect::<Vec<_>>()
            );
            assert_eq!(smoke.display_command, format!("cargo {args}"));
            assert!(smoke.needs.is_empty());
            assert!(smoke.environment.is_empty());
            assert_eq!(smoke.serial_group.as_deref(), Some("cargo-target"));
            assert!(!smoke.allow_failure);
        }
        // The port carries no Vitest projects, so the source's jsdom environment smoke has
        // no suite to run; `typecheck` checks the browser Rust on every line instead.
        assert!(node.iter().all(|gate| gate.id != "vitest-jsdom-smoke"));
        let cli = node
            .iter()
            .find(|gate| gate.id == "cli-lazy-search-startup-smoke");
        if node_major == 22 {
            let cli = cli.unwrap();
            assert_eq!(cli.label, "CLI lazy-search startup smoke");
            assert_eq!(cli.needs, ["build:web"]);
            assert_eq!(
                cli.environment.get("SEEKDEEP_REQUIRE_BUILT_CLI_SMOKE"),
                Some(&Some("1".to_owned()))
            );
            assert_eq!(
                cli.display_command,
                "cargo test --locked -p seekdeep --all-features --test shipped_cli_contracts"
            );
            assert_eq!(cli.serial_group.as_deref(), Some("cargo-target"));
        } else {
            assert!(cli.is_none());
        }
    }
}

#[test]
fn node_compat_skips_typecheck_only_on_the_exact_flag() {
    let mut environment = environment();
    environment.node_major = 22;
    environment
        .variables
        .insert("SEEKDEEP_NODE_COMPAT_SKIP_TYPECHECK".into(), "1".into());
    let node = gates_for_mode(GateMode::NodeCompat, &environment).unwrap();
    assert_eq!(
        node.iter().map(|gate| gate.id.as_str()).collect::<Vec<_>>(),
        [
            "build",
            "build:web",
            "source-worker-smoke",
            "jsonl-zstd-smoke",
            "seekdeep-source-launch-smoke",
            "cli-lazy-search-startup-smoke",
        ]
    );
    assert!(node[0].needs.is_empty());
    environment.node_major = 24;
    let node = gates_for_mode(GateMode::NodeCompat, &environment).unwrap();
    assert_eq!(
        node.iter().map(|gate| gate.id.as_str()).collect::<Vec<_>>(),
        [
            "source-worker-smoke",
            "jsonl-zstd-smoke",
            "seekdeep-source-launch-smoke",
        ]
    );
    environment
        .variables
        .insert("SEEKDEEP_NODE_COMPAT_SKIP_TYPECHECK".into(), "".into());
    assert_eq!(
        gates_for_mode(GateMode::NodeCompat, &environment).unwrap()[0].id,
        "typecheck"
    );
    environment
        .variables
        .insert("SEEKDEEP_NODE_COMPAT_SKIP_TYPECHECK".into(), "yes".into());
    let error = gates_for_mode(GateMode::NodeCompat, &environment)
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "run-gates: SEEKDEEP_NODE_COMPAT_SKIP_TYPECHECK must be 1 when set, got \"yes\"."
    );
}

#[test]
fn node_compat_smokes_name_suites_the_workspace_builds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let metadata = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--locked", "--no-deps", "--format-version=1"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        metadata.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    let mut environment = environment();
    environment.node_major = 22;
    let smokes = gates_for_mode(GateMode::NodeCompat, &environment)
        .unwrap()
        .into_iter()
        .filter(|gate| gate.command == std::path::Path::new("cargo"))
        .collect::<Vec<_>>();
    assert_eq!(smokes.len(), 4);
    for smoke in smokes {
        let args = smoke
            .args
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args[..2], ["test", "--locked"], "{}", smoke.id);
        let package = args.windows(2).find(|pair| pair[0] == "-p").unwrap()[1];
        let package = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == package)
            .unwrap_or_else(|| panic!("{}: package {package} is not in the workspace", smoke.id));
        let targets = package["targets"].as_array().unwrap();
        let has_target = |name: &str, kind: &str| {
            targets.iter().any(|value| {
                value["name"] == name
                    && value["kind"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == kind)
            })
        };
        for target in args
            .windows(2)
            .filter(|pair| pair[0] == "--test")
            .map(|pair| pair[1])
        {
            assert!(has_target(target, "test"), "{}: {target}", smoke.id);
        }
        if args.contains(&"--lib") {
            assert!(
                targets.iter().any(|value| value["kind"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|kind| kind == "lib")),
                "{}: no library target",
                smoke.id
            );
        }
    }
}

#[test]
fn static_lane_stays_source_only() {
    let ids = gates_for_mode(GateMode::CiStatic, &environment())
        .unwrap()
        .into_iter()
        .map(|gate| gate.id)
        .collect::<Vec<_>>();
    assert!(!ids.iter().any(|id| id == "build"));
    assert!(!ids.iter().any(|id| id == "doc-typecheck"));
}

#[test]
fn consumer_graph_owns_build_and_orders_all_artifact_readers() {
    let gates = gates_for_mode(GateMode::CiConsumers, &environment()).unwrap();
    assert_eq!(
        default_concurrency(GateMode::CiConsumers, gates.len(), 4),
        ConcurrencyDefault {
            workers: 10,
            source: "ci-consumers gate count".to_owned()
        }
    );
    assert_eq!(
        gates
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>(),
        [
            "build",
            "node-compat",
            "publint",
            "built-package-invariants",
            "lint-and-duplication",
            "snapshot",
            "web-snapshot",
            "doc-typecheck",
            "node-next-types",
            "built-bin-smoke",
        ]
    );
    assert_eq!(
        gates
            .iter()
            .find(|gate| gate.id == "built-package-invariants")
            .unwrap()
            .needs,
        ["publint"]
    );
    for id in [
        "lint-and-duplication",
        "snapshot",
        "web-snapshot",
        "doc-typecheck",
        "node-next-types",
        "built-bin-smoke",
    ] {
        assert_eq!(
            gates.iter().find(|gate| gate.id == id).unwrap().needs,
            ["built-package-invariants"]
        );
    }
    assert_eq!(
        gates
            .iter()
            .find(|gate| gate.id == "publint")
            .unwrap()
            .needs,
        ["build"]
    );
    assert_eq!(
        gates
            .iter()
            .find(|gate| gate.id == "snapshot")
            .unwrap()
            .environment
            .get("SEEKDEEP_EXAMPLE_MODE"),
        Some(&Some("lib".to_owned()))
    );
    assert_eq!(
        gates
            .iter()
            .find(|gate| gate.id == "doc-typecheck")
            .unwrap()
            .environment
            .get("SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT"),
        Some(&Some("1".to_owned()))
    );
    let built = gates
        .iter()
        .find(|gate| gate.id == "built-bin-smoke")
        .unwrap();
    assert_eq!(
        built.args,
        ["/private/pnpm.cjs", "run", "test:built-bin"].map(OsString::from)
    );
    let web = gates.iter().find(|gate| gate.id == "web-snapshot").unwrap();
    assert_eq!(
        web.display_command,
        "SEEKDEEP_SNAPSHOT=replay pnpm run test:web:built"
    );
}

#[test]
fn linux_primary_adds_compare_only_web_gate_after_built_invariants() {
    let gates = gates_for_mode(GateMode::CiLinuxPrimary, &environment()).unwrap();
    let web = gates.iter().find(|gate| gate.id == "web-snapshot").unwrap();
    assert_eq!(web.needs, ["built-package-invariants"]);
    assert_eq!(
        web.environment.get("SEEKDEEP_SNAPSHOT"),
        Some(&Some("replay".to_owned()))
    );
}

#[cfg(unix)]
#[test]
fn process_signal_and_spawn_failures_keep_independent_outcome_facts() {
    use seekdeep_repository_tools::run_gates::{format_gate_result_reason, run_gate};
    use std::path::Path;

    let mut terminated = gate("terminated", &[]);
    terminated.command = PathBuf::from("/bin/sh");
    terminated.args = vec![OsString::from("-c"), OsString::from("kill -TERM $$")];
    let inherited = std::env::vars_os().collect::<BTreeMap<_, _>>();
    let result = run_gate(Path::new("."), &inherited, terminated);
    assert_eq!(result.status, GateResultStatus::Failed);
    assert_eq!(result.exit_code, None);
    assert_eq!(result.signal_code.as_deref(), Some("SIGTERM"));
    assert_eq!(format_gate_result_reason(&result), "signal SIGTERM");

    let mut missing = gate("missing", &[]);
    missing.command = PathBuf::from("/definitely/missing/run-gates-command");
    let result = run_gate(Path::new("."), &inherited, missing);
    assert!(format_gate_result_reason(&result).contains("failed to start command"));
}
