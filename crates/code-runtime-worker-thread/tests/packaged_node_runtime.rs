//! Release-layout execution without relying on the source checkout's runtime assets.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_code_runtime::{CODE_RUNTIME, CodeRunRequest};
use seekdeep_code_runtime_worker_thread::{
    WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig, node_runtime_assets, plugin,
};
use seekdeep_cordis::Context;
use serde_json::json;

#[test]
fn packaged_artifacts_load_and_run_after_relocation() {
    let executable = std::env::current_exe().unwrap();
    let target = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let wasm = std::env::var_os("SEEKDEEP_NODE_WASM").map_or_else(
        || target.join("wasm32-unknown-unknown/release/seekdeep_code_runtime_node.wasm"),
        PathBuf::from,
    );
    assert!(
        wasm.is_file(),
        "build seekdeep-code-runtime-node for wasm32-unknown-unknown --release before package verification: {}",
        wasm.display()
    );
    let directory =
        std::env::temp_dir().join(format!("seekdeep-packaged-node-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let fixture = Fixture(directory);
    let runtime_dir = fixture.0.join("lib/seekdeep/code-runtime-node");
    let package = Command::new(env!("CARGO_BIN_EXE_package-code-runtime-node"))
        .arg(&wasm)
        .arg(&runtime_dir)
        .output()
        .unwrap();
    assert!(
        package.status.success(),
        "package failed: {}",
        String::from_utf8_lossy(&package.stderr)
    );
    assert!(runtime_dir.join("loader.mjs").is_file());
    assert!(
        runtime_dir
            .join("seekdeep_code_runtime_node_bg.wasm")
            .is_file()
    );
    let bin = fixture.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let probe = bin.join("seekdeep-runtime-probe");
    std::fs::copy(&executable, &probe).unwrap();
    let mut command = Command::new(probe);
    command
        .args([
            "--ignored",
            "--exact",
            "packaged_runtime_child",
            "--nocapture",
        ])
        .env_remove("SEEKDEEP_CODE_RUNTIME_NODE_DIR")
        .env("SEEKDEEP_PACKAGED_NODE_ASSETS", &runtime_dir)
        .env("SEEKDEEP_PACKAGED_NODE_CHILD", "1")
        .current_dir(&fixture.0);
    let child = command.output().unwrap();
    assert!(
        child.status.success(),
        "relocated runtime failed:\n{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
    std::fs::rename(&runtime_dir, fixture.0.join("uninstalled-node")).unwrap();
    let missing = command
        .env("SEEKDEEP_PACKAGED_NODE_MISSING", "1")
        .output()
        .unwrap();
    assert!(
        missing.status.success(),
        "missing-asset check failed:\n{}\n{}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(String::from_utf8_lossy(&missing.stdout).contains("1 passed"));
}

#[tokio::test]
#[ignore = "executed by the parent against relocated packaged assets"]
async fn packaged_runtime_child() {
    assert_eq!(
        std::env::var("SEEKDEEP_PACKAGED_NODE_CHILD").as_deref(),
        Ok("1")
    );
    assert!(std::env::var_os("SEEKDEEP_CODE_RUNTIME_NODE_DIR").is_none());
    if std::env::var_os("SEEKDEEP_PACKAGED_NODE_MISSING").is_some() {
        let error =
            WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Node code-runtime assets are not installed next to this executable"),
            "{error:#}"
        );
        return;
    }
    let directory = std::env::var_os("SEEKDEEP_PACKAGED_NODE_ASSETS").unwrap();
    assert!(Path::new(&directory).is_absolute());
    assert_eq!(
        node_runtime_assets().unwrap(),
        Path::new(&directory).canonicalize().unwrap()
    );
    let context = Context::new();
    let fiber = context.plugin(plugin(), json!({})).unwrap();
    fiber.await_settled().await.unwrap();
    let runtime = context.get(CODE_RUNTIME).unwrap();
    assert_eq!(runtime.language(), "typescript");
    assert_eq!(runtime.isolation(), "worker-thread");
    let result = runtime.run(CodeRunRequest {
        program: "const { createHash } = await import('node:crypto'); const { MessageChannel, receiveMessageOnPort, isMainThread } = await import('node:worker_threads'); const {port1, port2} = new MessageChannel(); port1.postMessage(Buffer.from('packaged')); const message = receiveMessageOnPort(port2).message; port1.close(); port2.close(); return { main:isMainThread, text:Buffer.from(message).toString(), hash:createHash('sha256').update('packaged').digest('hex') };".into(),
        bindings: Vec::new(), signal: None,
    }).await.unwrap();
    assert_eq!(result.error, None, "{result:?}");
    assert_eq!(
        result.value.as_ref().unwrap().get("text").unwrap().as_raw(),
        "\"packaged\""
    );
    assert_eq!(
        result.value.as_ref().unwrap().get("main").unwrap().as_raw(),
        "false"
    );
    assert_eq!(
        result
            .value
            .as_ref()
            .unwrap()
            .get("hash")
            .unwrap()
            .to_utf16()
            .unwrap()
            .len(),
        64
    );
    fiber.dispose().await.unwrap();
    assert!(context.get(CODE_RUNTIME).is_none());
    assert!(
        runtime
            .run(CodeRunRequest {
                program: "return 1".into(),
                bindings: Vec::new(),
                signal: None
            })
            .await
            .is_err()
    );
}

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
