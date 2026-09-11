//! Source worker lifecycle and hostile-program cases through the shipped Node backend.

use std::sync::Arc;

use indexmap::IndexMap;
use seekdeep_code_runtime::{
    CodeBindingErrorClass, CodeBindingFunction, CodeBindingNamespace, CodeJsonString,
    CodeRunFailureKind, CodeRunRequest, CodeRuntimeBackend,
};
use seekdeep_code_runtime_worker_thread::{WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig};
use serde_json::{Value, json};

fn runtime(max_output_bytes: Option<f64>) -> WorkerThreadCodeRuntime {
    WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
        compute_ms: Some(10_000.0),
        max_wall_ms: Some(30_000.0),
        max_output_bytes,
        ..Default::default()
    })
    .unwrap()
}

fn request(program: &str) -> CodeRunRequest {
    CodeRunRequest {
        program: program.into(),
        bindings: Vec::new(),
        signal: None,
    }
}

#[tokio::test]
async fn worker_boot_names_follow_javascript_own_property_order() {
    let mut functions: IndexMap<String, CodeBindingFunction> = IndexMap::new();
    for name in ["2", "ordinary", "1", "01", "4294967295", "4294967294"] {
        functions.insert(
            name.to_owned(),
            Arc::new(|value| Box::pin(async move { Ok(value) })),
        );
    }
    let mut request = request(
        "const {workerData} = await import('node:worker_threads'); return workerData.namespaces[0].names;",
    );
    request.bindings.push(CodeBindingNamespace {
        global: "tools".to_owned(),
        functions: functions
            .into_iter()
            .map(|(name, function)| (name.into(), function))
            .collect(),
        error_class: None,
    });
    let result = runtime(None).run(request).await.unwrap();
    assert_eq!(result.error, None);
    assert_eq!(
        result.value,
        Some(json!(["1", "2", "4294967294", "ordinary", "01", "4294967295"]).into())
    );
}

#[tokio::test]
async fn explicit_worker_exit_keeps_the_exit_code_and_runtime_usable() {
    let runtime = runtime(None);
    let result = runtime.run(request("process.exit(7);")).await.unwrap();
    let failure = result.error.unwrap();
    assert_eq!(failure.kind, CodeRunFailureKind::WorkerExit);
    assert_eq!(
        failure.message,
        "worker exited with code 7 before completing"
    );
    assert_eq!(
        runtime.run(request("return 'alive';")).await.unwrap().value,
        Some(json!("alive").into())
    );
}

