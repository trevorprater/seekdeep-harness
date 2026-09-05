//! Release workflows retain publication authority while resolving versions through Rust.

use serde_json::Value;

fn workflow(source: &str) -> Value {
    serde_yml::from_str(source).unwrap()
}

#[test]
fn version_and_wheel_steps_use_rust_and_matching_distribution_filenames() {
    let build_text = include_str!("../../../.github/workflows/build-exe-for-python-sdk.yml");
    let release_text = include_str!("../../../.github/workflows/python-release.yml");
    for text in [build_text, release_text] {
        assert!(!text.contains("scripts/build-python-release.py"));
        assert!(!text.contains("deepseek_harness_sdk-"));
        assert!(!text.contains("deepseek_harness_runtime_bin-"));
        assert!(text.contains(
            "cargo run --quiet --locked -p seekdeep-python-release -- version --github-output"
        ));
    }
    let build = workflow(build_text);
    let plan = build["jobs"]["plan"]["steps"].as_array().unwrap();
    assert!(
        plan.iter()
            .any(|step| step["uses"] == "dtolnay/rust-toolchain@1.93.1")
    );
    assert_eq!(build["env"]["CARGO_INCREMENTAL"], "0");
    for job in ["sdk-wheel", "build"] {
        let steps = build["jobs"][job]["steps"].as_array().unwrap();
        assert!(steps.iter().any(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("seekdeep-python-release -- build"))
        }));
    }
}

#[test]
fn publishing_stays_manual_repository_tag_gated_and_runtime_first() {
    let release = workflow(include_str!(
        "../../../.github/workflows/python-release.yml"
    ));
    assert_eq!(
        release["on"]["workflow_dispatch"]["inputs"]["publish"]["default"],
        false
    );
    assert_eq!(
        release["permissions"],
        serde_json::json!({"contents":"read"})
    );
    assert_eq!(release["concurrency"]["cancel-in-progress"], false);
    let expected_if = "github.event_name == 'workflow_dispatch' && inputs.publish";
    assert_eq!(release["jobs"]["publish-runtime"]["if"], expected_if);
    assert_eq!(release["jobs"]["publish-sdk"]["if"], expected_if);
    assert_eq!(
        release["jobs"]["publish-runtime"]["environment"],
        "pypi-runtime"
    );
    assert_eq!(release["jobs"]["publish-sdk"]["environment"], "pypi");
    assert_eq!(
        release["jobs"]["publish-sdk"]["needs"],
        serde_json::json!(["validate", "publish-runtime"])
    );
    let guard = release["jobs"]["validate"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "Authorize publication request")
        .unwrap()["run"]
        .as_str()
        .unwrap();
    for requirement in [
        "[ -n \"$PYPI_PUBLISHER_REPOSITORY\" ]",
        "[ \"$REPOSITORY\" = \"$PYPI_PUBLISHER_REPOSITORY\" ]",
        "[ \"$PUBLIC_PYPI_RELEASE_ENABLED\" = true ]",
        "[ \"$REF_TYPE\" = tag ]",
        "[ \"$REF_NAME\" = \"python-v$REPOSITORY_VERSION\" ]",
    ] {
        assert!(
            guard.contains(requirement),
            "missing publication requirement {requirement}"
        );
    }
    for job in ["publish-runtime", "publish-sdk"] {
        let steps = release["jobs"][job]["steps"].as_array().unwrap();
        assert!(steps.iter().any(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("sha256sum -c SHA256SUMS"))
        }));
        let publish = steps
            .iter()
            .find(|step| step["uses"] == "pypa/gh-action-pypi-publish@release/v1")
            .unwrap();
        assert_eq!(publish["with"]["attestations"], false);
    }
}

