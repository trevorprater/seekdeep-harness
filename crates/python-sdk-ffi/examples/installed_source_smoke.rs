//! Exercise installed SDK/runtime wheels with the pinned source's keyless model server.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

use seekdeep_python_runtime_smoke::snapshot as smoke_snapshot;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [source, python] = arguments.as_slice() else {
        return Err("usage: installed_source_smoke <pinned-source> <installed-python>".into());
    };
    let source = Path::new(source).canonicalize()?;
    verify_pin(&source)?;
    let python = Path::new(python);
    if !python.is_absolute() || !python.is_file() {
        return Err("installed Python must be an absolute executable path".into());
    }
    let temporary = tempfile::tempdir()?;
    let executable = installed_runtime(python, temporary.path())?;
    let script_root = temporary.path().join("scripts");
    std::fs::create_dir(&script_root)?;
    let script = script_root.join("smoke-python-runtime.py");
    let source_script = port_text(&std::fs::read_to_string(
        source.join("scripts/smoke-python-runtime.py"),
    )?);
    let compare = "        compare_snapshot_files(files, update_snapshots)";
    if source_script.matches(compare).count() != 1 {
        return Err("source snapshot comparison call changed".into());
    }
    let capture = temporary.path().join("captured");
    std::fs::create_dir(&capture)?;
    std::fs::write(&script, source_script.replace(compare, concat!(
        "        for filename, content in files.items():\n",
        "            (Path(os.environ['SEEKDEEP_SMOKE_CAPTURE']) / filename).write_text(content, encoding='utf-8')"
    )))?;
    stage_fixture(
        &source.join("scripts/snapshots/python-sdk-single-exe/advanced"),
        &script_root.join("snapshots/python-sdk-single-exe/advanced"),
    )?;
    let minimal = source.join("examples/jsonrpc-agent/minimal.cordis.yml");
    let staged_minimal = temporary
        .path()
        .join("examples/jsonrpc-agent/minimal.cordis.yml");
    std::fs::create_dir_all(
        staged_minimal
            .parent()
            .ok_or("minimal config parent absent")?,
    )?;
    std::fs::write(
        &staged_minimal,
        port_text(&std::fs::read_to_string(minimal)?),
    )?;
    for scenario in [
        "sdk-default",
        "sdk-custom",
        "sdk-minimal",
        "sdk-snapshot",
        "direct",
    ] {
        let mut command = isolated_python(python, temporary.path());
        command.env("SEEKDEEP_SMOKE_CAPTURE", &capture);
        command.arg(&script).args(["--scenario", scenario]);
        if scenario != "sdk-default" {
            command.arg("--exe").arg(&executable);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(format!("installed source smoke {scenario} failed with {status}").into());
        }
        if scenario == "sdk-snapshot" {
            compare_snapshots(
                &script_root.join("snapshots/python-sdk-single-exe/advanced"),
                &capture,
            )?;
        }
    }
    println!(
        "installed SDK/runtime wheels passed default, custom, minimal, advanced snapshot, and direct source smokes"
    );
    Ok(())
}

fn compare_snapshots(expected: &Path, actual: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let names = |root: &Path| -> Result<std::collections::BTreeSet<_>, std::io::Error> {
        std::fs::read_dir(root)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    };
    let expected_names = names(expected)?;
    if expected_names != names(actual)? {
        return Err("advanced snapshot file set differs".into());
    }
    for name in expected_names {
        let expected_text = std::fs::read_to_string(expected.join(&name))?;
        let actual_text = std::fs::read_to_string(actual.join(&name))?;
        if actual_text == expected_text {
            continue;
        }
        if name == "result.json" {
            let expected =
                smoke_snapshot::canonical_workflow_starts(serde_json::from_str(&expected_text)?)?;
            let actual =
                smoke_snapshot::canonical_workflow_starts(serde_json::from_str(&actual_text)?)?;
            if serde_json::to_string(&expected)? == serde_json::to_string(&actual)? {
                println!(
                    "advanced snapshot: exact data and causal order match across source-supported worker scheduling"
                );
                continue;
            }
        }
        return Err(format!("advanced snapshot differs in {}", name.to_string_lossy()).into());
    }
    Ok(())
}

fn stage_fixture(source: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            return Err("source smoke fixture contains a symlink".into());
        }
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            stage_fixture(&entry.path(), &target)?;
        } else {
            std::fs::write(target, port_text(&std::fs::read_to_string(entry.path())?))?;
        }
    }
    Ok(())
}

fn port_text(source: &str) -> String {
    source
        .replace("@deepseek-ai/", "@seekdeep-ai/")
        .replace("dsh-", "seekdeep-")
        .replace("DSH_", "SEEKDEEP_")
        .replace("DeepSeek Harness", "SeekDeep Harness")
        .replace("deepseek-harness", "seekdeep-harness")
}

fn verify_pin(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("source pin absent")?;
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    Ok(())
}

fn isolated_python(python: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(python);
    command
        .current_dir(cwd)
        .env_remove("PYTHONPATH")
        .env("PYTHONNOUSERSITE", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1");
    command
}

fn installed_runtime(python: &Path, cwd: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let output = isolated_python(python, cwd)
        .args([
            "-c",
            concat!(
                "import json, sys, deepseek_harness as sdk, deepseek_harness_runtime as runtime; ",
                "print(json.dumps({'prefix':sys.prefix,'sdk':sdk.__file__,",
                "'runtime':runtime.__file__,'exe':str(runtime.bundled_runtime_path())}))"
            ),
        ])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let paths: Value = serde_json::from_slice(&output.stdout)?;
    let prefix =
        Path::new(paths["prefix"].as_str().ok_or("Python prefix absent")?).canonicalize()?;
    for field in ["sdk", "runtime", "exe"] {
        let path = Path::new(
            paths[field]
                .as_str()
                .ok_or("installed artifact path absent")?,
        )
        .canonicalize()?;
        if !path.starts_with(&prefix) {
            return Err(format!(
                "{field} was imported outside the isolated environment: {}",
                path.display()
            )
            .into());
        }
    }
    Ok(PathBuf::from(
        paths["exe"].as_str().ok_or("runtime path absent")?,
    ))
}
