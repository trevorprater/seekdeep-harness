//! Port of the source `scripts/ci-workflow.spec.ts`: the shape of the CI, E2B, and git-hook
//! configuration that the runbooks rely on, read from the checked-in YAML.
//!
//! The source's two native Windows coverage-selection cases hold against the instrumented Rust
//! lane: its measured set and roster keep LSP source under the bar on every platform, and its one
//! `cargo llvm-cov` run keeps every test target in its own process.

use seekdeep_repository_tools::{
    coverage_exempt::INSTRUMENTED_LANE_EXCLUDED_PACKAGES,
    coverage_uncovered_locations::{
        CARGO_LLVM_COV_VERSION, MeasuredSet, ROSTER_PATH, Roster, parse_arguments, test_command,
    },
};
use serde_json::Value;

const RUNNER_PRIVATE_PNPM_DESTINATION: &str = "${{ runner.temp }}/setup-pnpm";
const MASTER_PUSH_ONLY: &str = "github.event_name == 'push' && github.ref == 'refs/heads/master'";

fn workflow(source: &str) -> Value {
    serde_yml::from_str(source).unwrap()
}

fn ci() -> Value {
    workflow(include_str!("../../../.github/workflows/ci.yml"))
}

fn job<'a>(workflow: &'a Value, name: &str) -> &'a Value {
    let job = &workflow["jobs"][name];
    assert!(job.is_object(), "workflow must define the {name} job");
    job
}

fn run_steps(job: &Value) -> Vec<&str> {
    job["steps"]
        .as_array()
        .expect("job steps")
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect()
}

fn runs_on(job: &Value) -> &str {
    job["runs-on"].as_str().expect("runs-on expression")
}

fn step<'a>(job: &'a Value, name: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .expect("job steps")
        .iter()
        .find(|step| step["name"] == name)
        .unwrap_or_else(|| panic!("workflow job must define the {name} step"))
}

fn needs(job: &Value) -> Vec<&str> {
    job["needs"]
        .as_array()
        .expect("aggregate needs")
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

#[test]
fn isolates_every_pnpm_action_setup_destination_per_runner() {
    let ci = ci();
    let mut setups = 0;
    for (name, job) in ci["jobs"].as_object().unwrap() {
        let Some(steps) = job["steps"].as_array() else {
            continue;
        };
        for step in steps {
            if step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("pnpm/action-setup@"))
            {
                setups += 1;
                assert_eq!(
                    step["with"]["dest"], RUNNER_PRIVATE_PNPM_DESTINATION,
                    "{name} must not share pnpm/action-setup's default destination"
                );
            }
        }
    }
    assert!(setups > 0);
}

