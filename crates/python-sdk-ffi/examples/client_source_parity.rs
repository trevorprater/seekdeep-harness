//! Run the pinned Python SDK tests against the generated Rust-backed facade.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [source, library] = arguments.as_slice() else {
        return Err("usage: client_source_parity <pinned-source> <native-library>".into());
    };
    let source = Path::new(source).canonicalize()?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("source pin absent")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let library = Path::new(library).canonicalize()?;
    let name = library
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("library basename absent")?;
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("deepseek_harness_runtime");
    let sdk = temporary.path().join("deepseek_harness");
    std::fs::create_dir_all(runtime.join("runtime"))?;
    std::fs::create_dir(&sdk)?;
    std::fs::copy(&library, runtime.join("runtime").join(name))?;
    for (name, text) in seekdeep_python_sdk::bindings::runtime_bindings(name)? {
        std::fs::write(runtime.join(name), text)?;
    }
    for (name, text) in seekdeep_python_sdk::bindings::sdk_bindings()? {
        std::fs::write(sdk.join(name), text)?;
    }
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data = repository.join("python/sdk-runtime/src/deepseek_harness_runtime");
    std::fs::copy(
        data.join("seekdeep-harness-runtime.json"),
        runtime.join("seekdeep-harness-runtime.json"),
    )?;
    std::fs::copy(
        data.join("runtime/cordis.yml"),
        runtime.join("runtime/cordis.yml"),
    )?;
    for filename in ["test_client.py", "test_runtime_resolution.py"] {
        let tests = std::fs::read_to_string(source.join("python/sdk/tests").join(filename))?
            .replace("@deepseek-ai/", "@seekdeep-ai/")
            .replace("dsh-", "seekdeep-")
            .replace("DSH_", "SEEKDEEP_")
            .replace("DeepSeek Harness", "SeekDeep Harness")
            .replace("deepseek-harness", "seekdeep-harness");
        std::fs::write(temporary.path().join(filename), tests)?;
    }
    std::fs::write(temporary.path().join("test_binding_identity.py"), EXTRA)?;
    let python = std::env::var_os("SEEKDEEP_PYTHON_TEST_PYTHON")
        .map_or_else(|| source.join("python/sdk/.venv/bin/python"), PathBuf::from);
    let expected = declaration_probe(&python, &source.join("python/sdk/src"))?;
    let actual = declaration_probe(&python, temporary.path())?;
    if actual != expected {
        return Err(
            format!("Python declaration mismatch: source={expected}, target={actual}").into(),
        );
    }
    println!("public exports, model declarations, and exception contracts match the pinned source");
    let status = Command::new(&python)
        .args([
            "-m",
            "pytest",
            "-p",
            "no:cacheprovider",
            "-o",
            "addopts=",
            "-q",
            "--maxfail=1",
        ])
        .arg(temporary.path())
        .env("PYTHONPATH", temporary.path())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(temporary.path())
        .status()?;
    if !status.success() {
        return Err("pinned Python client suite failed against native bindings".into());
    }
    Ok(())
}