#[tokio::test]
async fn preserves_binding_and_completion_json_after_model_mutates_boundary_globals() {
    let mut functions: IndexMap<String, CodeBindingFunction> = IndexMap::new();
    functions.insert(
        "echo".to_owned(),
        Arc::new(|value| Box::pin(async move { Ok(value) })),
    );
    functions.insert(
        "fail".to_owned(),
        Arc::new(|_| Box::pin(async { anyhow::bail!("nope") })),
    );
    let mut request = request(
        r"
const realm = { binding: tools.echo instanceof Function, errorClass: ToolCallError instanceof Function, error: new ToolCallError('echo', 'probe') instanceof Error };
const arrayPrototype = Array.prototype;
const objectPrototype = Object.prototype;
const setPrototype = Set.prototype;
const stringPrototype = String.prototype;
Array.isArray = () => false;
arrayPrototype.at = arrayPrototype.includes = arrayPrototype.pop = arrayPrototype.push = () => { throw new Error('mutated array method') };
Object.defineProperty = Object.getOwnPropertyDescriptor = Object.getPrototypeOf = Object.keys = () => { throw new Error('mutated object method') };
Object.hasOwn = () => false;
Object.is = () => true;
objectPrototype.propertyIsEnumerable = () => false;
Number.isFinite = Number.isSafeInteger = () => false;
Reflect.apply = Reflect.ownKeys = () => { throw new Error('mutated reflect method') };
setPrototype.add = setPrototype.delete = setPrototype.has = () => { throw new Error('mutated set method') };
stringPrototype.charCodeAt = stringPrototype.codePointAt = stringPrototype.slice = () => { throw new Error('mutated string method') };
Buffer.byteLength = () => 0;
Function.prototype.toString = () => 'mutated';
objectPrototype.get = () => undefined;
objectPrototype.constructor = arrayPrototype.constructor = null;
globalThis.Array = globalThis.Buffer = globalThis.Error = globalThis.Function = globalThis.Number = globalThis.Object = globalThis.Reflect = globalThis.Set = globalThis.String = undefined;
const echoed = await tools.echo({ request: ['€', 1] });
let failure;
try { await tools.fail({}) } catch (error) {
  failure = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message };
}
return { echoed, failure, completion: { ok: true, amount: 42 }, realm };
",
    );
    request.bindings.push(CodeBindingNamespace {
        global: "tools".to_owned(),
        functions: functions
            .into_iter()
            .map(|(name, function)| (name.into(), function))
            .collect(),
        error_class: Some(CodeBindingErrorClass {
            name: "ToolCallError".to_owned(),
            member_name_property: "toolName".into(),
        }),
    });
    let result = runtime(None).run(request).await.unwrap();
    assert_eq!(result.error, None, "{result:?}");
    assert_eq!(result.logs, Vec::<String>::new());
    assert_eq!(
        result.value,
        Some(json!({
            "echoed": {"request": ["€", 1]},
            "failure": {"typed": true, "name": "ToolCallError", "toolName": "fail", "message": "nope"},
            "completion": {"ok": true, "amount": 42},
            "realm": {"binding": true, "errorClass": true, "error": true}
        }).into())
    );
}

#[tokio::test]
async fn drains_real_pipe_bytes_queued_before_terminal_message() {
    let result = runtime(Some(200_000.0))
        .run(request(
            r"
const { parentPort } = await import('node:worker_threads');
const write = text => Object.getPrototypeOf(process.stdout).write.call(process.stdout, text);
write('late-pipe-' + 'x'.repeat(100_000));
parentPort.postMessage({ type: 'done', value: ['done'] });
for (;;) {}
",
        ))
        .await
        .unwrap();
    assert_eq!(result.error, None, "{result:?}");
    assert_eq!(result.value, Some(json!("done").into()));
    assert_eq!(
        CodeJsonString::join(&result.logs, ""),
        format!("late-pipe-{}", "x".repeat(100_000))
    );
}

#[tokio::test]
async fn pipe_overflow_preserves_a_bounded_prefix_and_overrides_completion() {
    let result = runtime(Some(80.0))
        .run(request(
            r"
const write = text => Object.getPrototypeOf(process.stdout).write.call(process.stdout, text);
write('a'.repeat(20));
await new Promise(resolve => setTimeout(resolve, 150));
write('b'.repeat(100));
await new Promise(resolve => setTimeout(resolve, 100));
return 1;
",
        ))
        .await
        .unwrap();
    assert_eq!(result.error.unwrap().kind, CodeRunFailureKind::OutputLimit);
    assert_eq!(result.value, None);
    assert_eq!(result.logs[0], "a".repeat(20));
    assert!(!result.logs[1].is_empty());
    assert!(
        "b".repeat(100)
            .starts_with(result.logs[1].as_str().unwrap())
    );
}

#[tokio::test]
async fn default_output_budget_accepts_exactly_64_mib_and_rejects_one_extra_byte() {
    let runtime = runtime(None);
    let exact = runtime
        .run(request("return 'x'.repeat(67_108_860);"))
        .await
        .unwrap();
    assert_eq!(exact.error, None, "{:?}", exact.error);
    assert_eq!(exact.logs, Vec::<String>::new());
    assert_eq!(
        exact.value.as_ref().map(|value| value.as_raw().len()),
        Some(67_108_862)
    );
    drop(exact);
    let oversized = runtime
        .run(request("return 'x'.repeat(67_108_861);"))
        .await
        .unwrap();
    assert_eq!(oversized.value, None);
    let failure = oversized.error.unwrap();
    assert_eq!(failure.kind, CodeRunFailureKind::OutputLimit);
    assert_eq!(failure.message, "outer output exceeded 67108864 bytes");
}