#[test]
fn keeps_a_required_wine_windows_job_a_non_blocking_native_windows_job_with_failover_and_a_master_only_standby()
 {
    let ci = ci();
    let windows = job(&ci, "windows");
    let windows_native = job(&ci, "windows-native");
    let wine_apt_cache = job(&ci, "wine-apt-cache");
    let serial_windows = job(&ci, "serial-windows");
    let aggregate = job(&ci, "all-checks-passed");

    // Required PR job: Wine on ubuntu-latest, runs wine-windows-gates.sh.
    assert_eq!(windows["runs-on"], "ubuntu-latest");
    assert_eq!(windows["name"], "windows node 24 / wine blocking");
    assert_eq!(windows["if"], "github.event_name == 'pull_request'");
    assert!(
        run_steps(windows)
            .iter()
            .any(|run| run.contains("wine-windows-gates.sh"))
    );

    // windows-native: non-blocking native job with failover, runs windows-complete.
    // Its pool is resolved by the Windows-specific switch.
    let native_pool = runs_on(windows_native);
    assert!(native_pool.contains("SEEKDEEP_CI_FAILOVER_WINDOWS"));
    assert!(!native_pool.contains("SEEKDEEP_CI_FAILOVER_LINUX"));
    assert!(native_pool.contains("self-hosted"));
    assert!(native_pool.contains("seekdeep-win-ci"));
    assert!(native_pool.contains("SEEKDEEP_CI_HOSTED_WINDOWS_RUNNER"));
    assert!(native_pool.contains("'windows-latest'"));
    assert_eq!(windows_native["name"], "windows node 24 / native complete");
    assert_eq!(windows_native["if"], "github.event_name == 'pull_request'");
    assert!(run_steps(windows_native).contains(&"pnpm run check:ci:windows-complete"));

    // wine-apt-cache: master-only, seeds the Wine apt cache.
    assert_eq!(wine_apt_cache["if"], MASTER_PUSH_ONLY);
    assert_eq!(wine_apt_cache["runs-on"], "ubuntu-latest");

    // serial-windows: master-only standby, self-hosted, non-blocking.
    assert_eq!(serial_windows["if"], MASTER_PUSH_ONLY);
    assert_eq!(
        serial_windows["runs-on"],
        serde_json::json!(["self-hosted", "seekdeep-win-ci", "windows"])
    );
    assert_eq!(
        serial_windows["name"],
        "serial / windows (self-hosted standby)"
    );

    // Aggregate: Wine `windows` required, native `windows-native` excluded.
    let required = needs(aggregate);
    assert!(required.contains(&"windows"));
    assert!(!required.contains(&"windows-native"));
    assert!(!required.contains(&"serial-windows"));

    // Linux failover is a separate switch: the three required Linux workers and the
    // verdict job resolve their pool through SEEKDEEP_CI_FAILOVER_LINUX, never the
    // Windows switch.
    for name in [
        "node-24",
        "node-24-coverage",
        "node-24-consumers",
        "all-checks-passed",
    ] {
        let pool = runs_on(job(&ci, name));
        assert!(
            pool.contains("SEEKDEEP_CI_FAILOVER_LINUX"),
            "{name} runs-on must use the Linux failover switch"
        );
        assert!(
            !pool.contains("SEEKDEEP_CI_FAILOVER_WINDOWS"),
            "{name} runs-on must not use the Windows failover switch"
        );
        assert!(pool.contains("vm-backup"));
        if name != "all-checks-passed" {
            assert!(pool.contains("SEEKDEEP_CI_HOSTED_LINUX_RUNNER"));
            assert!(pool.contains("'ubuntu-24.04'"));
        }
    }
}

#[test]
fn primary_jobs_install_rust_wasm_dependencies_before_their_gate_inventory() {
    let ci = ci();
    for name in [
        "node-24",
        "node-24-coverage",
        "node-24-consumers",
        "windows-native",
    ] {
        let steps = job(&ci, name)["steps"].as_array().unwrap();
        let gates = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with("pnpm run check:ci:"))
            })
            .expect("primary gate inventory");
        let setup = &steps[..gates];
        assert!(
            setup.iter().any(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("dtolnay/rust-toolchain@"))
                    && step["with"]["targets"] == "wasm32-unknown-unknown"
            }),
            "{name} must install the WASM target"
        );
        for required in [
            "cargo install --locked wasm-bindgen-cli --version 0.2.127",
            "pnpm --dir support/browser-dependencies install --ignore-workspace --frozen-lockfile --config.strictDepBuilds=false",
        ] {
            assert!(
                setup
                    .iter()
                    .any(|step| step["run"] == required && step["if"].is_null()),
                "{name} missing {required}"
            );
        }
    }
}

#[test]
fn exempts_push_from_cancellation_so_one_master_merge_does_not_cancel_the_running_drill() {
    let ci = ci();
    // Cancellation applies to the whole superseded run, so it is decided at workflow
    // level and gated on the event: only push is exempt, and the negated form keeps
    // workflow_dispatch cancelling (a re-dispatched benchmark holds a dozen runners).
    assert_eq!(
        ci["concurrency"]["cancel-in-progress"],
        "${{ github.event_name != 'push' }}"
    );

    // Neither drill may carry a job-level group, and both stay master-push-only.
    for name in ["serial-linux-selfhosted", "serial-windows"] {
        let drill = job(&ci, name);
        assert!(drill["concurrency"].is_null());
        assert_eq!(drill["if"], MASTER_PUSH_ONLY);
    }

    // A master push may only carry the cache seeder and the two drills. Classification
    // is an exact allowlist of the conditions in use, not a substring match.
    let not_push_reachable = [
        "github.event_name == 'pull_request'",
        "always() && github.event_name == 'pull_request'",
        "github.event_name == 'workflow_dispatch' && inputs.suite == 'larger-runner-benchmark'",
        "github.event_name == 'workflow_dispatch' && inputs.suite == 'consolidated-runner-benchmark'",
    ];
    let mut push_reachable = ci["jobs"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, job)| match &job["if"] {
            Value::Bool(condition) => *condition,
            Value::String(condition) => !not_push_reachable.contains(&condition.trim()),
            // Unconditional jobs run on every event; an unrecognized shape is surfaced.
            _ => true,
        })
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    push_reachable.sort_unstable();
    assert_eq!(
        push_reachable,
        [
            "serial-linux-selfhosted",
            "serial-windows",
            "wine-apt-cache"
        ]
    );

    // Each benchmark fans out to a dozen larger runners at once in this same group.
    for name in ["larger-runner-benchmark", "consolidated-runner-benchmark"] {
        let benchmark = job(&ci, name);
        assert_eq!(benchmark["strategy"]["max-parallel"], 12);
        assert_eq!(benchmark["timeout-minutes"], 15);
    }
}

