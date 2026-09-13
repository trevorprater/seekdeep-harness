//! Run the pinned runtime-resolution test file against generated Rust-backed Python bindings.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [source, library] = arguments.as_slice() else {
        return Err("usage: runtime_source_parity <pinned-source> <native-library>".into());
    };
    let source = Path::new(source).canonicalize()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("source pin absent")?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let library = Path::new(library).canonicalize()?;
    let name = library
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("library basename absent")?;
    let temporary = tempfile::tempdir()?;
    let package = temporary.path().join("deepseek_harness_runtime");
    std::fs::create_dir_all(package.join("runtime"))?;
    std::fs::copy(&library, package.join("runtime").join(name))?;
    for (name, text) in seekdeep_python_sdk::bindings::runtime_bindings(name)? {
        std::fs::write(package.join(name), text)?;
    }
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data = repository.join("python/sdk-runtime/src/deepseek_harness_runtime");
    std::fs::copy(
        data.join("seekdeep-harness-runtime.json"),
        package.join("seekdeep-harness-runtime.json"),
    )?;
    std::fs::copy(
        data.join("runtime/cordis.yml"),
        package.join("runtime/cordis.yml"),
    )?;
    let tests =
        std::fs::read_to_string(source.join("python/sdk/tests/test_runtime_resolution.py"))?
            .replace("@deepseek-ai/", "@seekdeep-ai/")
            .replace("dsh-", "seekdeep-")
            .replace("DSH_", "SEEKDEEP_")
            .replace("DeepSeek Harness", "SeekDeep Harness")
            .replace("deepseek-harness", "seekdeep-harness");
    let test = temporary.path().join("test_runtime_resolution.py");
    std::fs::write(&test, tests)?;
    let python = std::env::var_os("SEEKDEEP_PYTHON_TEST_PYTHON")
        .map_or_else(|| source.join("python/sdk/.venv/bin/python"), PathBuf::from);
    let status = Command::new(python)
        .args([
            "-m",
            "pytest",
            "-p",
            "no:cacheprovider",
            "-o",
            "addopts=",
            "-q",
        ])
        .arg(test)
        .env("PYTHONPATH", temporary.path())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(temporary.path())
        .status()?;
    if !status.success() {
        return Err("pinned runtime-resolution suite failed against native bindings".into());
    }
    Ok(())
}
