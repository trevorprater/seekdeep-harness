//! Host package entry generation over manifests, the captured declaration model, and the no-op
//! catalog.

use std::{path::Path, process::Command};

use tempfile::TempDir;
use xtask::host_entries::run;

const MODEL: &str = r#"{"formatVersion":1,"sourceCommit":"0","modules":[
{"packageRoot":"packages/a/plugin","output":"packages/a/plugin/lib/types/invariant.d.ts","content":"import type { Context } from '@deepseek-ai/cordis';\n/** Cordis companion plugin name. */\nexport declare const name = \"plugin-invariant\";\nexport declare const inject: string[];\n"},
{"packageRoot":"packages/a/plugin","output":"packages/a/plugin/lib/types/index.d.ts","content":"export declare const name = \"plugin\";\n"},
{"packageRoot":"packages/a/service","output":"packages/a/service/lib/types/invariant.d.ts","content":"export declare const name = \"service-invariant\";\n"},
{"packageRoot":"packages/subagent/subagent-dsh-sdk","output":"packages/subagent/subagent-dsh-sdk/lib/types/invariant.d.ts","content":"export declare const name = \"subagent-dsh-sdk-invariant\";\n"}
],"packages":[]}
"#;

const CATALOG: &str = "//! Source-derived catalog.\n\npub const NOOP_INVARIANTS: &[NoopInvariantDescriptor] = &[\n    NoopInvariantDescriptor::new(\n        \"packages/a/plugin/src/invariant.ts\",\n        \"plugin-invariant\",\n        \"@seekdeep-ai/seekdeep-plugin\",\n    ),\n    NoopInvariantDescriptor::new(\n        \"packages/subagent/subagent-dsh-sdk/src/invariant.ts\",\n        \"subagent-seekdeep-sdk-invariant\",\n        \"@seekdeep-ai/seekdeep-subagent-seekdeep-sdk\",\n    ),\n];\n";

const PLUGIN: &str = r#"{
  "name": "@seekdeep-ai/seekdeep-plugin",
  "type": "module",
  "main": "lib/index.js",
  "types": "lib/types/index.d.ts",
  "bin": { "seekdeep-plugin": "lib/bin.js" },
  "exports": {
    ".": { "types": "./lib/types/index.d.ts", "default": "./lib/index.js" },
    "./invariant": { "types": "./lib/types/invariant.d.ts", "default": "./lib/invariant.js" },
    "./types": { "types": "./lib/types/types.d.ts", "default": "./lib/types/types.js" },
    "./worker": { "types": "./lib/types/worker.d.ts", "default": "./lib/worker.cjs" },
    "./typert": { "types": "./lib/typert.host.d.ts", "default": "./lib/typert.host.js" },
    "./remote": { "types": "./lib/typert.remote-client.d.ts", "default": "./lib/typert.remote-client.js" },
    "./src/*": "./src/*",
    "./package.json": "./package.json"
  }
}
"#;

const SERVICE: &str = r#"{
  "name": "@seekdeep-ai/seekdeep-service",
  "type": "module",
  "main": "lib/index.js",
  "exports": {
    ".": { "types": "./lib/types/index.d.ts", "default": "./lib/index.js" },
    "./invariant": { "types": "./lib/types/invariant.d.ts", "default": "./lib/invariant.js" }
  }
}
"#;

const SDK: &str = r#"{
  "name": "@seekdeep-ai/seekdeep-subagent-seekdeep-sdk",
  "type": "module",
  "exports": { "./invariant": { "default": "./lib/invariant.js" } }
}
"#;

const WIDGET: &str = r#"{
  "name": "@seekdeep-ai/seekdeep-client-widget",
  "type": "module",
  "main": "lib/index.js",
  "scripts": { "bundle": "cargo xtask wasm-package --package seekdeep-client-widget --artifact seekdeep_client_widget --module-id @seekdeep-ai/seekdeep-client-widget --out-dir packages/client/widget/lib" },
  "exports": { ".": { "default": "./lib/index.js" }, "./invariant": { "default": "./lib/invariant.js" } }
}
"#;

const RUNTIME: &str = r#"{
  "name": "@seekdeep-ai/seekdeep-client-runtime",
  "type": "module",
  "main": "lib/index.js",
  "exports": { ".": { "default": "./lib/index.js" }, "./invariant": { "default": "./lib/invariant.js" } }
}
"#;

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "crates/api-remotes-client/contracts/client-declarations.json",
        MODEL,
    );
    write(
        root.path(),
        "crates/invariants/src/noop/catalog.rs",
        CATALOG,
    );
    write(root.path(), "packages/a/plugin/package.json", PLUGIN);
    write(root.path(), "packages/a/service/package.json", SERVICE);
    write(
        root.path(),
        "packages/subagent/subagent-seekdeep-sdk/package.json",
        SDK,
    );
    write(root.path(), "packages/client/widget/package.json", WIDGET);
    write(root.path(), "packages/client/runtime/package.json", RUNTIME);
    write(root.path(), "packages/a/.staged/package.json", SERVICE);
    root
}

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn read(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join(relative)).unwrap()
}

