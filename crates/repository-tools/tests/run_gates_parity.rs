//! Graph membership, dependency scheduling, process outcomes, and target rename parity.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use indexmap::IndexMap;
use seekdeep_repository_tools::run_gates::{
    ConcurrencyDefault, Gate, GateEnvironment, GateMode, GateResult, GateResultStatus,
    assign_cargo_serial_group, default_concurrency, format_gate_result_reason, gates_for_mode,
    run_gate, run_gates, validate_gate_graph,
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
        (GateMode::CiPrimary, 52),
        (GateMode::CiLinuxPrimary, 53),
        (GateMode::CiStatic, 35),
        (GateMode::CiLintContractsReady, 2),
        (GateMode::CiCoverage, 2),
        (GateMode::CiSnapshot, 2),
        (GateMode::CiArtifacts, 5),
        (GateMode::CiConsumers, 10),
        (GateMode::CiWindowsBlocking, 2),
        (GateMode::CiWindowsComplete, 44),
        (GateMode::CiWindowsObservational, 42),
        (GateMode::NodeCompat, 5),
        (GateMode::CheckAll, 46),
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
fn lint_and_typert_consumers_preserve_command_and_dependency_contracts() {
    let mut environment = environment();
    environment
        .variables
        .insert("SEEKDEEP_OXLINT_THREADS".into(), "4".into());
    let lint = gates_for_mode(GateMode::CiLintContractsReady, &environment)
        .unwrap()
        .remove(0);
    assert_eq!(
        lint.display_command,
        "SEEKDEEP_OXLINT_THREADS=4 pnpm run lint:contracts-ready"
    );
    assert_eq!(
        lint.args,
        [
            OsString::from("/private/pnpm.cjs"),
            OsString::from("run"),
            OsString::from("lint:contracts-ready")
        ]
    );

    let primary = gates_for_mode(GateMode::CiPrimary, &environment).unwrap();
    assert_eq!(
        primary
            .iter()
            .find(|gate| gate.id == "typert-contracts")
            .unwrap()
            .display_command,
        "pnpm run build:lib:host"
    );
    for id in ["typecheck", "lint", "doc-typecheck"] {
        assert_eq!(
            primary.iter().find(|gate| gate.id == id).unwrap().needs,
            ["typert-contracts"]
        );
    }
    assert_eq!(
        primary
            .iter()
            .find(|gate| gate.id == "build")
            .unwrap()
            .needs,
        ["typecheck", "lint", "doc-typecheck"]
    );
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
    let node = gates_for_mode(GateMode::NodeCompat, &environment()).unwrap();
    let jsdom = node
        .iter()
        .find(|gate| gate.id == "vitest-jsdom-smoke")
        .unwrap();
    assert_eq!(jsdom.label, "Vitest jsdom smoke");
    assert!(
        jsdom
            .args
            .contains(&OsString::from("scripts/vitest-environment.compat.spec.ts"))
    );
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
    let built = gates
        .iter()
        .find(|gate| gate.id == "built-bin-smoke")
        .unwrap();
    assert!(built.args.contains(&OsString::from(
        "packages/subagent/subagent-codex/tests/loader-composition.e2e.ts"
    )));
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
