//! Package-owned consumer declarations emitted by the same Remote contract backend.

mod capture;
mod consumer;
mod public;

use std::{
    path::{Component, Path, PathBuf},
    process::Command,
};

use seekdeep_typert_generator::{emitter::FaceModelEmitter, model::FaceModel};
use serde_json::Value;

pub(super) fn run(output_root: &Path, check: bool, source: Option<&Path>) -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let output_root = root.join(output_root);
    let model: Value = serde_json::from_slice(&std::fs::read(
        root.join("crates/api-remotes-client/contracts/host-model.json"),
    )?)?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin absent"))?;
    anyhow::ensure!(
        model["sourceCommit"] == pin,
        "contract model has a stale source pin"
    );
    if let Some(source) = source {
        capture_declarations(&root, source, pin, check)?;
    }
    let face: FaceModel = serde_json::from_value(model["face"].clone())?;
    let emitter = FaceModelEmitter::new(&face);
    for package in &face.packages {
        let package_path = Path::new(&package.root);
        anyhow::ensure!(
            package_path.components().count() == 3
                && package_path.starts_with("packages")
                && package_path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "invalid Remote package root: {}",
            package.root
        );
        let remote = emitter.emit(&package.name)?.remote.ok_or_else(|| {
            anyhow::anyhow!("Remote package has no contribution: {}", package.name)
        })?;
        let output = output_root.join(package_path).join("lib");
        for (name, content) in [
            ("typert.remote-client.d.ts", remote.dts),
            ("typert.remote-client.d.ts.map", remote.dts_map),
        ] {
            let path = output.join(name);
            let content = super::normalize(&content);
            if check {
                anyhow::ensure!(
                    std::fs::read_to_string(&path)? == content,
                    "stale Remote declaration: {}",
                    path.display()
                );
            } else {
                std::fs::create_dir_all(&output)?;
                std::fs::write(path, content)?;
            }
        }
    }
    println!(
        "published {} generated Remote declaration pairs",
        face.packages.len()
    );
    public::write_all(&root, &output_root, check)?;
    Ok(())
}

pub(super) fn write_package(root: &Path, module_id: &str, output: &Path) -> anyhow::Result<()> {
    public::write_package(root, module_id, output)
}

pub(super) fn consumer() -> anyhow::Result<()> {
    let metadata = super::super::cargo_metadata()?;
    let output = metadata.target_directory.join("xtask/remote-consumer");
    std::fs::create_dir_all(&output)?;
    let driver = output.join("consumer.mjs");
    std::fs::write(&driver, consumer::SCRIPT)?;
    let status = Command::new("node")
        .arg(driver)
        .arg(&metadata.workspace_root)
        .arg(&output)
        .current_dir(&metadata.workspace_root)
        .status()?;
    anyhow::ensure!(
        status.success(),
        "public Remote declaration consumer failed"
    );
    Ok(())
}

fn capture_declarations(root: &Path, source: &Path, pin: &str, check: bool) -> anyhow::Result<()> {
    super::super::verify_source(source)?;
    let output = Command::new("node")
        .args(["--input-type=module", "-e", capture::SCRIPT])
        .arg(source)
        .arg(root.join("crates/api-remotes-client/contracts/host-model.json"))
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "public declaration capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut model: Value = serde_json::from_slice(&output.stdout)?;
    model["sourceCommit"] = pin.into();
    println!(
        "captured {} canonical declaration modules",
        model["modules"].as_array().map_or(0, Vec::len)
    );
    super::publish(
        &root.join("crates/api-remotes-client/contracts/client-declarations.json"),
        &model,
        check,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_source_owned_package_maps_and_detects_stale_declarations() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        run(directory.path(), false, None)?;
        run(directory.path(), true, None)?;
        let goal = directory
            .path()
            .join("packages/goal/goal/lib/typert.remote-client.d.ts");
        let content = std::fs::read_to_string(&goal)?;
        assert!(content.contains("interface TypertRemoteNamespace$676f616c73"));
        assert!(content.contains("@seekdeep-ai/seekdeep-goal/client"));
        assert!(content.contains("'agent:goals/create'"));
        let map: Value = serde_json::from_slice(&std::fs::read(goal.with_extension("ts.map"))?)?;
        assert_eq!(map["sources"], serde_json::json!(["../src/index.ts"]));
        std::fs::write(&goal, "export {};\n")?;
        assert!(run(directory.path(), true, None).is_err());
        run(directory.path(), false, None)?;
        let header = directory
            .path()
            .join("packages/core/session/lib/types/types.d.ts");
        std::fs::write(header, "export type SessionId = string;\n")?;
        assert!(run(directory.path(), true, None).is_err());
        Ok(())
    }
}