#[test]
fn native_windows_coverage_is_the_cargo_test_lane_and_carries_no_vitest_projects() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for configuration in ["vitest.config.ts", "vitest.shared.ts"] {
        assert!(
            !root.join(configuration).exists(),
            "{configuration} must not return: the port carries no Vitest projects"
        );
    }
    let ci = ci();
    assert!(
        run_steps(job(&ci, "windows-native"))
            .iter()
            .any(|run| run.starts_with("cargo test --locked"))
    );
}

#[test]
fn requires_one_release_shaped_python_runtime_target_on_every_pull_request() {
    let ci = ci();
    let python_runtime = job(&ci, "python-runtime");
    assert_eq!(python_runtime["if"], "github.event_name == 'pull_request'");
    assert_eq!(
        python_runtime["name"],
        "python runtime / release-shaped Linux x64"
    );
    assert_eq!(
        python_runtime["uses"],
        "./.github/workflows/build-exe-for-python-sdk.yml"
    );
    assert_eq!(python_runtime["with"]["targets"], "node24-linux-x64");
    assert_eq!(python_runtime["with"]["ci"], true);
    assert!(needs(job(&ci, "all-checks-passed")).contains(&"python-runtime"));
}

#[test]
fn release_publication_requires_complete_parity_against_the_pinned_source_checkout() {
    let release = workflow(include_str!("../../../.github/workflows/release.yml"));
    let pack = job(&release, "pack");
    let steps = pack["steps"].as_array().expect("pack steps");
    let parity_index = steps
        .iter()
        .position(|step| step["run"] == "cargo xtask parity --scope all")
        .expect("complete parity gate");
    assert!(steps[parity_index]["if"].is_null());
    assert!(steps[parity_index]["continue-on-error"].is_null());
    steps[..parity_index]
        .iter()
        .find(|step| step["uses"] == "./.github/actions/setup-source-oracle")
        .expect("oracle prepared before the gate");
    let setup = workflow(include_str!(
        "../../../.github/actions/setup-source-oracle/action.yml"
    ));
    let setup_steps = setup["runs"]["steps"].as_array().unwrap();
    let oracle = setup_steps
        .iter()
        .find(|step| step["id"] == "checkout")
        .expect("complete source checkout");
    assert_eq!(
        oracle["env"]["SOURCE_COMMIT"],
        "${{ steps.pin.outputs.commit }}"
    );
    assert!(
        oracle["run"]
            .as_str()
            .unwrap()
            .contains("https://github.com/deepseek-ai/deepseek-harness.git")
    );
    assert!(setup_steps.iter().any(|step| {
        step["id"] == "pin"
            && step["run"]
                .as_str()
                .is_some_and(|run| run.contains("SOURCE_SNAPSHOT"))
    }));
    for (index, step) in steps.iter().enumerate() {
        if step["run"]
            .as_str()
            .is_some_and(|run| run.contains("release:pack"))
        {
            assert!(
                index > parity_index,
                "packaging must follow successful parity"
            );
        }
    }
    assert_eq!(job(&release, "publish")["needs"], "pack");
    assert!(
        !job(&release, "publish")["if"]
            .as_str()
            .unwrap()
            .contains("always()")
    );
}