fn error(result: anyhow::Result<impl Sized>) -> String {
    format!("{:#}", result.err().expect("expected failure"))
}

#[test]
fn writes_every_declared_runtime_entry_and_skips_client_builds() {
    let root = fixture();
    let output = tempfile::tempdir().unwrap();
    let report = run(root.path(), output.path(), false).unwrap();
    let inventory = report
        .packages
        .iter()
        .map(|package| (package.directory.as_str(), package.files.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        inventory,
        [
            (
                "packages/a/plugin",
                vec![
                    "lib/bin.js".to_owned(),
                    "lib/index.js".to_owned(),
                    "lib/invariant.js".to_owned(),
                    "lib/types/types.js".to_owned(),
                    "lib/worker.cjs".to_owned(),
                ],
            ),
            (
                "packages/a/service",
                vec!["lib/index.js".to_owned(), "lib/invariant.js".to_owned()],
            ),
            (
                "packages/subagent/subagent-seekdeep-sdk",
                vec!["lib/invariant.js".to_owned()],
            ),
        ]
    );
    assert_eq!(report.file_count(), 8);
    assert!(!output.path().join("packages/client").exists());
    assert!(
        !output
            .path()
            .join("packages/a/plugin/lib/typert.host.js")
            .exists()
    );
    assert!(
        !output
            .path()
            .join("packages/a/plugin/lib/typert.remote-client.js")
            .exists()
    );

    let index = read(output.path(), "packages/a/plugin/lib/index.js");
    assert!(index.starts_with(
        "// Generated by `cargo xtask host-entries` from packages/a/plugin/package.json; do not edit.\n"
    ));
    assert!(index.contains("export {};\n"));
    assert!(index.contains(
        "throw new Error(\"@seekdeep-ai/seekdeep-plugin/lib/index.js: this package runs inside the compiled seekdeep Host"
    ));
    let bin = read(output.path(), "packages/a/plugin/lib/bin.js");
    assert!(bin.starts_with("#!/usr/bin/env node\n// Generated by"));
    assert!(bin.contains("@seekdeep-ai/seekdeep-plugin/lib/bin.js:"));
    let worker = read(output.path(), "packages/a/plugin/lib/worker.cjs");
    assert!(worker.contains("'use strict';\nmodule.exports = {};\n"));
    assert!(!worker.contains("export {}"));

    let plugin = read(output.path(), "packages/a/plugin/lib/invariant.js");
    assert_eq!(
        plugin,
        "// Generated by `cargo xtask host-entries` from packages/a/plugin/package.json; do not edit.\n\
         const PACKAGE_NAME = \"@seekdeep-ai/seekdeep-plugin\";\n\
         export const name = \"plugin-invariant\";\n\
         export const inject = ['invariants'];\n\
         // The pinned source installer is a no-op (crates/invariants/src/noop/catalog.rs).\n\
         const install = () => {};\n\
         export const apply = ctx => Promise.resolve(ctx.invariants.register(PACKAGE_NAME, install));\n"
    );
    let service = read(output.path(), "packages/a/service/lib/invariant.js");
    assert!(service.contains("export const name = \"service-invariant\";\n"));
    assert!(service.contains(
        "const install = () => {\n  throw new Error(\"@seekdeep-ai/seekdeep-service: the service-invariant installer runs inside the compiled seekdeep Host"
    ));
    let sdk = read(
        output.path(),
        "packages/subagent/subagent-seekdeep-sdk/lib/invariant.js",
    );
    assert!(sdk.contains("export const name = \"subagent-seekdeep-sdk-invariant\";\n"));
    assert!(sdk.contains("const install = () => {};\n"));
}

#[test]
fn check_mode_reports_missing_and_stale_entries() {
    let root = fixture();
    let output = tempfile::tempdir().unwrap();
    assert!(error(run(root.path(), output.path(), true)).contains("packages/a/plugin/lib/bin.js"));
    run(root.path(), output.path(), false).unwrap();
    let verified = run(root.path(), output.path(), true).unwrap();
    assert_eq!(verified.file_count(), 8);
    write(
        output.path(),
        "packages/a/service/lib/index.js",
        "export const edited = true;\n",
    );
    std::fs::remove_file(output.path().join("packages/a/plugin/lib/worker.cjs")).unwrap();
    let message = error(run(root.path(), output.path(), true));
    assert!(message.starts_with("stale Host package entries; run `cargo xtask host-entries`:\n"));
    assert!(message.contains("packages/a/plugin/lib/worker.cjs\n"));
    assert!(message.ends_with("packages/a/service/lib/index.js"));
    assert!(!message.contains("packages/a/plugin/lib/index.js"));
}

#[test]
fn companion_without_canonical_declaration_or_with_disagreeing_catalog_is_rejected() {
    let root = fixture();
    let output = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "packages/a/orphan/package.json",
        SERVICE
            .replace("seekdeep-service", "seekdeep-orphan")
            .as_str(),
    );
    assert_eq!(
        error(run(root.path(), output.path(), false)),
        "packages/a/orphan: no canonical invariant companion declaration in crates/api-remotes-client/contracts/client-declarations.json"
    );
    std::fs::remove_dir_all(root.path().join("packages/a/orphan")).unwrap();
    write(
        root.path(),
        "crates/invariants/src/noop/catalog.rs",
        &CATALOG.replace("\"plugin-invariant\"", "\"renamed-invariant\""),
    );
    assert_eq!(
        error(run(root.path(), output.path(), false)),
        "packages/a/plugin: companion name \"plugin-invariant\" differs from the no-op catalog's \"renamed-invariant\""
    );
}

