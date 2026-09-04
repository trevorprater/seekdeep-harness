//! Source-authored supported/unsupported schema matrix and executable artifacts.

use std::{
    io::Write as _,
    process::{Command, Stdio},
};

use seekdeep_typert_generator::{emitter::FaceModelEmitter, model::FaceModel};
use serde_json::{Value, json};

fn cases() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("fixtures/source_schema_matrix.json")).unwrap()["cases"].as_array().unwrap().clone()
}

fn renamed(source: &str) -> String {
    source.replace(
        "@deepseek-ai/dsh-typert-generator",
        "@seekdeep-ai/seekdeep-typert-generator",
    )
}

#[test]
fn every_supported_and_unsupported_source_projection_matches() {
    for case in cases() {
        let face: FaceModel = serde_json::from_value(case["face"].clone()).unwrap();
        let actual = match FaceModelEmitter::new(&face).emit("@fixture/schema") {
            Ok(artifact) => json!({"ok":artifact.js}),
            Err(error) => json!({"error":{"name":error.name(),"message":error.to_string()}}),
        };
        let mut expected = case["outcome"].clone();
        if let Some(code) = expected.get_mut("ok") {
            *code = json!(renamed(code.as_str().unwrap()));
        }
        assert_eq!(actual, expected, "{}", case["name"]);
    }
}

const EXECUTE: &str = r"
const { z } = require('zod');
function decode(value) {
  if (Array.isArray(value)) return value.map(decode);
  if (!value || typeof value !== 'object') return value;
  if ('$undefined' in value) return undefined;
  if ('$bigint' in value) return BigInt(value.$bigint);
  if ('$symbol' in value) return Symbol(value.$symbol);
  if ('$function' in value) return () => undefined;
  if ('$date' in value) return new Date(value.$date);
  return Object.fromEntries(Object.entries(value).map(([key, value]) => [key, decode(value)]));
}
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => { input += chunk });
process.stdin.on('end', () => {
  const cases = JSON.parse(input);
  const failures = [];
  let assertions = 0;
  for (const item of cases) {
    const code = item.js.replace(/^import \{ z \} from 'zod'\n/m, '').replace(/^export const /gm, 'const ');
    const schema = new Function('z', code + '\nreturn Root;')(z);
    for (const expected of [true, false]) {
      const values = expected ? item.accepted : item.rejected;
      for (const value of values) {
        assertions += 1;
        if (schema.safeParse(decode(value)).success !== expected) failures.push({name:item.name,expected,value});
      }
    }
  }
  process.stdout.write(JSON.stringify({cases:cases.length, assertions, failures}));
});
";

#[test]
fn generated_schemas_execute_the_source_acceptance_and_rejection_cases() {
    let mut generated = Vec::new();
    for case in cases()
        .into_iter()
        .filter(|case| case["outcome"].get("ok").is_some())
    {
        let face: FaceModel = serde_json::from_value(case["face"].clone()).unwrap();
        let artifact = FaceModelEmitter::new(&face)
            .emit("@fixture/schema")
            .unwrap();
        generated.push(json!({"name":case["name"],"js":artifact.js,"accepted":case["accepted"],"rejected":case["rejected"]}));
    }
    let expected_count = generated.len();
    let expected_assertions = generated
        .iter()
        .map(|case| {
            case["accepted"].as_array().unwrap().len() + case["rejected"].as_array().unwrap().len()
        })
        .sum::<usize>();
    let mut child = Command::new("node")
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/typert/generator"),
        )
        .args(["-e", EXECUTE])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Node and the workspace Zod dependency are required for artifact execution");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&generated).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual["cases"], expected_count);
    assert_eq!(actual["assertions"], expected_assertions);
    assert_eq!(actual["failures"], json!([]));
}