#[test]
fn vendor_pack_installs_the_binding_generator_before_building_host_packages() {
    let vendor = workflow(include_str!(
        "../../../.github/workflows/release-vendor.yml"
    ));
    let steps = job(&vendor, "pack")["steps"].as_array().unwrap();
    let build = steps
        .iter()
        .position(|step| step["run"] == "pnpm run build:lib:host")
        .expect("Host build step");
    let setup = &steps[..build];
    assert!(setup.iter().any(|step| {
        step["uses"]
            .as_str()
            .is_some_and(|action| action.starts_with("dtolnay/rust-toolchain@"))
            && step["with"]["targets"] == "wasm32-unknown-unknown"
    }));
    let bindings = setup
        .iter()
        .find(|step| step["run"] == "cargo install --locked wasm-bindgen-cli --version 0.2.127")
        .expect("matching wasm-bindgen CLI before the Host build");
    assert!(bindings["if"].is_null());
    assert!(bindings["continue-on-error"].is_null());
}

#[test]
fn real_api_e2e_installs_the_rust_wasm_and_browser_build_dependencies_before_building() {
    let e2e = workflow(include_str!("../../../.github/workflows/e2e.yml"));
    let steps = job(&e2e, "e2e")["steps"].as_array().unwrap();
    let build = steps
        .iter()
        .position(|step| step["run"] == "pnpm run build")
        .expect("build step");
    let setup = &steps[..build];
    assert!(setup.iter().any(|step| {
        step["uses"]
            .as_str()
            .is_some_and(|action| action.starts_with("dtolnay/rust-toolchain@"))
            && step["with"]["targets"]
                .as_str()
                .is_some_and(|targets| targets.contains("wasm32-unknown-unknown"))
    }));
    for required in [
        "cargo install --locked wasm-bindgen-cli --version 0.2.127",
        "pnpm --dir support/browser-dependencies install --ignore-workspace --frozen-lockfile --config.strictDepBuilds=false",
    ] {
        let step = setup
            .iter()
            .find(|step| step["run"] == required)
            .expect(required);
        assert!(step["if"].is_null());
        assert!(step["continue-on-error"].is_null());
    }
}

#[test]
fn e2b_e2e_workflow_is_manual_only_and_fails_loud_before_running_the_focused_live_suite() {
    let e2b = workflow(include_str!("../../../.github/workflows/e2b-e2e.yml"));
    assert_eq!(e2b["on"], serde_json::json!({"workflow_dispatch": null}));
    let steps = job(&e2b, "e2b")["steps"].as_array().unwrap();
    let preflight = steps
        .iter()
        .find(|step| step["name"] == "Preflight (require E2B API key)")
        .expect("preflight step");
    assert_eq!(
        preflight["env"]["E2B_API_KEY"],
        "${{ secrets.E2B_API_KEY_EXTERNAL }}"
    );
    assert!(
        preflight["run"]
            .as_str()
            .unwrap()
            .contains("E2B_API_KEY_EXTERNAL repository secret")
    );
    let live = steps
        .iter()
        .find(|step| step["name"] == "E2B tests (live sandbox)")
        .expect("live step");
    assert_eq!(
        live["env"]["E2B_API_KEY"],
        "${{ secrets.E2B_API_KEY_EXTERNAL }}"
    );
    assert_eq!(live["env"]["SEEKDEEP_E2E_MAX_WORKERS"], "1");
    assert_eq!(live["env"]["SEEKDEEP_EXAMPLE_MODE"], "lib");
    assert!(
        live["run"]
            .as_str()
            .unwrap()
            .contains("packages/e2b/e2b/tests/composition.e2e.ts")
    );
}

#[test]
fn git_hooks_leave_frozen_agent_note_sidecars_to_the_archive_verifier() {
    let lefthook = workflow(include_str!("../../../lefthook.yml"));
    for hook in ["pre-commit", "pre-merge-commit"] {
        let pairing = lefthook[hook]["jobs"]
            .as_array()
            .unwrap_or_else(|| panic!("lefthook must define {hook} jobs"))
            .iter()
            .find(|job| job["name"] == "translation pairing (staged records)")
            .expect("pairing job");
        assert_eq!(
            pairing["exclude"],
            serde_json::json!([".agents/notes/archived/**"])
        );
    }
}

