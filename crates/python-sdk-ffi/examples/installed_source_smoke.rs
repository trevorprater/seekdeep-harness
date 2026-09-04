//! Exercise installed SDK/runtime wheels with the pinned source's keyless model server.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

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
    let script = temporary.path().join("smoke.py");
    let source_script = std::fs::read_to_string(source.join("scripts/smoke-python-runtime.py"))?
        .replace("@deepseek-ai/", "@seekdeep-ai/")
        .replace("dsh-", "seekdeep-")
        .replace("DSH_", "SEEKDEEP_")
        .replace("DeepSeek Harness", "SeekDeep Harness")
        .replace("deepseek-harness", "seekdeep-harness");
    std::fs::write(&script, source_script)?;
    for scenario in ["sdk-default", "sdk-custom", "direct"] {
        let mut command = isolated_python(python, temporary.path());
        command.arg(&script).args(["--scenario", scenario]);
        if scenario != "sdk-default" {
            command.arg("--exe").arg(&executable);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(format!("installed source smoke {scenario} failed with {status}").into());
        }
    }
    println!(
        "installed SDK/runtime wheels passed default, custom three-turn, and direct source smokes"
    );
    Ok(())
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
