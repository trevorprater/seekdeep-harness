//! Whole-command fixtures compare diagnostic order with the pinned source.

use std::{path::Path, process::Command};

use seekdeep_repository_tools::workspace_constraints::inspect_workspace_constraints;
use serde_json::{Value, json};

fn write(root: &Path, path: &str, value: &Value) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn package(name: &str, directory: &str) -> Value {
    json!({
        "name":name,"version":"1.2.3-rc.1","type":"module",
        "publishConfig":{"access":"public"},
        "repository":{"type":"git","url":"git+https://github.com/deepseek-ai/seekdeep-harness.git","directory":directory},
        "main":"lib/index.js","types":"lib/types/index.d.ts",
        "exports":{".":{"types":"./lib/types/index.d.ts","default":"./lib/index.js"}},
        "peerDependencies":{"@seekdeep-ai/cordis":"workspace:^"},
        "devDependencies":{"@seekdeep-ai/cordis":"workspace:^"},
        "files":["lib/index.js","lib/invariant.js","lib/types/**/*.d.ts"]
    })
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for directory in ["vendor", "packages", "native/landlock-run/packages", "apps"] {
        std::fs::create_dir_all(root.path().join(directory)).unwrap();
    }
    write(
        root.path(),
        "package.json",
        &json!({"name":"root","private":true,"version":"1.2.3-rc.1"}),
    );
    write(
        root.path(),
        "native/landlock-run/package.json",
        &json!({"name":"landlock","private":true,"version":"4.5.6"}),
    );
    write(
        root.path(),
        "vendor/cordis/package.json",
        &package("@seekdeep-ai/cordis", "vendor/cordis"),
    );
    write(
        root.path(),
        "packages/core/demo/package.json",
        &package("@seekdeep-ai/seekdeep-demo", "packages/core/demo"),
    );
    root
}

fn manifest_cases() -> Vec<(&'static str, &'static str, Value)> {
    let path = "packages/core/demo/package.json";
    let base = package("@seekdeep-ai/seekdeep-demo", "packages/core/demo");
    let mut cases = vec![("valid", path, base.clone())];
    cases.push((
        "flat-hierarchy",
        "packages/flat/package.json",
        json!({"private":true}),
    ));
    cases.push((
        "deep-hierarchy",
        "packages/core/nested/deeper/package.json",
        json!({"private":true}),
    ));
    cases.push((
        "ignored-artifact",
        "vendor/node_modules/package.json",
        json!({"private":false}),
    ));
    for (name, field, value) in [
        ("private", "private", json!(true)),
        ("access", "publishConfig", json!({"access":"restricted"})),
        ("repository", "repository", json!({})),
        ("version", "version", json!("1.2.4")),
        ("module", "type", json!("commonjs")),
        ("main", "main", json!("src/index.ts")),
        ("declarations", "types", json!("src/index.ts")),
        ("missing-peer", "peerDependencies", json!({})),
        ("missing-dev", "devDependencies", json!({})),
        (
            "mismatched-ranges",
            "devDependencies",
            json!({"@seekdeep-ai/cordis":"^1"}),
        ),
        (
            "raw-dependencies",
            "dependencies",
            json!({"@seekdeep-ai/cordis":"*"}),
        ),
        (
            "raw-optional",
            "optionalDependencies",
            json!({"@seekdeep-ai/cordis":"1"}),
        ),
        (
            "forbidden-payload",
            "files",
            json!(["src", "src/a.rs", "lib/a.js.map", "lib/a.d.ts.map"]),
        ),
        ("root-export", "exports", json!({".":"./lib/index.js"})),
        ("array-invariant", "exports", json!({"./invariant":[]})),
        (
            "partial-invariant",
            "exports",
            json!({"./invariant":{"types":"wrong"}}),
        ),
        (
            "null-invariant-fields",
            "exports",
            json!({"./invariant":{"types":null,"default":null}}),
        ),
        ("bin", "bin", json!({})),
        ("empty-bin", "bin", json!("")),
        ("worker", "exports", json!({"./worker":{}})),
        (
            "browser-bundles",
            "exports",
            json!({"./client":"./lib/client.js","./loader":{"default":"./lib/loader.js"},"./store":"./lib/store/index.js","./startup":"./lib/startup.js"}),
        ),
        (
            "emitted-tree",
            "exports",
            json!({"./client":"./lib/types/client.js"}),
        ),
        (
            "typert",
            "exports",
            json!({"./typert":{"types":"./lib/typert.host.d.ts","default":"./lib/typert.host.js"},"./client/typert":{"types":"./lib/typert.client.d.ts","default":"./lib/typert.client.js"},"./remote":{"types":"./lib/typert.remote-client.d.ts","default":"./lib/typert.remote-client.js"}}),
        ),
    ] {
        let mut manifest = base.clone();
        manifest[field] = value;
        cases.push((name, path, manifest));
    }
    cases
}

