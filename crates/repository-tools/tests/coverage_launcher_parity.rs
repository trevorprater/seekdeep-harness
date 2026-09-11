//! Public native coverage entry exercised by real Vitest and Istanbul.
#![cfg(unix)]

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use seekdeep_repository_tools::coverage_uncovered_locations::coverage_arguments;

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn defaults_and_caller_configuration_keep_their_vitest_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let reporter = root.join("reporter with spaces.cjs");
    let incoming = strings(&["fixture.spec.js", "--maxWorkers=6"]);
    let local = coverage_arguments(root, &reporter, &incoming, false);
    assert_eq!(
        &local[..4],
        strings(&[
            "run",
            "--coverage",
            "--coverage.reporter=text",
            "--coverage.reporter=html"
        ])
    );
    assert_eq!(&local[5..], incoming);
    let ci = coverage_arguments(root, &reporter, &incoming, true);
    assert!(!ci.contains(&OsString::from("--coverage.reporter=html")));
    assert_eq!(&ci[4..], incoming);
    for incoming in [
        strings(&["--config", "custom.mjs", "--testNamePattern", "with spaces"]),
        strings(&["--root=nested"]),
        strings(&["--no-config"]),
        strings(&["--coverage.reporter=json-summary"]),
    ] {
        let expected = [strings(&["run", "--coverage"]), incoming.clone()].concat();
        assert_eq!(
            coverage_arguments(root, &reporter, &incoming, false),
            expected
        );
    }
    std::fs::write(root.join("vitest.config.mts"), "export default {};\n").unwrap();
    assert_eq!(
        coverage_arguments(root, &reporter, &incoming, false),
        [strings(&["run", "--coverage"]), incoming].concat()
    );
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        PathBuf::from,
    );
    for name in ["vitest", "@vitest/coverage-v8", "istanbul-lib-report"] {
        let destination = root.join("node_modules").join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(source.join("node_modules").join(name), destination).unwrap();
    }
    std::fs::write(root.join("package.json"), "{\"name\":\"coverage-fixture\",\"type\":\"module\",\"private\":true,\"devDependencies\":{\"vitest\":\"4.1.8\",\"@vitest/coverage-v8\":\"4.1.8\",\"istanbul-lib-report\":\"3.0.1\"}}\n").unwrap();
    std::fs::write(
        root.join("branch.js"),
        "export function branch(value) {\n  if (value) return 1;\n  return 2;\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("branch.spec.js"), "import { test, expect } from 'vitest';\nimport { branch } from './branch.js';\ntest('hits selected branch', () => expect(branch(true)).toBe(1));\ntest('must stay filtered', () => { throw new Error('testNamePattern was lost'); });\n").unwrap();
    directory
}

fn run(root: &Path, report: &str, ci: &str, extra: &[&str], prerequisite: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_run-coverage"));
    command
        .current_dir(root)
        .env("CI", ci)
        .env("NO_COLOR", "1")
        .env("CARGO_NET_OFFLINE", "true")
        .args([
            "branch.spec.js",
            "--testNamePattern=hits selected",
            "--maxWorkers=1",
            "--coverage.include=branch.js",
            "--no-cache",
        ])
        .arg(format!(
            "--coverage.reportsDirectory={}",
            root.join(report).display()
        ))
        .args(extra);
    if prerequisite {
        command.env_remove("SEEKDEEP_COVERAGE_REPORT_BIN");
    } else {
        command.env(
            "SEEKDEEP_COVERAGE_REPORT_BIN",
            env!("CARGO_BIN_EXE_coverage-uncovered-locations"),
        );
    }
    command.output().unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn actual_vitest_entry_builds_reporter_selects_ci_html_and_keeps_overrides() {
    let directory = fixture();
    let root = directory.path();
    let ci = run(root, "ci", "0", &[], true);
    assert_success(&ci);
    let text = String::from_utf8_lossy(&ci.stdout);
    assert!(
        text.contains("branch.js:") && text.contains("uncovered statement"),
        "{text}"
    );
    assert!(!root.join("ci/index.html").exists());
    let local = run(root, "local", "", &[], false);
    assert_success(&local);
    assert!(root.join("local/index.html").is_file());
    assert!(String::from_utf8_lossy(&local.stdout).contains("uncovered statement"));
    let explicit = run(
        root,
        "explicit",
        "",
        &["--coverage.reporter=json-summary"],
        false,
    );
    assert_success(&explicit);
    assert!(root.join("explicit/coverage-summary.json").is_file());
    assert!(!String::from_utf8_lossy(&explicit.stdout).contains("uncovered statement"));
    let failed = run(
        root,
        "failed",
        "1",
        &["--coverage.thresholds.lines=100"],
        false,
    );
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stdout).contains("uncovered statement"));
    std::fs::write(root.join("custom.config.mjs"), "import { writeFileSync } from 'node:fs';\nexport default { test: { coverage: { reporter: ['json-summary'] } }, plugins: [{ name: 'fixture-config', configResolved(config) { writeFileSync('config-observed.json', JSON.stringify({ path: config.configFile, mode: config.mode })); } }] };\n").unwrap();
    let configured = run(
        root,
        "configured",
        "",
        &[
            "--config",
            "custom.config.mjs",
            "--configLoader=native",
            "--mode=coverage-fixture",
        ],
        false,
    );
    assert_success(&configured);
    assert!(root.join("configured/coverage-summary.json").is_file());
    assert!(!String::from_utf8_lossy(&configured.stdout).contains("uncovered statement"));
    let observed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("config-observed.json")).unwrap()).unwrap();
    assert!(
        observed["path"]
            .as_str()
            .unwrap()
            .ends_with("custom.config.mjs")
    );
    assert_eq!(observed["mode"], "coverage-fixture");
    println!(
        "real Vitest native coverage entry passed: compiled reporter prerequisite, clickable uncovered lines, truthy CI/no HTML, local HTML, caller reporter/config/filter/mode preservation, threshold exit1"
    );
}

#[test]
fn pnpm_public_coverage_script_reaches_the_native_reporter() {
    let directory = fixture();
    let root = directory.path();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("pnpm")
        .current_dir(&repository)
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("COREPACK_ENABLE_NETWORK", "0")
        .env("CARGO_NET_OFFLINE", "true")
        .env("NODE_PATH", root.join("node_modules"))
        .env(
            "SEEKDEEP_COVERAGE_REPORT_BIN",
            env!("CARGO_BIN_EXE_coverage-uncovered-locations"),
        )
        .args(["run", "test:coverage", "--root"])
        .arg(root)
        .args([
            "branch.spec.js",
            "--testNamePattern=hits selected",
            "--coverage.include=branch.js",
            "--maxWorkers=1",
            "--no-cache",
            "--coverage.reporter=text",
        ])
        .arg(format!(
            "--coverage.reporter={}",
            repository
                .join("scripts/coverage-uncovered-locations.cjs")
                .display()
        ))
        .arg(format!(
            "--coverage.reportsDirectory={}",
            root.join("public").display()
        ))
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("uncovered statement"));
    println!(
        "actual pnpm test:coverage -> Cargo -> native launcher -> Vitest -> Istanbul -> native reporter passed"
    );
}
