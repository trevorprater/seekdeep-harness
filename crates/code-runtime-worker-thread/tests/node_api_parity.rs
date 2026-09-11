//! Real Node standard-library behavior through the public Rust worker backend.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use seekdeep_code_runtime::{
    CodeJsonString, CodeJsonValue, CodeRunFailure, CodeRunRequest, CodeRunResult,
    CodeRuntimeBackend,
};
use seekdeep_code_runtime_worker_thread::{
    WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, worker_json::decode_code_json,
};
use serde::Deserialize;
use serde_json::{Value, json};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

fn request(program: &str) -> CodeRunRequest {
    CodeRunRequest {
        program: program.into(),
        bindings: Vec::new(),
        signal: None,
    }
}

fn backend() -> WorkerThreadCodeRuntime {
    WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
        compute_ms: Some(10_000.0),
        max_wall_ms: Some(20_000.0),
        max_output_bytes: Some(1_000_000.0),
        ..Default::default()
    })
    .unwrap()
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
    assert_eq!(
        String::from_utf8(head.stdout).unwrap().trim(),
        commit,
        "source oracle must remain pinned"
    );
    Path::new(source).join("packages/code-runtime/code-runtime-worker-thread/src/worker.ts")
}

fn source(program: &str) -> CodeRunResult {
    #[derive(Deserialize)]
    struct SourceResult {
        value: Option<CodeJsonValue>,
        logs: Vec<CodeJsonString>,
        error: Option<CodeRunFailure>,
    }
    let mut child =
        Command::new(std::env::var_os("SEEKDEEP_NODE_BINARY").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/node_worker_oracle.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    serde_json::to_writer(
        child.stdin.as_mut().unwrap(),
        &json!({"program":program, "sourceWorker":source_worker()}),
    )
    .unwrap();
    child.stdin.take().unwrap().flush().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "source failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: SourceResult = serde_json::from_slice(&output.stdout).unwrap();
    CodeRunResult {
        value: result
            .value
            .as_ref()
            .map(|wire| decode_code_json(wire).expect("source lossless completion")),
        logs: result.logs,
        error: result.error,
    }
}

async fn differential(runtime: &WorkerThreadCodeRuntime, program: &str) -> CodeRunResult {
    let expected = source(program);
    let actual = runtime.run(request(program)).await.unwrap();
    if let (Some(Value::Object(actual)), Some(Value::Object(expected))) = (
        actual.value.as_ref().and_then(CodeJsonValue::as_serde_json),
        expected
            .value
            .as_ref()
            .and_then(CodeJsonValue::as_serde_json),
    ) {
        for (key, expected) in expected {
            assert_eq!(actual.get(key), Some(expected), "source mismatch at {key}");
        }
    }
    assert_eq!(actual, expected, "program: {program}");
    actual
}

#[tokio::test]
async fn exposes_real_node_namespaces_and_all_worker_thread_exports() {
    let runtime = backend();
    let result = differential(&runtime, r"
const names = ['assert', 'assert/strict', 'async_hooks', 'buffer', 'child_process', 'cluster', 'console', 'constants', 'crypto', 'dgram', 'diagnostics_channel', 'dns', 'dns/promises', 'events', 'fs', 'fs/promises', 'http', 'http2', 'https', 'inspector', 'inspector/promises', 'module', 'net', 'os', 'path', 'path/posix', 'path/win32', 'perf_hooks', 'process', 'querystring', 'readline', 'readline/promises', 'repl', 'stream', 'stream/consumers', 'stream/promises', 'stream/web', 'string_decoder', 'timers', 'timers/promises', 'tls', 'trace_events', 'tty', 'url', 'util', 'util/types', 'v8', 'vm', 'worker_threads', 'zlib'];
const modules = {};
for (const name of names) {
  try {
    const first = await import('node:' + name);
    const second = await import(name);
    modules[name] = { same: first === second, tag: Object.prototype.toString.call(first), exports: Object.keys(first).sort().map(key => [key, typeof first[key]]) };
  } catch (error) {
    modules[name] = { error: { name: error.name, code: error.code, message: error.message } };
  }
}
return modules;
").await;
    assert_eq!(result.value.unwrap().object_entries().unwrap().len(), 50);
}

#[tokio::test]
async fn buffer_crypto_compression_streams_urls_and_foreign_realms_match_source() {
    let runtime = backend();
    differential(&runtime, r"
const { createHash, webcrypto } = await import('node:crypto');
const { gzipSync, gunzipSync } = await import('node:zlib');
const { Readable, Transform } = await import('node:stream');
const { pipeline } = await import('node:stream/promises');
const { StringDecoder } = await import('node:string_decoder');
const { createContext, runInContext } = await import('node:vm');
const data = Buffer.from('héllo 😀', 'utf8');
const decoder = new StringDecoder('utf8');
const decoded = decoder.write(data.subarray(0, 2)) + decoder.end(data.subarray(2));
const chunks = [];
await pipeline(Readable.from(['a', 'b']), new Transform({ transform(chunk, encoding, done) { done(null, chunk.toString().toUpperCase()); } }), async source => { for await (const chunk of source) chunks.push(chunk.toString()); });
const context = createContext({});
const foreign = runInContext('({ list: [1, 2], plain: Object.create(null) })', context);
console.log(new Map([['x', 3]]), new Set([1, 2]), Buffer.from([1, 2, 3]));
return { hex: data.toString('hex'), bytes: Buffer.byteLength('héllo 😀'), decoded, hash: createHash('sha256').update(data).digest('hex'), compressed: gunzipSync(gzipSync(data)).equals(data), digest: Buffer.from(await webcrypto.subtle.digest('SHA-256', data)).toString('hex'), chunks, foreign, url: new URL('/x?q=a b', 'https://example.test').href, encoding: new TextDecoder().decode(new TextEncoder().encode('中文')) };
").await;
}

#[tokio::test]
async fn stackless_and_unrenderable_exceptions_match_source() {
    let runtime = backend();
    for program in [
        "const error = new Error('bare'); error.stack = undefined; throw error;",
        "const error = new Error('bare'); error.stack = null; error.message = 42; throw error;",
        "const error = new Error('ignored'); error.stack = 42; throw error;",
        "const error = new Error('ignored'); Object.defineProperty(error, 'stack', {get() {throw 'broken stack';}}); throw error;",
        "throw {toString() {throw 'broken string';}};",
        "throw 'raw-throw';",
    ] {
        differential(&runtime, program).await;
    }
}

#[tokio::test]
async fn worker_channels_transfer_lists_nested_workers_and_environment_data_match_source() {
    let runtime = backend();
    differential(&runtime, r"
const { Worker, MessageChannel, receiveMessageOnPort, isMainThread, threadId, setEnvironmentData, getEnvironmentData, markAsUntransferable, isMarkedAsUntransferable, resourceLimits } = await import('node:worker_threads');
setEnvironmentData('fixture', { value: 7 });
const buffer = new ArrayBuffer(4); new Uint8Array(buffer).set([2, 3, 5, 7]);
const channel = new MessageChannel();
channel.port1.postMessage({ bytes: buffer }, [buffer]);
const received = receiveMessageOnPort(channel.port2);
const preserved = new ArrayBuffer(1); markAsUntransferable(preserved);
const nested = new Worker(`const { parentPort, workerData, getEnvironmentData, isMainThread } = require('node:worker_threads'); parentPort.postMessage({ value: workerData + 1, environment: getEnvironmentData('fixture'), main: isMainThread, env: Object.keys(process.env) });`, { eval: true, workerData: 41 });
const answer = await new Promise((resolve, reject) => { nested.once('message', resolve); nested.once('error', reject); });
await nested.terminate(); channel.port1.close(); channel.port2.close();
return { main: isMainThread, positiveId: threadId > 0, heap: resourceLimits.maxOldGenerationSizeMb, detached: buffer.byteLength, received: [...new Uint8Array(received.message.bytes)], marked: isMarkedAsUntransferable(preserved), answer, environment: getEnvironmentData('fixture') };
").await;
}

#[tokio::test]
async fn actual_files_package_resolution_commonjs_json_and_dynamic_import_errors_match_source() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("node_modules/fixture-package")).unwrap();
    std::fs::write(fixture.0.join("node_modules/fixture-package/package.json"), "{\"name\":\"fixture-package\",\"type\":\"module\",\"exports\":{\"import\":\"./entry.mjs\",\"require\":\"./entry.cjs\"}}\n").unwrap();
    std::fs::write(
        fixture.0.join("node_modules/fixture-package/entry.mjs"),
        "export const value = 'esm'; export default 17;\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join("node_modules/fixture-package/entry.cjs"),
        "module.exports = { value: 'cjs', count: 23 };\n",
    )
    .unwrap();
    std::fs::write(fixture.0.join("data.json"), "{\"nested\":[true,null,9]}\n").unwrap();
    std::fs::write(fixture.0.join("entry.mjs"), "import * as esm from 'fixture-package'; import { createRequire } from 'node:module'; const require = createRequire(import.meta.url); export const result = { esm: { ...esm }, cjs: require('fixture-package'), json: require('./data.json') };\n").unwrap();
    let path = serde_json::to_string(&fixture.0).unwrap();
    let program = format!(
        r"
const root = {path};
const fs = await import('node:fs/promises');
const {{ pathToFileURL }} = await import('node:url');
const url = pathToFileURL(root + '/entry.mjs').href;
const first = await import(url), second = await import(url);
await fs.writeFile(root + '/output.txt', Buffer.from('round trip'));
const stat = await fs.stat(root + '/output.txt');
const text = await fs.readFile(root + '/output.txt', 'utf8');
const json = await import(pathToFileURL(root + '/data.json').href, {{ with: {{ type: 'json' }} }});
let missing; try {{ await import('node:not_a_real_module'); }} catch (error) {{ missing = {{ name:error.name, code:error.code, message:error.message }}; }}
return {{ result:first.result, same:first === second, stat:stat.isFile(), text, json:json.default, missing }};
"
    );
    differential(&backend(), &program).await;
}

#[tokio::test]
async fn network_and_subprocess_apis_execute_real_operations() {
    differential(&backend(), r"
const http = await import('node:http');
const { once } = await import('node:events');
const { execFile } = await import('node:child_process');
const { promisify } = await import('node:util');
const server = http.createServer((request, response) => { response.setHeader('content-type', 'application/json'); response.end(JSON.stringify({ path:request.url, method:request.method })); });
server.listen(0, '127.0.0.1'); await once(server, 'listening');
let response;
try { response = await (await fetch('http://127.0.0.1:' + server.address().port + '/actual?x=1')).json(); }
finally { await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())); }
const child = await promisify(execFile)(process.execPath, ['-e', 'process.stdout.write(JSON.stringify({empty:Object.keys(process.env).length===0,result:6*7}))'], { env: {} });
return { response, child: JSON.parse(child.stdout), stderr: child.stderr };
").await;
}

#[tokio::test]
async fn concurrent_runs_share_real_node_broadcast_channels() {
    let runtime = backend();
    let channel = format!(
        "seekdeep-parity-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let receiver = format!(
        "const {{ BroadcastChannel }} = await import('node:worker_threads'); const channel = new BroadcastChannel({}); const result = await new Promise(resolve => {{ channel.onmessage = event => resolve(event.data); }}); channel.close(); return result;",
        serde_json::to_string(&channel).unwrap()
    );
    let sender = format!(
        "const {{ BroadcastChannel }} = await import('node:worker_threads'); const channel = new BroadcastChannel({}); await new Promise(resolve => setTimeout(resolve, 100)); channel.postMessage({{shared:true}}); channel.close(); return 'sent';",
        serde_json::to_string(&channel).unwrap()
    );
    let (delivery, sent) = tokio::join!(
        runtime.run(request(&receiver)),
        runtime.run(request(&sender))
    );
    assert_eq!(delivery.unwrap().value, Some(json!({"shared":true}).into()));
    assert_eq!(sent.unwrap().value, Some(json!("sent").into()));
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "seekdeep-node-api-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot create fixture: {error}"),
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
