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
    let expected = python_probe(&python, &source.join("python/sdk/src"), EDGE_PROBE)?;
    let actual = python_probe(&python, temporary.path(), EDGE_PROBE)?;
    if actual != expected {
        return Err(format!("Python API edge mismatch: source={expected}, target={actual}").into());
    }
    println!("public API value and path edges match the pinned source");
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
    python_probe(python, package, DECLARATIONS)
}

fn python_probe(
    python: &Path,
    package: &Path,
    probe: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let output = Command::new(python)
        .args(["-c", probe])
        .env("PYTHONPATH", package)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(package)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

const EDGE_PROBE: &str = r#"
import json, os, sys, tempfile
from pathlib import Path
from deepseek_harness import DeepSeekHarness, DeepSeekHarnessConfig, HarnessClient, HarnessConfig, Notification
from deepseek_harness.api import _is_inbox_receipt, final_response, finish_reason, normalize_input
from deepseek_harness.client import _int_or_none
import deepseek_harness_runtime as runtime

class Text(str):
    pass

class Number(int):
    pass

class Marker:
    pass

class FalseHarnessConfig(DeepSeekHarnessConfig):
    truth_calls = 0
    def __bool__(self):
        self.truth_calls += 1
        return False

class RenderedText:
    def __init__(self, rendered, truth=True):
        self.rendered, self.truth = rendered, truth
        self.truth_calls = self.string_calls = 0
    def __bool__(self):
        self.truth_calls += 1
        return self.truth
    def __str__(self):
        self.string_calls += 1
        return self.rendered

class BrokenText(RenderedText):
    def __init__(self, marker):
        super().__init__("unused")
        self.marker = marker
    def __str__(self):
        self.string_calls += 1
        raise self.marker

class ContraryType(str):
    def __eq__(self, other):
        return False
    def __ne__(self, other):
        return False

def failure(call):
    try:
        call()
    except BaseException as error:
        return [type(error).__name__, str(error)]
    return None

def outcome(call):
    try:
        return ["ok", call()]
    except BaseException as error:
        return ["error", type(error).__name__, str(error)]

def runtime_resolution_import_error():
    marker = ImportError("resolver failed after import")
    original = runtime.resolve_bundled_launch_args
    runtime.resolve_bundled_launch_args = lambda: (_ for _ in ()).throw(marker)
    client = HarnessClient()
    try:
        client.start()
    except BaseException as error:
        return [type(error).__name__, error is marker,
                None if error.__cause__ is None else type(error.__cause__).__name__,
                error.__cause__ is marker]
    finally:
        runtime.resolve_bundled_launch_args = original
        client.close()
    return None

def wide_wire_values():
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        peer = root / "peer.py"
        capture = root / "capture.jsonl"
        peer.write_text('''
import json, os, sys
for line in sys.stdin:
    message=json.loads(line)
    with open(os.environ["CAPTURE"],"a") as output:
        output.write(json.dumps(message,separators=(",",":"))+"\\n")
    if message.get("method")=="shutdown":
        print(json.dumps({"id":message["id"],"result":{}}),flush=True)
        break
''')
        client = HarnessClient(HarnessConfig(
            launch_args_override=(sys.executable,str(peer)),
            env={"CAPTURE":str(capture)},
        ))
        wide = 10**100
        try:
            client.start()
            client.respond(wide,{"wide":wide})
            client.respond_error(-wide,code=wide,message="wide",data={"wide":-wide})
            client.notify("wide",{"wide":wide})
            failure = None
        except BaseException as error:
            failure = [type(error).__name__,str(error)]
        finally:
            client.close()
        messages = [json.loads(line) for line in capture.read_text().splitlines()]
        return [failure,
                any(message.get("id")==wide and message.get("result",{}).get("wide")==wide for message in messages),
                any(message.get("id")==-wide and message.get("error",{}).get("code")==wide
                    and message.get("error",{}).get("data",{}).get("wide")==-wide for message in messages),
                any(message.get("method")=="wide" and message.get("params",{}).get("wide")==wide for message in messages)]

def wide_high_level_config():
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        peer = root / "peer.py"
        capture = root / "capture.jsonl"
        peer.write_text('''
import json, os, sys
for line in sys.stdin:
    message=json.loads(line)
    with open(os.environ["CAPTURE"],"a") as output:
        output.write(json.dumps(message,separators=(",",":"))+"\\n")
    if message.get("method") in ("initialize","shutdown"):
        print(json.dumps({"id":message["id"],"result":{}}),flush=True)
    if message.get("method")=="shutdown": break
''')
        wide = 10**100
        harness = DeepSeekHarness(
            max_tokens=wide,
            launch_args_override=(sys.executable,str(peer)),
            env={"CAPTURE":str(capture)},
        )
        harness.start()
        harness.close()
        messages = [json.loads(line) for line in capture.read_text().splitlines()]
        value = next(message["params"]["maxTokens"] for message in messages
                     if message.get("method")=="initialize")
        return [type(value).__name__,str(value),value==wide]

def wide_inbound_values():
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        peer = root / "peer.py"
        peer.write_text('''
import json, sys
wide=10**100
for line in sys.stdin:
    message=json.loads(line)
    method=message.get("method")
    if method=="wide-result":
        print(json.dumps({"id":message["id"],"result":{"wide":wide}}),flush=True)
    elif method=="wide-error":
        print(json.dumps({"id":message["id"],"error":{"code":wide,"message":"wide","data":{"wide":-wide}}}),flush=True)
    elif method=="wide-request":
        print(json.dumps({"id":wide,"method":"peer.call","params":{"wide":wide}}),flush=True)
    elif method=="shutdown":
        print(json.dumps({"id":message["id"],"result":{}}),flush=True)
        break
''')
        wide = 10**100
        client = HarnessClient(HarnessConfig(
            launch_args_override=(sys.executable,str(peer)),
            request_timeout_seconds=2,
        ))
        client.start()
        result = client._request_raw("wide-result")
        try:
            client._request_raw("wide-error")
        except BaseException as error:
            error_result = [type(error).__name__,error.code==wide,error.data=={"wide":-wide}]
        else:
            error_result = None
        client.notify("wide-request")
        request = client.next_request()
        client.close()
        return [result=={"wide":wide},error_result,
                type(request.id).__name__,request.id==wide,request.payload=={"wide":wide}]

text = Text("subclass")
normalized_text = normalize_input(text)
values = [[], {}, (), Marker()]
normalized_values = [normalize_input(value) is value for value in values]
number = Number(7)
false_config = FalseHarnessConfig(model="not-selected")
false_config_harness = DeepSeekHarness(false_config)
rendered_text = RenderedText("rendered")
false_text = RenderedText("must-not-render", False)
surrogate_text = RenderedText("\ud800")
custom_projection = final_response([{"type":"assistant/message","data":{"content":[
    {"type":"text","text":rendered_text},
    {"type":"text","text":false_text},
    {"type":"text","text":surrogate_text},
]}}])
render_marker = RuntimeError("render failed")
broken_text = BrokenText(render_marker)
try:
    final_response([{"type":"assistant/message","data":{"content":[{"type":"text","text":broken_text}]}}])
except BaseException as render_error:
    broken_projection = [render_error is render_marker, type(render_error).__name__,
                         broken_text.truth_calls, broken_text.string_calls]
else:
    broken_projection = None
kind = Text("subclass-kind")
projected_kind = finish_reason([{"type":"turn/end","data":{"reason":{"kind":kind}}}])
contrary_response = final_response([{"type":ContraryType("other"),"data":{"content":[{"type":"text","text":"contrary"}]}}])
contrary_reason = finish_reason([{"type":ContraryType("other"),"data":{"reason":{"kind":"contrary"}}}])
notification = Notification("session.event", {
    "sessionId":"root",
    "event":{"type":"agent/inbox/spliced","data":{"inserted":[{"id":"message"}]}},
})
events = [
    {"type":"assistant/message","data":{"content":[
        {"type":"text","text":None}, {"type":"text","text":False},
        {"type":"text","text":0}, {"type":"text","text":""},
        {"type":"text","text":7}, {"type":"text","text":True},
        {"type":"text","text":3.5}, {"type":"image","text":"ignored"},
    ]}},
    {"type":"assistant/message","data":{"message":{"content":[{"type":"text","text":"last"}]}}},
]

with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    nested = workspace / "nested"
    nested.mkdir(parents=True)
    (root / "alias").symlink_to(workspace, target_is_directory=True)
    previous = Path.cwd()
    os.chdir(root)
    try:
        config = DeepSeekHarnessConfig(
            cwd="alias/nested/..",
            runtime_cwd="",
            session_root="sessions",
            cordis="config.yml",
            env={"EDGE":"yes"},
            launch_args_override=("python", "peer.py"),
            max_tokens=True,
        )
        harness = DeepSeekHarness(config)
        client_env = harness.client.config.env
        path_result = {
            "same_config":harness.config is config,
            "same_client":harness.client is harness.client,
            "client_cwd_is_root":Path(harness.client.config.cwd).resolve() == root.resolve(),
            "client_launch":harness.client.config.launch_args_override,
            "workspace_is_resolved_alias":Path(client_env.get("DSH_CWD",client_env.get("SEEKDEEP_CWD"))).resolve() == workspace.resolve(),
            "session_root":client_env.get("DSH_SESSION_ROOT",client_env.get("SEEKDEEP_SESSION_ROOT")),
            "cordis":client_env.get("DSH_CORDIS_CONFIG",client_env.get("SEEKDEEP_CORDIS_CONFIG")),
            "custom":client_env["EDGE"],
        }
    finally:
        os.chdir(previous)

result = {
    "normalize_text_type":type(normalized_text[0]["text"]).__name__,
    "normalize_text_identity":normalized_text[0]["text"] is text,
    "normalize_passthrough":normalized_values,
    "final_response":final_response(events),
    "finish_reason":finish_reason([
        {"type":"turn/end","data":{"reason":{"kind":"first"}}},
        {"type":"turn/end","data":{"reason":{"kind":"future-kind"}}},
    ]),
    "finish_missing":finish_reason([]),
    "finish_subclass_identity":projected_kind is kind,
    "contrary_comparisons":[contrary_response,contrary_reason],
    "finish_failure":failure(lambda:finish_reason([{"type":"turn/end","data":{"reason":{}}}])),
    "receipt":_is_inbox_receipt(notification,"root","message"),
    "receipt_wrong":_is_inbox_receipt(notification,"other","message"),
    "int_subclass_identity":_int_or_none(number) is number,
    "int_bool":_int_or_none(True),
    "int_float":_int_or_none(7.0),
    "constructor_conflict":failure(lambda:DeepSeekHarness(DeepSeekHarnessConfig(),model="other")),
    "constructor_unknown":failure(lambda:DeepSeekHarness(unknown=True)),
    "falsey_high_config":[false_config_harness.config is false_config,
                           type(false_config_harness.config).__name__,
                           false_config_harness.config.model,
                           false_config.truth_calls],
    "nonfinite_text":[
        outcome(lambda value=value:final_response([{"type":"assistant/message","data":{"content":[{"type":"text","text":value}]}}]))
        for value in [float("nan"),float("inf"),float("-inf")]
    ],
    "wide_integer_text":[
        outcome(lambda value=value:final_response([{"type":"assistant/message","data":{"content":[{"type":"text","text":value}]}}]))
        for value in [10**100,-(10**100)]
    ],
    "custom_projection":[custom_projection.encode("unicode_escape").decode("ascii"),
                         rendered_text.truth_calls,rendered_text.string_calls,
                         false_text.truth_calls,false_text.string_calls,
                         surrogate_text.truth_calls,surrogate_text.string_calls],
    "broken_projection":broken_projection,
    "client_falsey_config":HarnessClient(HarnessConfig()).config == HarnessConfig(),
    "resolver_import_error":runtime_resolution_import_error(),
    "wide_wire_values":wide_wire_values(),
    "wide_high_level_config":wide_high_level_config(),
    "wide_inbound_values":wide_inbound_values(),
    "paths":path_result,
}
print(json.dumps(result,sort_keys=True,default=str))
"#;

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

def test_harness_preserves_post_import_resolver_failure(monkeypatch):
    import deepseek_harness_runtime as runtime
    marker = ImportError("client-owned runtime import failure")
    def fail():
        raise marker
    monkeypatch.setattr(runtime, "resolve_bundled_launch_args", fail)
    harness = DeepSeekHarness()
    try:
        harness.start()
    except ImportError as error:
        assert error is marker
    else:
        raise AssertionError("client import failure was lost")
    finally:
        harness.close()
"#;
