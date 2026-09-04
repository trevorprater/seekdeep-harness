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
