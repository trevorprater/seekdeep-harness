//! Complete generic binding metadata compared with the pinned runtime implementation.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use indexmap::IndexMap;
use seekdeep_code_runtime::{
    CodeBindingErrorClass, CodeBindingFailure, CodeBindingFunction, CodeBindingNamespace,
    CodeJsonString, CodeJsonValue, CodeRunRequest, CodeRunResult, CodeRuntimeBackend,
};
use seekdeep_code_runtime_worker_thread::{WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Scenario {
    name: String,
    global: String,
    class_name: Option<String>,
    property: CodeJsonString,
    names: Vec<CodeJsonString>,
    program: CodeJsonString,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<CodeJsonValue>,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct Call {
    name: CodeJsonString,
    args: CodeJsonValue,
}

#[derive(Debug, Deserialize)]
struct Observation {
    name: String,
    result: Option<CodeRunResult>,
    rejected: Option<CodeJsonString>,
    calls: Vec<Call>,
}

fn scenario(
    name: &str,
    global: &str,
    class_name: Option<&str>,
    property: CodeJsonString,
    mutate: bool,
) -> Scenario {
    let constructor = class_name.unwrap_or("globalThis.Error");
    let program = format!(
        r"
const {{ workerData }} = await import('node:worker_threads');
const __namespace = {global};
const __errorCtor = {constructor};
const boot = globalThis.structuredClone(workerData.namespaces[0]);
if ({mutate}) {{
  workerData.namespaces[0].errorClass.name = 'changed\ud800';
  workerData.namespaces[0].errorClass.memberNameProperty = '\ud800';
}}
const property = workerData.namespaces[0].errorClass?.memberNameProperty;
const names = globalThis.Object.keys(__namespace);
const records = [];
for (const name of names) {{
  try {{ await __namespace[name]({{ requested: name }}); }} catch (error) {{
    records.push({{
      name, typed: error instanceof __errorCtor, errorName: error.name, message: error.message,
      own: property === undefined ? false : globalThis.Object.hasOwn(error, property),
      descriptor: property === undefined ? null : globalThis.Object.getOwnPropertyDescriptor(error, property),
    }});
  }}
}}
return {{ names, boot, constructorName: __errorCtor.name,
  functionRealm: __errorCtor instanceof globalThis.Function,
  nullPrototype: globalThis.Object.getPrototypeOf(__namespace) === null,
  records }};
"
    );
    Scenario {
        name: name.to_owned(),
        global: global.to_owned(),
        class_name: class_name.map(str::to_owned),
        property,
        names: vec![
            "2".into(),
            "ordinary".into(),
            "1".into(),
            CodeJsonString::from_utf16(&[0xd800]),
            CodeJsonString::from_utf16(&[0xdfff]),
            "\\ud800".into(),
            "__proto__".into(),
            "constructor".into(),
            "hasOwnProperty".into(),
            "".into(),
            "4294967295".into(),
            "4294967294".into(),
        ],
        program: program.into(),
        resolution: None,
    }
}

fn cases() -> Vec<Scenario> {
    let mut cases = Vec::new();
    for (name, property) in [
        ("ordinary", "member".into()),
        ("surrogate", CodeJsonString::from_utf16(&[0xdfff])),
        ("constructor", "constructor".into()),
        ("cause", "cause".into()),
        ("empty-dunder", "____".into()),
        ("lf", "__\n__".into()),
        ("cr", "__\r__".into()),
        ("line-separator", "__\u{2028}__".into()),
        ("paragraph-separator", "__\u{2029}__".into()),
        ("trailing-newline", "__x__\n".into()),
        ("literal-escape", "\\ud800".into()),
        ("nul", "\0".into()),
        ("empty-rejected", "".into()),
        ("stack-rejected", "stack".into()),
        ("dunder-rejected", "__x__".into()),
        (
            "surrogate-dunder-rejected",
            CodeJsonString::from_utf16(&[95, 95, 0xd800, 95, 95]),
        ),
    ] {
        cases.push(scenario(name, "api", Some("BindingError"), property, false));
    }
    cases.push(scenario(
        "live-descriptor",
        "api",
        Some("BindingError"),
        "initial".into(),
        true,
    ));
    cases.push(scenario("untyped", "api", None, "unused".into(), false));
    for (name, global, class) in [
        ("builtin-global", "Function", "BindingError"),
        ("builtin-class", "api", "Error"),
        ("reserved-global", "console", "BindingError"),
        ("invalid-global", "bad name", "BindingError"),
        ("portable-global", "lambda", "BindingError"),
        ("invalid-class", "api", "Bad Name"),
        ("reserved-class", "api", "console"),
        ("colliding-class", "api", "api"),
    ] {
        cases.push(scenario(name, global, Some(class), "member".into(), false));
    }
    for raw in ["-0", "1e999", "-1e999", "[1,{\"value\":-0.0}]", "-1e-999"] {
        cases.push(Scenario {
            name: format!("native-number-{raw}"),
            global: "host".to_owned(),
            class_name: None,
            property: "unused".into(),
            names: vec!["value".into()],
            program: include_str!("fixtures/native-number-reply.ts").into(),
            resolution: Some(CodeJsonValue::parse(raw.to_owned()).unwrap()),
        });
    }
    cases
}

fn source_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let pin = std::fs::read_to_string(root.join("SOURCE_SNAPSHOT")).unwrap();
    let source = pin
        .lines()
        .find_map(|line| line.strip_prefix("repository="))
        .unwrap();
    let commit = pin
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(source)
        .output()
        .unwrap();
    assert!(head.status.success());
    assert_eq!(String::from_utf8(head.stdout).unwrap().trim(), commit);
    source.into()
}

fn source(cases: &[Scenario]) -> Vec<Observation> {
    let source = source_root();
    let directory =
        std::env::temp_dir().join(format!("seekdeep-binding-metadata-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let input = directory.join("input.json");
    let output = directory.join("output.json");
    std::fs::write(&input, serde_json::to_vec(cases).unwrap()).unwrap();
    let run =
        Command::new(std::env::var_os("SEEKDEEP_NODE_BINARY").unwrap_or_else(|| "node".into()))
            .arg(source.join("node_modules/vitest/vitest.mjs"))
            .args(["run", "--config"])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/binding-metadata.vitest.config.mjs"),
            )
            .env("SEEKDEEP_SOURCE_ORACLE_ROOT", &source)
            .env("SEEKDEEP_BINDING_METADATA_INPUT", &input)
            .env("SEEKDEEP_BINDING_METADATA_OUTPUT", &output)
            .current_dir(&source)
            .output()
            .unwrap();
    assert!(
        run.status.success(),
        "source metadata oracle failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let result = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    result
}

#[tokio::test]
async fn arbitrary_member_names_error_properties_boot_data_and_validation_match_source() {
    let cases = cases();
    let expected = source(&cases);
    assert_eq!(expected.len(), cases.len());
    let runtime = WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
        compute_ms: Some(10_000.0),
        max_wall_ms: Some(20_000.0),
        max_output_bytes: Some(1_000_000.0),
        ..Default::default()
    })
    .unwrap();
    for (case, expected) in cases.into_iter().zip(expected) {
        assert_eq!(case.name, expected.name);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut functions = IndexMap::<CodeJsonString, CodeBindingFunction>::new();
        for name in case.names {
            let recorded = calls.clone();
            let resolution = case.resolution.clone();
            functions.insert(
                name.clone(),
                Arc::new(move |args| {
                    recorded.lock().unwrap().push(Call {
                        name: name.clone(),
                        args,
                    });
                    let resolution = resolution.clone();
                    Box::pin(async move {
                        if let Some(value) = resolution {
                            return Ok(value);
                        }
                        Err(CodeBindingFailure {
                            message: CodeJsonString::from_utf16(&[
                                102, 97, 105, 108, 117, 114, 101, 32, 0xd800,
                            ]),
                        }
                        .into())
                    })
                }),
            );
        }
        let actual = runtime
            .run(CodeRunRequest {
                program: case.program,
                bindings: vec![CodeBindingNamespace {
                    global: case.global,
                    functions,
                    error_class: case.class_name.map(|name| CodeBindingErrorClass {
                        name,
                        member_name_property: case.property,
                    }),
                }],
                signal: None,
            })
            .await;
        match (actual, expected.result, expected.rejected) {
            (Ok(actual), Some(expected), None) => assert_eq!(actual, expected, "{}", case.name),
            (Err(actual), None, Some(expected)) => {
                let expected = expected.try_into_string().unwrap();
                let expected = expected
                    .strip_prefix("dsh-code-runtime-worker-thread:")
                    .map_or_else(
                        || expected.clone(),
                        |suffix| format!("seekdeep-code-runtime-worker-thread:{suffix}"),
                    );
                assert_eq!(actual.to_string(), expected, "{}", case.name);
            }
            (actual, result, rejected) => panic!(
                "{}: actual {actual:?}, source result {result:?}, source rejected {rejected:?}",
                case.name
            ),
        }
        assert_eq!(
            *calls.lock().unwrap(),
            expected.calls,
            "{} calls",
            case.name
        );
    }
}