#[test]
fn entries_load_through_plain_node_with_the_documented_outcomes() {
    let root = fixture();
    let output = tempfile::tempdir().unwrap();
    run(root.path(), output.path(), false).unwrap();
    let script = r"
import { pathToFileURL } from 'node:url';
const directory = process.argv[1];
const load = relative => import(pathToFileURL(`${directory}/${relative}`).href);
const owned = [];
const ctx = { invariants: { register(packageName, install) { owned.push(packageName); install(); return () => {}; } } };
const plugin = await load('packages/a/plugin/lib/invariant.js');
if (plugin.name !== 'plugin-invariant' || !plugin.inject.includes('invariants') || 'default' in plugin) throw new Error('plugin companion shape');
if (typeof (await plugin.apply(ctx)) !== 'function') throw new Error('plugin companion disposer');
const service = await load('packages/a/service/lib/invariant.js');
await Promise.resolve().then(() => service.apply(ctx)).then(() => { throw new Error('service installer must fail'); }, error => {
  if (!error.message.includes('service-invariant installer runs inside the compiled seekdeep Host')) throw error;
});
if (owned.join(',') !== '@seekdeep-ai/seekdeep-plugin,@seekdeep-ai/seekdeep-service') throw new Error(`ownership ${owned}`);
for (const entry of ['packages/a/plugin/lib/index.js', 'packages/a/plugin/lib/worker.cjs', 'packages/a/plugin/lib/types/types.js']) {
  await load(entry).then(() => { throw new Error(`${entry} must fail to load`); }, error => {
    if (!error.message.startsWith(`@seekdeep-ai/seekdeep-plugin/${entry.replace('packages/a/plugin/', '')}: this package runs inside the compiled seekdeep Host`)) throw error;
  });
}
process.stdout.write('ok');
";
    let probe = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg("--")
        .arg(output.path())
        .output()
        .expect("node is available to the xtask tests");
    assert!(
        probe.status.success() && probe.stdout == b"ok",
        "{}{}",
        String::from_utf8_lossy(&probe.stdout),
        String::from_utf8_lossy(&probe.stderr)
    );
}

#[test]
fn repository_host_packages_all_receive_index_and_companion_entries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let output = tempfile::tempdir().unwrap();
    let report = run(&root, output.path(), false).unwrap();
    assert!(report.packages.len() > 100, "{}", report.packages.len());
    for package in &report.packages {
        assert!(
            package.files.contains(&"lib/index.js".to_owned())
                && package.files.contains(&"lib/invariant.js".to_owned()),
            "{}: {:?}",
            package.directory,
            package.files
        );
        assert!(
            !package.name.starts_with("@seekdeep-ai/seekdeep-client-"),
            "{} is a Client build",
            package.name
        );
    }
    let tools = read(output.path(), "packages/core/tools/lib/invariant.js");
    assert!(tools.contains("export const name = \"tools-invariant\";\n"));
    let sdk = read(
        output.path(),
        "packages/subagent/subagent-seekdeep-sdk/lib/invariant.js",
    );
    assert!(sdk.contains("export const name = \"subagent-seekdeep-sdk-invariant\";\n"));
    assert_eq!(run(&root, output.path(), true).unwrap(), report);
}