#[test]
fn runner_local_cache_paths_are_resolved_in_a_step_before_cache_and_toolchain_setup() {
    let github = workflow(include_str!(
        "../../../.github/workflows/build-exe-for-python-sdk.yml"
    ));
    let build = &github["jobs"]["build"];
    assert!(build["env"].is_null());
    let steps = build["steps"].as_array().unwrap();
    let setup = steps
        .iter()
        .position(|step| step["name"] == "Select task-local Rust cache directories")
        .unwrap();
    let script = steps[setup]["run"].as_str().unwrap();
    assert!(script.contains("CARGO_HOME=$RUNNER_TEMP/seekdeep-sdk-cargo"));
    assert!(script.contains("RUSTUP_HOME=$RUNNER_TEMP/seekdeep-sdk-rustup"));
    assert!(script.contains(">> \"$GITHUB_ENV\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("runner temporary");
        std::fs::create_dir(&root).unwrap();
        let environment = root.join("github-env");
        let status = std::process::Command::new("bash")
            .args(["-eu", "-c", script])
            .env("RUNNER_TEMP", &root)
            .env("GITHUB_ENV", &environment)
            .status()
            .unwrap();
        assert!(status.success());
        for name in ["seekdeep-sdk-cargo", "seekdeep-sdk-rustup"] {
            let metadata = std::fs::metadata(root.join(name)).unwrap();
            assert!(metadata.is_dir());
            assert_eq!(metadata.uid(), std::fs::metadata(&root).unwrap().uid());
        }
        let recorded = std::fs::read_to_string(environment).unwrap();
        assert!(recorded.contains(&format!("CARGO_HOME={}/seekdeep-sdk-cargo", root.display())));
        assert!(recorded.contains(&format!(
            "RUSTUP_HOME={}/seekdeep-sdk-rustup",
            root.display()
        )));
    }
    for action in ["actions/cache@v4", "dtolnay/rust-toolchain@1.93.1"] {
        let consumer = steps
            .iter()
            .position(|step| step["uses"] == action)
            .unwrap();
        assert!(setup < consumer, "cache paths must precede {action}");
    }
}

#[test]
fn native_builders_keep_manylinux_compilation_and_validation_on_the_same_pinned_images() {
    let github = workflow(include_str!(
        "../../../.github/workflows/build-exe-for-python-sdk.yml"
    ));
    let steps = github["jobs"]["build"]["steps"].as_array().unwrap();
    let build = steps
        .iter()
        .find(|step| step["name"] == "Build Rust executable against manylinux 2.28")
        .unwrap();
    assert_eq!(build["if"], "runner.os == 'Linux'");
    let script = build["run"].as_str().unwrap();
    for expected in [
        "docker run --rm",
        "target/manylinux",
        "-e CARGO_INCREMENTAL",
        "--bin build-exe-for-python-sdk",
        "--user",
        "rustup which --toolchain 1.93.1 cargo",
        "export PATH=\"$SEEKDEEP_RUST_BIN:$PATH\"",
    ] {
        assert!(script.contains(expected), "{expected}");
    }
    for name in ["MANYLINUX_X64_IMAGE", "MANYLINUX_ARM64_IMAGE"] {
        assert!(github["env"][name].as_str().unwrap().contains("@sha256:"));
    }
    let gitlab_text = include_str!("../../../.gitlab-ci.yml");
    assert!(!gitlab_text.contains("scripts/build-exe-for-python-sdk.ts"));
    assert!(!gitlab_text.contains("scripts/build-python-release.py"));
    assert!(!gitlab_text.contains("deepseek_harness_sdk-"));
    assert!(gitlab_text.contains("rustup which --toolchain 1.93.1 cargo"));
    assert!(gitlab_text.contains("mkdir -p \"$sdk_cargo_home\" \"$sdk_rustup_home\""));
    assert!(gitlab_text.contains("export PATH=\"$SEEKDEEP_RUST_BIN:$PATH\""));
    let gitlab = workflow(gitlab_text);
    for name in ["MANYLINUX_X64_IMAGE", "MANYLINUX_ARM64_IMAGE"] {
        assert_eq!(gitlab["variables"][name], github["env"][name]);
    }
    assert_eq!(gitlab["publish-python"]["resource_group"], "python-release");
}

#[test]
fn smoke_workflows_use_the_native_runner_and_installed_wheel_payloads() {
    let build_text = include_str!("../../../.github/workflows/build-exe-for-python-sdk.yml");
    let release_text = include_str!("../../../.github/workflows/python-release.yml");
    let gitlab_text = include_str!("../../../.gitlab-ci.yml");
    for text in [build_text, release_text, gitlab_text] {
        assert!(!text.contains("scripts/smoke-python-runtime.py"));
        assert!(text.contains("seekdeep-python-runtime-smoke"));
        assert!(text.contains("python_runtime_sdk_interrupt"));
        assert!(!text.contains("/Users/trevor/ws/deepseek-harness"));
        assert!(!text.contains("source_parity"));
    }
    let build = workflow(build_text);
    let steps = build["jobs"]["build"]["steps"].as_array().unwrap();
    let native = steps
        .iter()
        .find(|step| step["name"] == "Build Rust executable against manylinux 2.28")
        .unwrap()["run"]
        .as_str()
        .unwrap();
    assert!(native.contains("cargo build --locked -p seekdeep-python-runtime-smoke"));
    assert!(native.contains("--bin smoke-python-runtime --example python_runtime_sdk_interrupt"));
    let installed = steps
        .iter()
        .find(|step| {
            step["name"] == "Install only the SDK into a clean venv and validate all scenarios"
        })
        .unwrap()["run"]
        .as_str()
        .unwrap();
    assert!(installed.contains("bundled_runtime_path()"));
    assert!(installed.contains("--scenario all --exe \"$sdk_installed_runtime\""));
    let baseline = steps
        .iter()
        .find(|step| step["name"] == "Run wheel in a manylinux 2.28 container")
        .unwrap()["run"]
        .as_str()
        .unwrap();
    assert!(baseline.contains("/work/target/manylinux/debug/smoke-python-runtime --root /work"));
    assert!(baseline.contains("--python /tmp/seekdeep-sdk/bin/python --scenario all"));
    assert!(
        baseline.contains("/work/target/manylinux/debug/examples/python_runtime_sdk_interrupt")
    );
    assert!(gitlab_text.contains("/work/target/manylinux/debug/smoke-python-runtime --root /work"));
    assert!(gitlab_text.contains("--python /tmp/seekdeep-sdk/bin/python --scenario all"));
}
