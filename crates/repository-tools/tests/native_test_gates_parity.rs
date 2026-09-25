//! Source roster completeness and real compiled-suite success/failure propagation.

use std::{
    collections::BTreeMap, ffi::OsString, num::NonZeroUsize, path::PathBuf, process::Command,
};

use seekdeep_repository_tools::{
    coverage_exempt::COVERAGE_EXEMPT_HEAVY_SUITES,
    native_test_gates::{
        NativeTestCommand, NativeTestGate, native_suites, parse_native_test_arguments,
        run_native_test_command,
    },
    run_gates::{GateEnvironment, GateMode, assign_cargo_serial_group, gates_for_mode},
};

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn every_source_smoke_and_heavy_suite_has_an_existing_compiled_target() {
    let root = repository();
    let oracle = seekdeep_source_oracle::SourceOracle::open(&root).unwrap();
    let source = oracle.read("scripts/run-gates.ts").unwrap();
    let function = source
        .split_once("function builtBinSmokeGate(")
        .unwrap()
        .1
        .split_once("\n}\n")
        .unwrap()
        .0;
    let pattern = regex::Regex::new(r"'([^']+\.e2e\.ts)'").unwrap();
    let source_paths = pattern
        .captures_iter(function)
        .map(|capture| capture[1].to_owned())
        .collect::<Vec<_>>();
    let smoke = native_suites(NativeTestGate::BuiltBin);
    assert_eq!(
        smoke.iter().map(|suite| suite.source).collect::<Vec<_>>(),
        source_paths
    );
    let heavy = native_suites(NativeTestGate::CoverageExemptHeavy);
    assert_eq!(
        heavy.iter().map(|suite| suite.source).collect::<Vec<_>>(),
        COVERAGE_EXEMPT_HEAVY_SUITES
            .iter()
            .map(|suite| suite.filter)
            .collect::<Vec<_>>()
    );

    let metadata = Command::new(env!("CARGO"))
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
    for suite in smoke.iter().chain(&heavy) {
        let NativeTestCommand::Cargo(args) = &suite.command else {
            continue;
        };
        if args.first() != Some(&"test") {
            continue;
        }
        let package = args.windows(2).find(|pair| pair[0] == "--package").unwrap()[1];
        let package = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == package)
            .unwrap();
        for target in args
            .windows(2)
            .filter(|pair| pair[0] == "--test")
            .map(|pair| pair[1])
        {
            assert!(
                package["targets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value["name"] == target
                        && value["kind"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|kind| kind == "test")),
                "{}: {target}",
                suite.source
            );
        }
    }
}

#[test]
fn native_leaf_scripts_share_the_cargo_resource_group() {
    let environment = GateEnvironment {
        variables: BTreeMap::new(),
        node_executable: "node".into(),
        pnpm_entrypoint: "pnpm.cjs".into(),
        node_major: 24,
        available_parallelism: 4,
    };
    for (mode, id) in [
        (GateMode::CiConsumers, "built-bin-smoke"),
        (GateMode::CiCoverage, "coverage-exempt-heavy"),
    ] {
        let mut gates = gates_for_mode(mode, &environment).unwrap();
        assign_cargo_serial_group(&repository(), &mut gates).unwrap();
        let gate = gates.iter().find(|gate| gate.id == id).unwrap();
        assert_eq!(gate.serial_group.as_deref(), Some("cargo-target"));
        assert!(!gate.allow_failure);
    }
}

#[test]
fn worker_limits_are_validated_before_any_suite_starts() {
    let valid = ["coverage-exempt-heavy", "--maxWorkers=3"].map(OsString::from);
    let parsed = parse_native_test_arguments(&valid).unwrap();
    assert_eq!(parsed.gate, NativeTestGate::CoverageExemptHeavy);
    assert_eq!(parsed.workers.unwrap().get(), 3);
    for arguments in [
        vec!["unknown"],
        vec!["built-bin", "--maxWorkers=0"],
        vec!["built-bin", "--maxWorkers=-1"],
    ] {
        assert!(
            parse_native_test_arguments(
                &arguments
                    .into_iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
    }
}

#[test]
fn e2e_lane_selects_multiple_modules_in_one_test_binary() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join("scripts")).unwrap();
    std::fs::create_dir_all(root.join("crates/fixture/tests")).unwrap();
    std::fs::copy(
        repository().join("scripts/run-e2e-lane.sh"),
        root.join("scripts/run-e2e-lane.sh"),
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("crates/fixture/Cargo.toml"),
        "[package]\nname = \"e2e-lane-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\nautotests = false\n[[test]]\nname = \"main\"\npath = \"tests/main.rs\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("crates/fixture/tests/main.rs"),
        "mod first;\nmod second;\n#[test]\nfn unselected() { panic!(\"keyless tests must not run\"); }\n",
    )
    .unwrap();
    for name in ["first", "second"] {
        std::fs::write(
            root.join(format!("crates/fixture/tests/{name}.rs")),
            "// DEEPSEEK_API_KEY\n#[test]\n#[ignore]\nfn selected() {}\n",
        )
        .unwrap();
    }
    let output = Command::new("bash")
        .arg("scripts/run-e2e-lane.sh")
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env("SEEKDEEP_E2E_MAX_WORKERS", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test first::selected ... ok"), "{stdout}");
    assert!(stdout.contains("test second::selected ... ok"), "{stdout}");
    assert!(stdout.contains("2 passed; 0 failed; 0 ignored"), "{stdout}");
}

#[test]
fn a_real_cargo_test_failure_remains_a_gate_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join("tests")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"native-gate-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[workspace]\n").unwrap();
    std::fs::write(root.join("tests/suite.rs"), "#[test]\nfn required_state() { assert!(std::path::Path::new(\"accept\").is_file(), \"required state missing\"); }\n").unwrap();
    std::fs::write(root.join("accept"), "present\n").unwrap();
    let command = NativeTestCommand::Cargo(vec![
        "test",
        "--offline",
        "--target-dir",
        "target",
        "--test",
        "suite",
        "--",
        "required_state",
    ]);
    run_native_test_command(root, &command, NonZeroUsize::new(1)).unwrap();
    std::fs::remove_file(root.join("accept")).unwrap();
    assert!(run_native_test_command(root, &command, NonZeroUsize::new(1)).is_err());
}

#[test]
fn built_lsp_consumer_queries_the_compiled_fixture_and_disposes() {
    run_native_test_command(&repository(), &NativeTestCommand::LspBuiltConsumer, None).unwrap();
}