#[tokio::test]
async fn completed_worker_cannot_leave_a_nested_worker_running() {
    let fixture = std::env::temp_dir().join(format!(
        "seekdeep-worker-nested-stop-{}",
        std::process::id()
    ));
    let fixture_text = serde_json::to_string(&fixture).unwrap();
    let result = runtime(None).run(request(&format!(r"
const {{ Worker }} = await import('node:worker_threads');
const nested = new Worker(`const {{ parentPort, workerData }} = require('node:worker_threads'); require('node:fs').writeFileSync(workerData, 'ready'); parentPort.postMessage('ready'); setTimeout(() => require('node:fs').appendFileSync(workerData, ':orphan'), 200);`, {{ eval: true, workerData: {fixture_text} }});
await new Promise((resolve, reject) => {{ nested.once('message', resolve); nested.once('error', reject); }});
return 'finished';
"))).await.unwrap();
    assert_eq!(result.error, None, "{result:?}");
    assert_eq!(result.value, Some(json!("finished").into()));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(std::fs::read_to_string(&fixture).unwrap(), "ready");
    std::fs::remove_file(fixture).unwrap();
}

#[test]
#[cfg(unix)]
fn startup_cancellation_and_timeout_reap_the_unready_process_and_endpoint() {
    use std::{os::unix::fs::PermissionsExt as _, process::Command};

    let directory =
        std::env::temp_dir().join(format!("seekdeep-worker-startup-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = String::from_utf8(node.stdout).unwrap();
    let node = node.trim();
    let marker = directory.join("started.json");
    let fake = directory.join("unready-node.mjs");
    std::fs::write(&fake, format!("#!{node}\nimport {{ writeFileSync }} from 'node:fs';\nwriteFileSync({}, JSON.stringify({{pid:process.pid,endpoint:process.argv.at(-1)}}));\nsetInterval(() => {{}}, 1000);\n", serde_json::to_string(&marker).unwrap())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    for mode in ["abort", "timeout"] {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "unready_node_child", "--nocapture"])
            .env("SEEKDEEP_NODE_BINARY", &fake)
            .env("SEEKDEEP_UNREADY_NODE_MARKER", &marker)
            .env("SEEKDEEP_UNREADY_NODE_MODE", mode)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "startup {mode} check failed:\n{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        let started: Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        let alive = Command::new("kill")
            .args(["-0", &started["pid"].to_string()])
            .output()
            .unwrap();
        assert!(
            !alive.status.success(),
            "unready Node process must be reaped before return"
        );
        assert!(!std::path::Path::new(started["endpoint"].as_str().unwrap()).exists());
        std::fs::remove_file(&marker).unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
#[ignore = "isolated process with an explicitly unready Node executable"]
#[cfg(unix)]
async fn unready_node_child() {
    use std::time::Duration;

    let mode = std::env::var("SEEKDEEP_UNREADY_NODE_MODE").unwrap();
    let marker =
        std::path::PathBuf::from(std::env::var_os("SEEKDEEP_UNREADY_NODE_MARKER").unwrap());
    let runtime = WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
        max_wall_ms: Some(if mode == "timeout" { 1_000.0 } else { 30_000.0 }),
        ..Default::default()
    })
    .unwrap();
    let signal = seekdeep_llm::AbortSignal::default();
    let mut request = request("return 1;");
    request.signal = Some(signal.clone());
    let pending = tokio::spawn(async move { runtime.run(request).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    if mode == "abort" {
        signal.abort_with_reason(json!("startup cancelled"));
    }
    let result = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.logs, Vec::<String>::new());
    assert_eq!(result.value, None);
    let failure = result.error.unwrap();
    if mode == "abort" {
        assert_eq!(failure.kind, CodeRunFailureKind::Abort);
        assert_eq!(failure.message, "startup cancelled");
    } else {
        assert_eq!(failure.kind, CodeRunFailureKind::Timeout);
        assert_eq!(failure.message, "wall-clock ceiling reached (1000ms)");
    }
}
