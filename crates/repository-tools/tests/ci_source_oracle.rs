//! Source-dependent CI lanes share one pinned checkout and its executable artifacts.

use std::{path::Path, process::Command};

use serde_json::Value;

const ACTION: &str = "./.github/actions/setup-source-oracle";

fn yaml(text: &str) -> Value {
    serde_yml::from_str(text).unwrap()
}

fn setup() -> Value {
    yaml(include_str!(
        "../../../.github/actions/setup-source-oracle/action.yml"
    ))
}

fn source_before_gate(job: &Value, gate: &str) {
    let steps = job["steps"].as_array().unwrap();
    let source = steps
        .iter()
        .position(|step| step["uses"] == ACTION)
        .unwrap();
    let gate = steps.iter().position(|step| step["run"] == gate).unwrap();
    assert!(source < gate);
    assert!(steps[source]["if"].is_null());
    assert!(steps[source]["continue-on-error"].is_null());
}

#[test]
fn every_source_dependent_lane_prepares_the_oracle_before_running_its_gate() {
    let ci = yaml(include_str!("../../../.github/workflows/ci.yml"));
    for (name, gate) in [
        ("node-24", "pnpm run check:ci:static"),
        ("node-24-coverage", "pnpm run check:ci:coverage"),
        ("node-24-consumers", "pnpm run check:ci:consumers"),
        ("windows-native", "pnpm run check:ci:windows-complete"),
        ("serial-linux", "pnpm run check:ci:linux-primary"),
        ("serial-linux-selfhosted", "pnpm run check:ci:linux-primary"),
        ("serial-macos", "pnpm run check:ci"),
        ("serial-windows", "pnpm run check:ci:windows-complete"),
        ("consolidated-runner-benchmark", "pnpm run check:ci"),
    ] {
        source_before_gate(&ci["jobs"][name], gate);
    }
    for (workflow, name, gate) in [
        (
            include_str!("../../../.github/workflows/docs-pages.yml"),
            "build",
            "pnpm run doc-sync",
        ),
        (
            include_str!("../../../.github/workflows/e2e.yml"),
            "e2e",
            "pnpm run build",
        ),
        (
            include_str!("../../../.github/workflows/release.yml"),
            "pack",
            "pnpm run build",
        ),
        (
            include_str!("../../../.github/workflows/release-vendor.yml"),
            "pack",
            "pnpm run build:lib:host",
        ),
    ] {
        source_before_gate(&yaml(workflow)["jobs"][name], gate);
    }
    let setup = setup();
    let steps = setup["runs"]["steps"].as_array().unwrap();
    let checkout = steps
        .iter()
        .position(|step| step["id"] == "checkout")
        .unwrap();
    let install = steps
        .iter()
        .position(|step| {
            step["run"] == "pnpm --dir \"$SEEKDEEP_PARITY_SOURCE\" install --frozen-lockfile"
        })
        .unwrap();
    let build = steps
        .iter()
        .position(|step| step["run"] == "pnpm --dir \"$SEEKDEEP_PARITY_SOURCE\" run build:lib")
        .unwrap();
    assert!(checkout < install && install < build);
    assert_eq!(
        steps[checkout]["env"]["SOURCE_COMMIT"],
        "${{ steps.pin.outputs.commit }}"
    );
    assert!(steps.iter().all(|step| step["continue-on-error"].is_null()));
}

#[test]
fn docs_deployment_covers_rust_inputs_and_has_the_complete_build_toolchain() {
    let docs = yaml(include_str!("../../../.github/workflows/docs-pages.yml"));
    let branches = docs["on"]["push"]["branches"].as_array().unwrap();
    assert!(branches.contains(&Value::String("main".to_owned())));
    assert!(branches.contains(&Value::String("master".to_owned())));
    let paths = docs["on"]["push"]["paths"].as_array().unwrap();
    for source in [
        "crates/repository-tools/src/doc_site.rs",
        "crates/repository-tools/src/doc_site/prepare.rs",
        "crates/repository-runner/src/main.rs",
        "crates/docs-site-runtime/src/lib.rs",
        "crates/source-oracle/src/lib.rs",
        "xtask/src/main.rs",
        ".github/actions/setup-source-oracle/action.yml",
        "SOURCE_SNAPSHOT",
    ] {
        assert!(
            paths
                .iter()
                .any(|pattern| glob::Pattern::new(pattern.as_str().unwrap())
                    .unwrap()
                    .matches(source)),
            "{source}"
        );
    }
    let steps = docs["jobs"]["build"]["steps"].as_array().unwrap();
    let install = steps
        .iter()
        .position(|step| step["run"] == "pnpm install --frozen-lockfile")
        .unwrap();
    let setup = &steps[..install];
    assert!(
        setup
            .iter()
            .any(|step| step["uses"] == "dtolnay/rust-toolchain@1.93.1"
                && step["with"]["targets"] == "wasm32-unknown-unknown")
    );
    assert!(
        setup
            .iter()
            .any(|step| step["run"] == "cargo install --locked wasm-bindgen-cli --version 0.2.127")
    );
    assert!(setup.iter().any(|step| step["run"] == "pnpm --dir support/browser-dependencies install --ignore-workspace --frozen-lockfile --config.strictDepBuilds=false"));
}

