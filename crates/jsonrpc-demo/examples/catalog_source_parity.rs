//! Compare compiled runtime registrations with the pinned source module exports.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "../src/runtime_catalog.rs"]
mod runtime_catalog;

const ORACLE: &str = r"
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
const root = process.argv[1];
const sourceRequire = createRequire(path.join(root, 'package.json'));
const { register } = await import(pathToFileURL(sourceRequire.resolve('tsx/esm/api')).href);
const loader = register({namespace:'seekdeep-catalog-oracle', tsconfig:path.join(root,'tsconfig.base.json')});
const files = execFileSync('git', ['-C', root, 'ls-files', 'packages/*/*/package.json', 'vendor/*/package.json'], {encoding:'utf8'}).trim().split('\n');
const packages = new Map(files.map(file => [JSON.parse(fs.readFileSync(path.join(root,file),'utf8')).name, file]));
const cases = JSON.parse(fs.readFileSync(0,'utf8'));
const results = [];
try {
for (const entry of cases) {
  const name = entry.package.replace('@seekdeep-ai/seekdeep-', '@deepseek-ai/dsh-').replace('@seekdeep-ai/', '@deepseek-ai/');
  const file = packages.get(name);
  if (!file) throw new Error(`source package ${name} is absent`);
  const namespace = await loader.import(pathToFileURL(path.join(root, path.dirname(file), 'src/index.ts')).href, pathToFileURL(path.join(root,'package.json')).href);
  const plugin = namespace.default ?? namespace;
  const inject = plugin.inject ?? [];
  const required = Array.isArray(inject) ? inject : Object.keys(inject);
  results.push({package:entry.package, loadable:typeof plugin === 'function' || typeof plugin.apply === 'function', name:plugin.name ?? null, inject:required});
}
console.log(JSON.stringify(results));
} finally { await loader.unregister(); }
";

fn main() -> anyhow::Result<()> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("pinned source root required"))?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin missing"))?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == pin,
        "oracle differs from SOURCE_SNAPSHOT"
    );
    let catalog = seekdeep_loader::PluginCatalog::new();
    runtime_catalog::register(&catalog, false, None)?;
    let manifest: Value =
        serde_json::from_str(include_str!("../../../python/sdk-runtime/package.json"))?;
    let mut cases = Vec::new();
    for &(name, factory) in runtime_catalog::FACTORIES {
        let package = format!("@seekdeep-ai/{name}");
        anyhow::ensure!(
            manifest["dependencies"].get(&package).is_some(),
            "factory {package} is outside the runtime manifest"
        );
        catalog.preflight_yaml(&format!("- name: '{package}'\n"))?;
        let plugin = factory();
        cases.push(json!({"package":package,"name":plugin.name(),"inject":plugin.inject()}));
    }
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", ORACLE])
        .arg(Path::new(&source))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("oracle input missing"))?
        .write_all(&serde_json::to_vec(&cases)?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(expected.len() == cases.len(), "oracle result count differs");
    let mut mismatches = Vec::new();
    for (actual, expected) in cases.iter().zip(expected) {
        if expected["loadable"] != true || actual["inject"] != expected["inject"] {
            mismatches.push(json!({"native":actual,"source":expected}));
        }
    }
    anyhow::ensure!(
        mismatches.is_empty(),
        "source registration differences: {}",
        serde_json::to_string_pretty(&mismatches)?
    );
    println!(
        "{} runtime factories resolve and match source required-service declarations",
        cases.len()
    );
    Ok(())
}
