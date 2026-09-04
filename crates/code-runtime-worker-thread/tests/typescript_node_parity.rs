//! Live Node differential for the pinned source's async-body type stripping.

use std::{
    io::Write as _,
    process::{Command, Stdio},
    sync::Arc,
};

use indexmap::IndexMap;
use seekdeep_code_runtime::{
    CodeBindingNamespace, CodeRunFailureKind, CodeRunRequest, CodeRuntimeBackend,
};
use seekdeep_code_runtime_worker_thread::{
    WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, typescript::strip_typescript,
};
use serde::Deserialize;
use serde_json::Value;

const INPUTS: &str = include_str!("fixtures/typescript_inputs.json");
const PREFIX: &str = "async function __seekdeep_program__() {\n";
const SUFFIX: &str = "\n}";
const NODE_ORACLE: &str = r"
const { stripTypeScriptTypes } = require('node:module');
const prefix = 'async function __dsh_program__() {\n';
const suffix = '\n}';
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { input += chunk });
process.stdin.on('end', () => {
  const cases = JSON.parse(input).map(({ name, program }) => {
    try {
      const code = stripTypeScriptTypes(prefix + program + suffix);
      return { name, program, status: 'success', code: code.slice(prefix.length, -suffix.length) };
    } catch (error) {
      return { name, program, status: 'failure', message: error.message };
    }
  });
  process.stdout.write(JSON.stringify(cases));
});
";

fn unexpected_binding(_: Value) -> futures::future::BoxFuture<'static, anyhow::Result<Value>> {
    panic!("invalid source must not reach a host binding")
}

#[derive(Deserialize)]
struct OracleCase {
    name: String,
    program: String,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum Outcome {
    Success { code: String },
    Failure { message: String },
}

fn oracle() -> Vec<OracleCase> {
    let mut child = Command::new("node")
        .args(["--no-warnings", "-e", NODE_ORACLE])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect(
            "Node from the source's supported engines range is required for differential tests",
        );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(INPUTS.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<OracleCase> = serde_json::from_slice(&output.stdout).unwrap();
    let inputs: Vec<Value> = serde_json::from_str(INPUTS).unwrap();
    assert_eq!(cases.len(), inputs.len());
    for (case, input) in cases.iter().zip(inputs) {
        assert_eq!(case.name, input["name"]);
        assert_eq!(case.program, input["program"]);
    }
    cases
}

#[test]
fn erasure_and_first_diagnostics_match_node_for_the_complete_corpus() {
    let mut failures = Vec::new();
    for case in oracle() {
        let expected = match case.outcome {
            Outcome::Success { code } => Ok(format!("{PREFIX}{code}{SUFFIX}")),
            Outcome::Failure { message } => Err(message),
        };
        let actual = strip_typescript(&case.program).map_err(|error| error.to_string());
        if actual != expected {
            failures.push(format!(
                "{}:\n  expected {expected:?}\n  actual   {actual:?}",
                case.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn syntax_failure_precedes_host_dispatch_and_worker_creation() {
    let runtime = WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig::default()).unwrap();
    let request = CodeRunRequest {
        program: "enum E { A }; return E.A".to_owned(),
        bindings: vec![CodeBindingNamespace {
            global: "tools".to_owned(),
            functions: IndexMap::from([(
                "never".to_owned(),
                Arc::new(unexpected_binding) as seekdeep_code_runtime::CodeBindingFunction,
            )]),
            error_class: None,
        }],
        signal: None,
    };
    // No Tokio runtime exists here: invalid syntax must settle before requiring one.
    let result = futures::executor::block_on(runtime.run(request)).unwrap();
    let error = result.error.unwrap();
    assert_eq!(error.kind, CodeRunFailureKind::Exception);
    assert_eq!(
        error.message,
        "TypeScript enum is not supported in strip-only mode"
    );
    assert!(result.logs.is_empty());
}