fn execute(script: &str, root: &Path, output: &Path, environment: &Path) -> std::process::Output {
    Command::new("bash")
        .args(["-eu", "-c", script])
        .current_dir(root)
        .env("GITHUB_OUTPUT", output)
        .env("GITHUB_ENV", environment)
        .env("RUNNER_TEMP", root.parent().unwrap())
        .output()
        .unwrap()
}

#[test]
fn actual_action_scripts_reject_bad_pins_and_export_a_native_absolute_path() {
    let setup = setup();
    let steps = setup["runs"]["steps"].as_array().unwrap();
    let pin = steps.iter().find(|step| step["id"] == "pin").unwrap()["run"]
        .as_str()
        .unwrap();
    let location = steps.iter().find(|step| step["id"] == "location").unwrap()["run"]
        .as_str()
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("space and 中文");
    std::fs::create_dir(&root).unwrap();
    let output = root.join("output");
    let environment = root.join("environment");
    let revision = "0123456789abcdef0123456789abcdef01234567";
    std::fs::write(
        root.join("SOURCE_SNAPSHOT"),
        format!("repository=absent\ncommit={revision}\n"),
    )
    .unwrap();
    let result = execute(pin, &root, &output, &environment);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        format!("commit={revision}\n")
    );
    let result = execute(location, &root, &output, &environment);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let recorded = std::fs::read_to_string(&environment).unwrap();
    let path = Path::new(
        recorded
            .trim()
            .strip_prefix("SEEKDEEP_PARITY_SOURCE=")
            .unwrap(),
    );
    assert!(path.is_absolute());
    assert!(
        !path
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap())
    );
    assert!(path.is_dir());
    for snapshot in [
        "commit=short\n".to_owned(),
        "repository=missing\n".to_owned(),
        format!("commit={revision}\ncommit={revision}\n"),
    ] {
        std::fs::write(root.join("SOURCE_SNAPSHOT"), snapshot).unwrap();
        let before = std::fs::read(&output).unwrap();
        let result = execute(pin, &root, &output, &environment);
        assert!(!result.status.success());
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=CI oracle fixture",
            "-c",
            "user.email=oracle@example.invalid",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn actual_checkout_script_fetches_the_pin_outside_the_project() {
    let setup = setup();
    let steps = setup["runs"]["steps"].as_array().unwrap();
    let location = steps.iter().find(|step| step["id"] == "location").unwrap()["run"]
        .as_str()
        .unwrap();
    let checkout = steps.iter().find(|step| step["id"] == "checkout").unwrap()["run"]
        .as_str()
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project 中文");
    let original = temporary.path().join("source remote");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join(".gitattributes"), "original.txt -text\n").unwrap();
    std::fs::write(original.join("original.txt"), "pinned content\n").unwrap();
    git(&original, &["init", "--quiet"]);
    git(&original, &["add", "."]);
    git(&original, &["commit", "--quiet", "-m", "Source pin"]);
    let revision = git(&original, &["rev-parse", "HEAD"]);
    std::fs::write(original.join("original.txt"), "newer content\n").unwrap();
    git(&original, &["commit", "--quiet", "-am", "Newer source"]);

    let output = root.join("output");
    let environment = root.join("environment");
    let allocated = execute(location, &root, &output, &environment);
    assert!(
        allocated.status.success(),
        "{}",
        String::from_utf8_lossy(&allocated.stderr)
    );
    let environment = std::fs::read_to_string(&environment).unwrap();
    let source = Path::new(
        environment
            .trim()
            .strip_prefix("SEEKDEEP_PARITY_SOURCE=")
            .unwrap(),
    );
    // Redirect only the public Git remote to a local fixture; execute the action unchanged.
    let fetched = Command::new("bash")
        .args(["-eu", "-c", checkout])
        .current_dir(&root)
        .env("SEEKDEEP_PARITY_SOURCE", source)
        .env("SOURCE_COMMIT", &revision)
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!(
                "url.{}.insteadOf",
                original.to_string_lossy().replace('\\', "/")
            ),
        )
        .env(
            "GIT_CONFIG_VALUE_0",
            "https://github.com/deepseek-ai/deepseek-harness.git",
        )
        .output()
        .unwrap();
    assert!(
        fetched.status.success(),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    assert!(
        !source
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap())
    );
    assert_eq!(git(source, &["rev-parse", "HEAD"]), revision);
    assert_eq!(
        std::fs::read_to_string(source.join("original.txt")).unwrap(),
        "pinned content\n"
    );
    assert!(git(source, &["status", "--porcelain"]).is_empty());
    let detached = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(source)
        .status()
        .unwrap();
    assert!(!detached.success());
}
