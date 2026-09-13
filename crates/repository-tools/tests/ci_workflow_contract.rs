//! Port of the source `scripts/ci-workflow.spec.ts`: the shape of the CI, E2B, and git-hook
//! configuration that the runbooks rely on, read from the checked-in YAML.
//!
//! The source's two Vitest-project assertions (LSP source under native Windows coverage,
//! process-isolated projects) have no counterpart: the port carries no Vitest projects, and
//! native Windows coverage is the `cargo test` invocation the native job runs.

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
    assert!(native_pool.contains("seekdeep-windows-2025-16core"));
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
