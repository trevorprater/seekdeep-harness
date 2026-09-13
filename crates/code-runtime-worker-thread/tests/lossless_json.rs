//! Source-worker differentials across the real native binding and result boundary.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use indexmap::IndexMap;
use seekdeep_code_runtime::{
    CodeBindingErrorClass, CodeBindingFailure, CodeBindingFunction, CodeBindingNamespace,
    CodeJsonString, CodeJsonValue, CodeRunFailure, CodeRunRequest, CodeRuntimeBackend,
};
use seekdeep_code_runtime_worker_thread::{
    WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, worker_json::decode_code_json,
};
use serde::Deserialize;
use serde_json::json;

const NATIVE_VALUE: &str = r#"{"\ud800":"\udfff","nested":["\ud800"],"__proto__":{"x":1}}"#;
const NATIVE_ERROR: &str = r#""native \ud800 failure""#;

#[derive(Deserialize)]
struct SourceOutcome {
    value: Option<CodeJsonValue>,
    calls: Vec<CodeJsonValue>,
    logs: Vec<CodeJsonString>,
    error: Option<CodeRunFailure>,
}

fn source_worker() -> PathBuf {
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
    Path::new(source).join("packages/code-runtime/code-runtime-worker-thread/src/worker.ts")
}

fn source(program: &str) -> SourceOutcome {
    let mut child =
        Command::new(std::env::var_os("SEEKDEEP_NODE_BINARY").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/lossless_json_oracle.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    serde_json::to_writer(
        child.stdin.as_mut().unwrap(),
        &json!({
            "program": program,
            "sourceWorker": source_worker(),
            "nativeValue": NATIVE_VALUE,
            "nativeError": NATIVE_ERROR,
        }),
    )
    .unwrap();
    child.stdin.take().unwrap().flush().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "source fixture failed: {}",
        String::from_utf8(output.stderr).unwrap()
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

async fn differential(runtime: &WorkerThreadCodeRuntime, program: &str) {
    let expected = source(program);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut functions = IndexMap::<String, CodeBindingFunction>::new();
    for name in ["echo", "native", "reject"] {
        let observed = calls.clone();
        functions.insert(
            name.to_owned(),
            Arc::new(move |argument| {
                observed.lock().unwrap().push(argument.clone());
                Box::pin(async move {
                    match name {
                        "native" => Ok(CodeJsonValue::parse(NATIVE_VALUE.to_owned())?),
                        "reject" => Err(CodeBindingFailure {
                            message: CodeJsonString::parse(NATIVE_ERROR.to_owned())?,
                        }
                        .into()),
                        _ => Ok(argument),
                    }
                })
            }),
        );
    }
    let actual = runtime
        .run(CodeRunRequest {
            program: program.into(),
            bindings: vec![CodeBindingNamespace {
                global: "host".to_owned(),
                functions: functions
                    .into_iter()
                    .map(|(name, function)| (name.into(), function))
                    .collect(),
                error_class: Some(CodeBindingErrorClass {
                    name: "BindingError".to_owned(),
                    member_name_property: "member".into(),
                }),
            }],
            signal: None,
        })
        .await
        .unwrap();
    let expected_value = expected
        .value
        .as_ref()
        .map(|wire| decode_code_json(wire).unwrap());
    let expected_calls = expected
        .calls
        .iter()
        .map(|wire| decode_code_json(wire).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual.value, expected_value, "completion: {program}");
    assert_eq!(
        *calls.lock().unwrap(),
        expected_calls,
        "binding arguments: {program}"
    );
    assert_eq!(actual.logs, expected.logs, "captured text: {program}");
    assert_eq!(actual.error, expected.error, "failure: {program}");
    let serialized = serde_json::to_string(&actual).unwrap();
    assert_eq!(
        serde_json::from_str::<seekdeep_code_runtime::CodeRunResult>(&serialized).unwrap(),
        actual
    );
}

fn runtime() -> WorkerThreadCodeRuntime {
    WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
        compute_ms: Some(30_000.0),
        max_wall_ms: Some(60_000.0),
        max_output_bytes: Some(1_000_000.0),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn strings_keys_bindings_results_logs_and_rejections_match_the_source_worker() {
    let runtime = runtime();
    for program in [
        r"return '\ud800';",
        r"return ['\ud800', '\udfff', '\ud800a\udfff', '\ud83d\ude00', '\\ud800', '�', null, false, 1.5];",
        r"return await host.echo('\ud800');",
        r"const object = {}; Object.defineProperty(object, '\ud800', {get() {return '\udfff'}, enumerable: true}); object['�'] = 'replacement'; const value = await host.echo(object); return {value, keys: Object.keys(value).map(key => key.charCodeAt(0))};",
        r"const value = await host.native({request:'\ud800'}); return {value, key: Object.keys(value)[0].charCodeAt(0), valueType: typeof value['\ud800'], unit: value['\ud800'].charCodeAt(0)};",
        r"return await host.echo({'\ud800':'x','1':'one','01':'zero-one','0':'zero','�':'y'});",
        r"console.log('\ud800'); return '\udfff';",
        r"throw '\ud800';",
        r"try { await host.reject({reason:'\udfff'}); } catch (error) { return {name:error.name, message:error.message, member:error.member, unit:error.message.charCodeAt(7)}; }",
        "return -0;",
        "return () => 1;",
    ] {
        differential(&runtime, program).await;
    }
}

#[tokio::test]
async fn fourteen_thousand_levels_cross_source_and_native_binding_boundaries() {
    differential(&runtime(), r"let value = '\ud800'; for (let i = 0; i < 14000; i++) value = [value]; return await host.echo(value);").await;
}

#[tokio::test]
async fn invalid_native_numbers_are_rejected_before_worker_port_delivery() {
    let runtime = runtime();
    for raw in ["-0", "1e999", "-1e999", "[1,{\"value\":-0.0}]", "-1e-999"] {
        let value = CodeJsonValue::parse(raw.to_owned()).unwrap();
        let function: CodeBindingFunction = Arc::new(move |_| {
            let value = value.clone();
            Box::pin(async move { Ok(value) })
        });
        let actual = runtime
            .run(CodeRunRequest {
                program: include_str!("fixtures/native-number-reply.ts").into(),
                bindings: vec![CodeBindingNamespace {
                    global: "host".to_owned(),
                    functions: [("value".into(), function)].into(),
                    error_class: None,
                }],
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(actual.error, None, "{raw}: {actual:?}");
        assert_eq!(
            actual.value,
            Some(
                json!({
                    "received": {
                        "ok": false,
                        "hasValue": false,
                        "message": "binding resolution must be lossless JSON",
                    },
                    "rejection": "binding resolution must be lossless JSON",
                })
                .into()
            ),
            "native resolution: {raw}"
        );
    }
}