#[test]
fn python_release_validates_wheels_from_a_labeled_dry_run_before_any_publication() {
    let release = workflow(include_str!(
        "../../../.github/workflows/python-release.yml"
    ));
    let publish = &release["on"]["workflow_dispatch"]["inputs"]["publish"];
    assert_eq!(publish["type"], "boolean");
    assert_eq!(publish["default"], false);
    assert_eq!(
        release["on"]["pull_request"],
        serde_json::json!({"types": ["labeled"]})
    );
    let build = job(&release, "build");
    assert_eq!(
        build["if"],
        "github.event_name == 'workflow_dispatch' || github.event.label.name == 'python-release-dry-run'"
    );
    assert_eq!(
        build["uses"],
        "./.github/workflows/build-exe-for-python-sdk.yml"
    );
    assert_eq!(
        build["with"]["targets"],
        "node24-linux-x64,node24-linux-arm64,node24-macos-arm64"
    );
    assert_eq!(build["with"]["release"], true);
    let compatibility = job(&release, "python-compat");
    assert_eq!(
        compatibility["strategy"]["matrix"]["python"],
        serde_json::json!(["3.10", "3.14"])
    );
    // The product rename carries the SDK distribution the source pins as `deepseek-harness-sdk`.
    assert!(
        compatibility["steps"]
            .to_string()
            .contains("seekdeep-harness-sdk==${{ steps.compatibility-version.outputs.version }}")
    );
    let validate = job(&release, "validate");
    let validate_steps = validate["steps"].to_string();
    assert!(validate_steps.contains("PUBLIC_PYPI_RELEASE_ENABLED"));
    assert!(validate_steps.contains("100000000"));
    let authorize = step(validate, "Authorize publication request");
    assert_eq!(
        authorize["env"]["PYPI_PUBLISHER_REPOSITORY"],
        "${{ vars.PYPI_PUBLISHER_REPOSITORY }}"
    );
    assert_eq!(authorize["env"]["REPOSITORY"], "${{ github.repository }}");
    assert!(
        authorize["run"]
            .as_str()
            .unwrap()
            .contains("[ \"$REPOSITORY\" = \"$PYPI_PUBLISHER_REPOSITORY\" ]")
    );
}

#[test]
fn python_release_publishes_only_from_a_manual_dispatch_with_verified_hashes() {
    let release = workflow(include_str!(
        "../../../.github/workflows/python-release.yml"
    ));
    let runtime = job(&release, "publish-runtime");
    let sdk = job(&release, "publish-sdk");
    let manual = "github.event_name == 'workflow_dispatch' && inputs.publish";
    let permissions = serde_json::json!({"contents": "read", "id-token": "write"});
    assert_eq!(runtime["if"], manual);
    assert_eq!(runtime["needs"], "validate");
    assert_eq!(runtime["environment"], "pypi-runtime");
    assert_eq!(runtime["permissions"], permissions);
    assert_eq!(sdk["if"], manual);
    assert_eq!(
        sdk["needs"],
        serde_json::json!(["validate", "publish-runtime"])
    );
    assert_eq!(sdk["environment"], "pypi");
    assert_eq!(sdk["permissions"], permissions);
    let steps = [runtime, sdk]
        .iter()
        .flat_map(|job| job["steps"].as_array().expect("publish steps"))
        .collect::<Vec<_>>();
    assert!(steps.iter().all(|step| {
        !step["uses"]
            .as_str()
            .is_some_and(|uses| uses.starts_with("actions/checkout@"))
    }));
    assert_eq!(
        steps
            .iter()
            .filter(|step| step["uses"] == "pypa/gh-action-pypi-publish@release/v1")
            .count(),
        2
    );
    let runtime_publish = step(runtime, "Publish runtime wheels");
    assert_eq!(runtime_publish["with"]["packages-dir"], "dist/runtime/");
    assert_eq!(runtime_publish["with"]["attestations"], false);
    let sdk_publish = step(sdk, "Publish SDK wheel");
    assert_eq!(sdk_publish["with"]["packages-dir"], "dist/sdk/");
    assert_eq!(sdk_publish["with"]["attestations"], false);
    for publisher in [runtime, sdk] {
        assert_eq!(
            step(publisher, "Verify release artifact hashes")["run"],
            "cd dist && sha256sum -c SHA256SUMS"
        );
    }
}

