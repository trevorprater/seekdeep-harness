//! Differential validation of authored catalog models against the pinned projector.

use std::{
    error::Error,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "../tests/support/catalog_cases.rs"]
mod cases;

const ORACLE: &str = r"
const { resolve } = require('node:path');
const { createRequire } = require('node:module');
const root = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(root, 'package.json'));
sourceRequire('tsx/cjs');
const ts = sourceRequire('typescript');
const { CordisCatalogProjector, renderPageRegion, renderInheritedPage } = sourceRequire('./packages/typert/generator/src/cordis-catalog.ts');
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { input += chunk });
process.stdin.on('end', () => {
  const results = JSON.parse(input).map(item => {
    const policy = { ...item.policy, foundationTypeNames: new Set(item.policy.foundationTypeNames),
      ...(item.policy.runtimeServiceExclusions === undefined ? {} : { runtimeServiceExclusions: new Set(item.policy.runtimeServiceExclusions) }) };
    try {
      const projector = new CordisCatalogProjector(item.face, item.sourceDeclarations, policy);
      const model = projector.project();
      const runtime = projector.renderRuntimeApi(model);
      const region = renderPageRegion(item.page, model.services, model.events, policy);
      const module = {};
      new Function('exports', ts.transpileModule(runtime, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText)(module);
      const catalog = { services: module.SERVICE_API, events: module.EVENT_API, types: module.TYPE_API, inheritedContext: module.INHERITED_CTX_API };
      return { name: item.name, outcome: { ok: { model, runtime: runtime.replace('@deepseek-ai/dsh-tool-cordis', '@seekdeep-ai/seekdeep-tool-cordis'), catalog, region, inherited: renderInheritedPage(policy) } } };
    } catch (error) {
      return { name: item.name, outcome: { error: { name: error.name, message: error.message } } };
    }
  });
  process.stdout.write(JSON.stringify(results));
});
";

fn main() -> Result<(), Box<dyn Error>> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or("usage: catalog_cases <pinned-source-root>")?;
    let source = Path::new(&source);
    let head = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pinned = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("missing source pin")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pinned {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let input = cases::cases();
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
        .ok_or("missing oracle stdin")?
        .write_all(&serde_json::to_vec(&input)?)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    let mut failures = 0;
    for (case, expected) in input.iter().zip(&expected) {
        let actual = json!({"name":case["name"],"outcome":cases::outcome(case)});
        if let Some(difference) =
            difference(&actual, expected, case["name"].as_str().unwrap_or("case"))
        {
            eprintln!("{difference}");
            failures += 1;
        }
    }
    if failures > 0 || input.len() != expected.len() {
        return Err(format!("{failures} catalog cases differ").into());
    }
    println!(
        "{} catalog model cases match the live source projector",
        input.len()
    );
    Ok(())
}

fn difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    if actual == expected {
        return None;
    }
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            for (key, actual) in actual {
                if let Some(difference) = difference(
                    actual,
                    expected.get(key).unwrap_or(&Value::Null),
                    &format!("{path}.{key}"),
                ) {
                    return Some(difference);
                }
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                if let Some(difference) = difference(actual, expected, &format!("{path}[{index}]"))
                {
                    return Some(difference);
                }
            }
        }
        (Value::String(actual), Value::String(expected)) => {
            for (index, (actual, expected)) in actual.lines().zip(expected.lines()).enumerate() {
                if actual != expected {
                    return Some(format!(
                        "{path} line {}: actual {:?}, expected {:?}",
                        index + 1,
                        actual.chars().take(180).collect::<String>(),
                        expected.chars().take(180).collect::<String>()
                    ));
                }
            }
        }
        _ => {}
    }
    Some(format!("{path}: values or collection sizes differ"))
}
