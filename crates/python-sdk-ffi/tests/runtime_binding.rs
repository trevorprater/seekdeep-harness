//! Real ctypes imports exercise owned buffers, nested callbacks, and runtime lookup.

use std::process::Command;

const PROBE: &str = r#"
import os, pathlib, threading
import deepseek_harness_runtime as runtime
from deepseek_harness_runtime import _bridge

root = runtime.bundled_package_dir()
assert runtime.bundled_default_config_path() == root / "runtime" / "cordis.yml"
assert _bridge.invoke({"op":"about"})["abiVersion"] == 2
class Notification:
    def __init__(self, method, payload):
        self.method, self.payload = method, payload
created = []
def construct(method, payload):
    value = Notification(method, payload)
    created.append(value)
    return value
value = _bridge.invoke({"op":"notification.create","value":{"method":"tick","payload":{"value":1}}},
    callbacks={"notification.create":construct})
assert value is created[0]
assert value.payload == {"value":1}
for mode in ["bogus", "", 7, False, [], {}, float("nan"), "\ud800"]:
    try:
        runtime.resolve_bundled_launch_args(mode)
    except ValueError as error:
        assert "expected 'exe' or 'node'" in str(error), str(error)
    else:
        raise AssertionError("unknown mode accepted")

original_tag = runtime._current_platform_tag
runtime._current_platform_tag = lambda: "macos-arm64"
exe = root / "runtime" / "seekdeep-jsonrpc-agent-pkg-macos-arm64"
exe.touch()
try:
    runtime.bundled_runtime_path()
except FileNotFoundError as error:
    assert "node-pty spawn helper" in str(error)
else:
    raise AssertionError("missing helper accepted")
pathlib.Path(str(exe) + "-spawn-helper").touch()
assert runtime.bundled_runtime_path() == exe
os.environ[runtime.RUNTIME_MODE_ENV_VAR] = "invalid-env"
assert runtime.resolve_bundled_launch_args("exe") == (str(exe),)

class Exe:
    def __eq__(self, other):
        return other == "exe"
    def __repr__(self):
        raise AssertionError("accepted mode must not be represented")
assert runtime.resolve_bundled_launch_args(Exe()) == (str(exe),)
marker = RuntimeError("retained callback exception")
class Broken:
    def __eq__(self, other):
        return False
    def __repr__(self):
        raise marker
try:
    runtime.resolve_bundled_launch_args(Broken())
except RuntimeError as error:
    assert error is marker
else:
    raise AssertionError("callback exception lost")

results = []
def parallel():
    results.append(runtime.resolve_bundled_launch_args("exe"))
threads = [threading.Thread(target=parallel) for _ in range(20)]
for thread in threads: thread.start()
for thread in threads: thread.join()
assert results == [(str(exe),)] * 20
assert _bridge._active == {}
runtime._current_platform_tag = original_tag
print("runtime binding operations passed")
"#;

#[test]
fn generated_runtime_bindings_use_the_real_native_library() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("deepseek_harness_runtime");
    std::fs::create_dir_all(package.join("runtime")).unwrap();
    let name = if cfg!(target_os = "macos") {
        "libseekdeep_python_sdk_ffi.dylib"
    } else if cfg!(windows) {
        "seekdeep_python_sdk_ffi.dll"
    } else {
        "libseekdeep_python_sdk_ffi.so"
    };
    let library = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(name);
    assert!(
        library.is_file(),
        "normal Cargo integration-test library missing: {}",
        library.display()
    );
    std::fs::copy(library, package.join("runtime").join(name)).unwrap();
    for (name, text) in seekdeep_python_sdk::bindings::runtime_bindings(name).unwrap() {
        std::fs::write(package.join(name), text).unwrap();
    }
    std::fs::write(
        package.join(seekdeep_python_sdk::runtime::PACKAGE_METADATA_FILENAME),
        "{}",
    )
    .unwrap();
    std::fs::write(package.join("runtime/cordis.yml"), "[]").unwrap();
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["-c", PROBE])
        .env("PYTHONPATH", root.path())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "runtime binding operations passed"
    );
}