#[test]
fn python_wheel_builder_exposes_itself_to_the_release_caller_with_normalized_versions() {
    let builder = workflow(include_str!(
        "../../../.github/workflows/build-exe-for-python-sdk.yml"
    ));
    let inputs = &builder["on"]["workflow_call"]["inputs"];
    assert!(inputs["targets"].is_object());
    for flag in ["ci", "release"] {
        assert_eq!(inputs[flag]["type"], "boolean");
        assert_eq!(inputs[flag]["default"], false);
    }
    assert_eq!(
        builder["concurrency"]["group"],
        "build-single-exe-${{ github.workflow }}-${{ github.ref }}"
    );
    let plan = job(&builder, "plan");
    let condition = plan["if"].as_str().unwrap();
    assert!(condition.contains("inputs.ci"));
    assert!(condition.contains("inputs.release"));
    // The source normalizes its PEP 440 version in a shell step; the port resolves it through
    // the Rust release crate, whose GitHub output feeds the same plan outputs.
    let version = step(plan, "Resolve repository version");
    assert_eq!(version["id"], "version");
    assert!(version["run"].as_str().unwrap().contains(
        "cargo run --quiet --locked -p seekdeep-python-release -- version --github-output >> \"$GITHUB_OUTPUT\""
    ));
    assert_eq!(
        plan["outputs"]["version"],
        "${{ steps.version.outputs.version }}"
    );
    assert_eq!(
        plan["outputs"]["repository-version"],
        "${{ steps.version.outputs.repository-version }}"
    );
    assert!(builder.to_string().contains("macosx_14_0_arm64"));
    let build = job(&builder, "build");
    // The source rebuilds node-pty inside manylinux 2.28 and reads the addon's GLIBC
    // requirements; the port builds the Rust runtime inside that container and proves the
    // payload's GLIBC requirement from the produced executable and binding.
    let container_build = step(build, "Build Rust executable against manylinux 2.28");
    assert_eq!(container_build["if"], "runner.os == 'Linux'");
    let glibc = step(build, "Check Linux GLIBC requirements");
    assert_eq!(glibc["if"], "runner.os == 'Linux'");
    let glibc_script = glibc["run"].as_str().unwrap();
    assert!(glibc_script.contains("readelf --version-info"));
    assert!(glibc_script.contains("glibc-versions.txt"));
    assert!(glibc_script.contains("le 2.28"));
    // The source's Python deployment-target checker is the verified xtask command.
    let macos = step(build, "Check macOS deployment target");
    assert_eq!(macos["if"], "runner.os == 'macOS'");
    let macos_script = macos["run"].as_str().unwrap();
    assert!(macos_script.contains("cargo xtask macos-deployment-target"));
    assert!(macos_script.contains("$EXE-spawn-helper"));
    let smoke = step(build, "Run wheel in a manylinux 2.28 container");
    assert_eq!(smoke["if"], "runner.os == 'Linux'");
    assert!(
        smoke["run"]
            .as_str()
            .unwrap()
            .contains("-e SEEKDEEP_TELEMETRY_DISABLED")
    );
}

#[test]
fn gitlab_uses_the_shared_macos_deployment_target_check() {
    let gitlab = workflow(include_str!("../../../.gitlab-ci.yml"));
    let script = gitlab[".runtime-wheel"]["script"]
        .as_array()
        .expect("GitLab CI must define the runtime wheel script");
    let check = script
        .iter()
        .filter_map(Value::as_str)
        .find(|step| step.contains("PLATFORM\" = macos-arm64"))
        .expect("GitLab CI must check the macOS deployment target");
    assert!(check.contains("cargo xtask macos-deployment-target"));
    assert!(check.contains("\"$EXE\" \"$EXE-spawn-helper\""));
}