fn cases() -> Vec<(&'static str, &'static str, Value)> {
    let mut cases = manifest_cases();
    let path = "packages/core/demo/package.json";
    for name in [
        "base",
        "web-app",
        "headless",
        "client-ui-theme",
        "sdk-jsonrpc-demo",
        "sandbox-windows-acl",
        "skill-badge",
        "subprocess-local",
    ] {
        cases.push((
            name,
            path,
            package(
                &format!("@seekdeep-ai/seekdeep-{name}"),
                "packages/core/demo",
            ),
        ));
    }
    for (name, value) in [
        ("invalid-root-version", "1.2"),
        ("unicode-version", "١.2.3"),
        ("newline-version", "1.2.3\n"),
    ] {
        cases.push((
            name,
            "package.json",
            json!({"private":true,"version":value}),
        ));
    }
    for name in [
        "@seekdeep-ai/seekdeep",
        "@seekdeep-ai/seekdeep-web-frontend",
        "@seekdeep-ai/unknown",
    ] {
        cases.push((
            "application",
            "apps/web/package.json",
            package(name, "apps/web"),
        ));
    }
    for name in [
        "@seekdeep-ai/node-addon-landlock-run",
        "@seekdeep-ai/node-addon-landlock-run-linux-x64",
        "@seekdeep-ai/unexpected",
    ] {
        let mut value = package(name, "native/landlock-run/packages/main");
        value["files"] = json!(["src/main.c", "src/other.c"]);
        cases.push((
            "landlock",
            "native/landlock-run/packages/main/package.json",
            value,
        ));
    }
    cases
}

#[test]
fn malformed_publication_entries_fail_instead_of_being_ignored() {
    let root = fixture();
    let mut manifest = package(
        "@seekdeep-ai/node-addon-landlock-run",
        "native/landlock-run/packages/main",
    );
    manifest["files"] = json!([17]);
    write(
        root.path(),
        "native/landlock-run/packages/main/package.json",
        &manifest,
    );
    assert!(
        inspect_workspace_constraints(root.path())
            .unwrap_err()
            .to_string()
            .contains("publication file must be a string")
    );
}

#[test]
fn valid_workspace_is_silent_and_missing_discovery_roots_fail() {
    let root = fixture();
    assert!(
        inspect_workspace_constraints(root.path())
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir(root.path().join("apps")).unwrap();
    assert!(inspect_workspace_constraints(root.path()).is_err());
}

#[test]
fn invalid_manifest_rules_produce_diagnostics() {
    for (name, path, value) in cases() {
        let root = fixture();
        write(root.path(), path, &value);
        let errors = inspect_workspace_constraints(root.path()).unwrap();
        if matches!(name, "valid" | "empty-bin" | "ignored-artifact") {
            assert!(errors.is_empty(), "{name}: {errors:?}");
        } else {
            assert!(!errors.is_empty(), "{name}");
        }
    }
}

#[test]
#[ignore = "requires SEEKDEEP_PARITY_SOURCE and the pinned oracle's tsx dependencies"]
fn source_differential_workspace_constraints() {
    let source = std::path::PathBuf::from(
        std::env::var_os("SEEKDEEP_PARITY_SOURCE").expect("source checkout"),
    );
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(head.status.success());
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), pin);
    let script = std::fs::read_to_string(source.join("scripts/check-workspace-constraints.ts"))
        .unwrap()
        .replace("@deepseek-ai/", "@seekdeep-ai/")
        .replace("dsh-", "seekdeep-")
        .replace("/dsh'", "/seekdeep'")
        .replace("deepseek-harness", "seekdeep-harness")
        .replace(
            "const root = resolve(import.meta.dirname, '..')",
            "const root = resolve(process.argv[2])",
        )
        .replace(
            "./publication-payload.ts",
            source
                .join("scripts/publication-payload.ts")
                .to_str()
                .unwrap(),
        )
        .replace(
            "./project-reference-faces.ts",
            source
                .join("scripts/project-reference-faces.ts")
                .to_str()
                .unwrap(),
        );
    let driver = tempfile::tempdir().unwrap();
    let entry = driver.path().join("constraints.mts");
    std::fs::write(&entry, script).unwrap();
    let cases = cases();
    for (name, path, value) in &cases {
        let root = fixture();
        write(root.path(), path, value);
        let native = inspect_workspace_constraints(root.path()).unwrap();
        let expected = Command::new("node")
            .arg("--import")
            .arg(source.join("node_modules/tsx/dist/loader.mjs"))
            .arg(&entry)
            .arg(root.path())
            .output()
            .unwrap();
        assert!(
            expected.stdout.is_empty(),
            "{name}: unexpected source stdout"
        );
        assert_eq!(
            expected.status.code(),
            Some(i32::from(!native.is_empty())),
            "{name}: {}",
            String::from_utf8_lossy(&expected.stderr)
        );
        let diagnostics = if native.is_empty() {
            String::new()
        } else {
            format!("{}\n", native.join("\n"))
        };
        assert_eq!(
            String::from_utf8(expected.stderr).unwrap(),
            diagnostics,
            "{name}"
        );
    }
    println!(
        "{} whole-command manifest cases match the pinned source",
        cases.len()
    );
}
