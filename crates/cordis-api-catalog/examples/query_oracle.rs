//! Exercises generated source query functions with caller-supplied catalog records.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use seekdeep_cordis_api_catalog::{query_event_api, query_service_api};
use serde_json::{Value, json};

const ORACLE: &str = r"
const { resolve } = require('node:path');
const { createRequire } = require('node:module');
const root = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(root, 'package.json'));
sourceRequire('tsx/cjs');
const source = sourceRequire('./packages/extensions/tool-cordis/src/api-catalog.ts');
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { input += chunk });
process.stdin.on('end', () => {
  const cases = JSON.parse(input);
  process.stdout.write(JSON.stringify(cases.map(item => {
    source.TYPE_API.splice(0, source.TYPE_API.length, ...item.types);
    try {
      return { ok: item.kind === 'service' ? source.queryServiceApi(item.key, item.entries) : source.queryEventApi(item.key, item.entries) };
    } catch (error) { return { error: error.message }; }
  })));
});
";

fn main() -> anyhow::Result<()> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: query_oracle <pinned-source-root>"))?;
    let source = Path::new(&source);
    let head = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pinned = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("missing source pin"))?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == pinned,
        "oracle differs from SOURCE_SNAPSHOT"
    );
    let cases = cases();
    let mut child = Command::new("node")
        .args(["-e", ORACLE])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing oracle stdin"))?
        .write_all(&serde_json::to_vec(&cases)?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(expected.len() == cases.len(), "source result count differs");
    for (index, (case, expected)) in cases.iter().zip(expected).enumerate() {
        let key = case.get("key").and_then(Value::as_str);
        let entries = case["entries"].as_array().expect("case records");
        let types = case["types"].as_array().expect("case types");
        let actual = if case["kind"] == "service" {
            query_service_api(key, entries, types)
        } else {
            query_event_api(key, entries, types)
        };
        let actual = match actual {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"error":error.to_string()}),
        };
        anyhow::ensure!(
            actual == expected,
            "query {index} differs: {actual} != {expected}"
        );
    }
    println!(
        "{} caller-supplied catalog queries match the live source",
        cases.len()
    );
    Ok(())
}

fn cases() -> Vec<Value> {
    let types = json!([
        {"name":"Beta","declaration":"interface Beta { next: \u{8}Alpha\u{8} }"},
        {"name":"Alpha","declaration":"interface Alpha { next: \u{8}Beta\u{8} }"},
        {"name":"unused","declaration":"type unused = number"},
        {"name":"Alpha","declaration":"duplicate name retained in final source order"}
    ]);
    let mut cases = Vec::new();
    for key in ["plain", "", "quoted-\"key", "$valid_1", "名"] {
        let methods = json!([{"signature":"run(value: \u{8}Alpha\u{8}): void","description":"Run.","parameters":[],"extra":{"kept":true}}]);
        let entries =
            json!([{"key":key,"summary":"Short.","description":"Complete.","methods":methods}]);
        cases.push(json!({"kind":"service","entries":entries,"types":types}));
        cases.push(json!({"kind":"service","key":key,"entries":entries,"types":types}));
    }
    for signature in [
        "event(value: Alpha): void",
        "event(value: \u{8}Alpha\u{8}): void",
    ] {
        let entries = json!([{"name":"changed","mode":"emit","summary":"Short.","description":"Complete.","signature":signature,"parameters":[]}]);
        cases.push(json!({"kind":"event","entries":entries,"types":types}));
        cases.push(json!({"kind":"event","key":"changed","entries":entries,"types":types}));
    }
    for kind in ["service", "event"] {
        cases.push(json!({"kind":kind,"key":"missing","entries":[],"types":types}));
    }
    cases
}