#[test]
fn issue_lifecycle_uses_explicit_review_handoff_events_without_rerunning_when_a_draft_becomes_ready()
 {
    let lifecycle = workflow(include_str!(
        "../../../.github/workflows/issue-lifecycle.yml"
    ));
    let policy = workflow(include_str!("../../../.github/workflows/issue-policy.yml"));
    let pull_request_types = lifecycle["on"]["pull_request"]["types"]
        .as_array()
        .expect("lifecycle pull_request types");
    assert!(!pull_request_types.contains(&Value::from("ready_for_review")));
    assert!(pull_request_types.contains(&Value::from("review_requested")));
    assert_eq!(
        lifecycle["on"]["pull_request_review"]["types"],
        serde_json::json!(["submitted"])
    );
    // The port's lifecycle app is optional, so its client id guards the job ahead of the
    // source's review-handoff condition, which is kept verbatim.
    let condition = job(&lifecycle, "lifecycle")["if"].as_str().unwrap();
    assert!(condition.starts_with("${{ vars.SEEKDEEP_ISSUE_APP_CLIENT_ID != '' && ("));
    assert!(condition.contains(
        "github.event_name != 'pull_request_review' || (github.event.action == 'submitted' && github.event.review.state == 'changes_requested')"
    ));
    assert!(
        policy["on"]["pull_request"]["types"]
            .as_array()
            .expect("policy pull_request types")
            .contains(&Value::from("ready_for_review"))
    );
}

#[test]
fn coverage_lanes_install_the_instrumentation_toolchain_before_their_gate_inventory() {
    let ci = ci();
    for name in ["node-24-coverage", "windows-native"] {
        let steps = job(&ci, name)["steps"].as_array().unwrap();
        let gates = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.starts_with("pnpm run check:ci:"))
            })
            .expect("gate inventory");
        let setup = &steps[..gates];
        let toolchain = setup
            .iter()
            .find(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("dtolnay/rust-toolchain@"))
            })
            .expect("toolchain step");
        assert_eq!(
            toolchain["with"]["components"], "llvm-tools-preview",
            "{name} must install the LLVM tools"
        );
        let installer = setup
            .iter()
            .find(|step| step["uses"] == "taiki-e/install-action@v2")
            .unwrap_or_else(|| panic!("{name} must install cargo-llvm-cov"));
        assert_eq!(
            installer["with"]["tool"],
            format!("cargo-llvm-cov@{CARGO_LLVM_COV_VERSION}")
        );
        assert!(installer["if"].is_null());
    }
    // The instrumented target directory has its own cache key on the Linux lane.
    let cache = job(&ci, "node-24-coverage")["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["uses"] == "Swatinem/rust-cache@v2")
        .expect("rust cache");
    assert_eq!(cache["with"]["key"], "coverage-rust-wasm");
}

#[test]
fn keeps_supported_lsp_source_under_native_windows_coverage() {
    // The source pinned that its coverage configuration no longer excludes lsp-stdio's
    // connection, index, and instance modules. The port's measured set holds their crate to the
    // bar on every platform, and no roster entry lifts an lsp-stdio file on Windows alone.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let measured = MeasuredSet::load(&root).unwrap();
    for file in [
        "crates/lsp-stdio/src/connection.rs",
        "crates/lsp-stdio/src/provider.rs",
        "crates/lsp-stdio/src/instance.rs",
    ] {
        assert!(measured.measures(file), "{file} must stay measured");
        assert!(root.join(file).is_file(), "{file} must exist");
    }
    let roster = Roster::load(&root.join(ROSTER_PATH)).unwrap();
    for (path, entry) in &roster.files {
        assert!(
            !(path.starts_with("crates/lsp-stdio/")
                && entry.platforms.iter().any(|platform| platform == "windows")),
            "{path} must not be lifted on Windows alone"
        );
    }
}

#[test]
fn keeps_every_suite_process_isolated_on_native_windows() {
    // The source pinned every Vitest project on the forks pool with no win32 threads switch.
    // The port's instrumented lane is one `cargo llvm-cov` run whose test targets are separate
    // processes on every platform; its command carries no platform-dependent pool switch.
    let command = test_command(
        &parse_arguments(&[]).unwrap(),
        INSTRUMENTED_LANE_EXCLUDED_PACKAGES,
    );
    let text = command
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(text.contains(&"--workspace".to_owned()));
    assert!(text.contains(&"--no-fail-fast".to_owned()));
    assert!(!text.iter().any(|argument| argument.starts_with("--jobs")
        || argument == "-j"
        || argument.contains("test-threads")));
    for package in INSTRUMENTED_LANE_EXCLUDED_PACKAGES {
        assert!(text.contains(&(*package).to_owned()));
    }
}