fn declaration_probe(
    python: &Path,
    package: &Path,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let output = Command::new(python)
        .args(["-c", DECLARATIONS])
        .env("PYTHONPATH", package)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(package)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

const DECLARATIONS: &str = r#"
import dataclasses, inspect, json
import deepseek_harness as sdk
from deepseek_harness import models, errors

payload = {"value":1}
notification = models.Notification("tick", payload)
request = models.IncomingRequest(True, "request", payload)
try:
    notification.extra = 1
except AttributeError:
    slots = True
else:
    slots = False
failure = errors.JsonRpcError(True, "failed", payload)
result = {
    "exports":sdk.__all__,
    "models":{
        name:{"signature":str(inspect.signature(getattr(models,name))),
              "annotations":getattr(models,name).__annotations__}
        for name in ["Notification","IncomingRequest","ServerInfo","InitializeResponse"]
    },
    "notification":{
        "fields":[field.name for field in dataclasses.fields(notification)],
        "shared":notification.payload is payload,
        "slots":slots,
        "equal":notification == models.Notification("tick",{"value":1}),
        "repr":repr(notification),
    },
    "request":{"fields":[field.name for field in dataclasses.fields(request)],
               "shared":request.payload is payload,"repr":repr(request)},
    "initialize":models.InitializeResponse.model_validate({"serverInfo":{"name":"fixture","version":"1","ignored":True},"ignored":True}).model_dump(),
    "empty_initialize":models.InitializeResponse().model_dump(),
    "errors":{name:[base.__name__ for base in getattr(errors,name).__mro__]
              for name in ["HarnessError","TransportClosedError","SdkProtocolError","JsonRpcError"]},
    "jsonrpc":{"signature":str(inspect.signature(errors.JsonRpcError)),
               "args":failure.args,"code":failure.code,"message":failure.message,
               "shared":failure.data is payload,"repr":repr(failure)},
}
print(json.dumps(result,sort_keys=True,default=str))
"#;

const EXTRA: &str = r#"
import gc, json, sys
from deepseek_harness import DeepSeekHarness, HarnessClient, HarnessConfig

def test_original_payload_and_notification_identity():
    client = HarnessClient()
    message = {"method":"tick","params":{"value":1}}
    with client.subscribe_notifications() as a, client.subscribe_notifications() as b:
        client._handle_message(message)
        left, right = a.next(), b.next()
        assert left is right
        assert left.payload is message["params"]
        left.payload["value"] = 2
        assert right.payload["value"] == 2

def test_callback_exception_references_do_not_accumulate():
    client = HarnessClient()
    before = len(client._context.objects)
    def bad(_):
        raise RuntimeError("expected")
    for _ in range(30):
        with client.subscribe_notifications(bad) as subscription:
            client._handle_message({"method":"tick"})
            try:
                subscription.next()
            except RuntimeError as error:
                assert str(error) == "expected"
            else:
                raise AssertionError("filter error lost")
            client.next_notification()
    gc.collect()
    assert len(client._context.objects) == before

def test_late_event_mutation_and_replacement_preserve_captured_identity(tmp_path):
    peer = tmp_path / "peer.py"
    peer.write_text('''
import json, sys
for line in sys.stdin:
    message=json.loads(line)
    method=message.get("method")
    if method=="initialize":
        print(json.dumps({"id":message["id"],"result":{}}),flush=True)
    elif method=="session/prompt":
        print(json.dumps({"method":"session.event","params":{"sessionId":"main","event":{"type":"agent/inbox/spliced","data":{"inserted":[{"id":"m"}]}}}}),flush=True)
        print(json.dumps({"id":message["id"],"result":{"messageId":"m"}}),flush=True)
        print(json.dumps({"method":"session.event","params":{"sessionId":"main","event":{"type":"assistant/message","data":{"content":[{"type":"text","text":"before"}]}}}}),flush=True)
        print(json.dumps({"method":"session.status","params":{"sessionId":"main","status":"idle"}}),flush=True)
    elif method=="shutdown":
        print(json.dumps({"id":message["id"],"result":{}}),flush=True)
        break
''')
    launch = (sys.executable,str(peer))
    saved = {}
    with DeepSeekHarness(launch_args_override=launch) as harness:
        assert harness.config.launch_args_override is launch
        assert harness.client.config.launch_args_override is launch
        def observe(notification):
            if notification.method=="session.event" and notification.payload["event"]["type"]=="assistant/message":
                saved["notification"]=notification
                saved["event"]=notification.payload["event"]
            if notification.method=="session.status":
                saved["event"]["data"]["content"][0]["text"]="after"
                saved["notification"].payload["event"]={"type":"replacement"}
                harness.config.session_root="changed-at-idle"
        result=harness.run("test",session_id="main",on_notification=observe)
    assert result.final_response=="after"
    assert result.events[-1] is saved["event"]
    assert result.notifications[-2] is saved["notification"]
    assert result.notifications[-2].payload["event"]["type"]=="replacement"
    assert result.session_root=="changed-at-idle"

def test_harness_preserves_client_callback_exception_owner(monkeypatch):
    import deepseek_harness_runtime as runtime
    marker = RuntimeError("client-owned callback failure")
    def fail():
        raise marker
    monkeypatch.setattr(runtime, "resolve_bundled_launch_args", fail)
    harness = DeepSeekHarness()
    try:
        harness.start()
    except BaseException as error:
        assert error is marker
    else:
        raise AssertionError("client callback failure was lost")
    finally:
        harness.close()

def test_harness_preserves_client_import_error_cause(monkeypatch):
    import deepseek_harness_runtime as runtime
    marker = ImportError("client-owned runtime import failure")
    def fail():
        raise marker
    monkeypatch.setattr(runtime, "resolve_bundled_launch_args", fail)
    harness = DeepSeekHarness()
    try:
        harness.start()
    except FileNotFoundError as error:
        assert error.__cause__ is marker
    else:
        raise AssertionError("client import failure was lost")
    finally:
        harness.close()
"#;
