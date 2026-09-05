//! Canonical foreign-language headers; runtime implementations remain compiled Rust.

use std::path::{Component, Path};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    format_version: u32,
    source_commit: String,
    modules: Vec<Module>,
    packages: Vec<Package>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Module {
    package_root: String,
    output: String,
    content: String,
}

#[derive(Deserialize)]
struct Package {
    root: String,
    name: String,
}

fn read_model(root: &Path) -> anyhow::Result<Model> {
    let model: Model = serde_json::from_slice(&std::fs::read(
        root.join("crates/api-remotes-client/contracts/client-declarations.json"),
    )?)?;
    let pin = include_str!("../../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin absent"))?;
    anyhow::ensure!(
        model.format_version == 1,
        "unsupported public declaration model"
    );
    anyhow::ensure!(
        model.source_commit == pin,
        "public declarations have a stale source pin"
    );
    for module in &model.modules {
        let output = Path::new(&module.output);
        let prefix = Path::new(&module.package_root).join("lib/types");
        anyhow::ensure!(
            output.starts_with(prefix)
                && module.output.ends_with(".d.ts")
                && output
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "invalid public declaration output: {}",
            module.output
        );
    }
    Ok(model)
}

pub(super) fn write_all(root: &Path, output: &Path, check: bool) -> anyhow::Result<()> {
    let model = read_model(root)?;
    for module in &model.modules {
        write(&output.join(&module.output), &module.content, check)?;
    }
    println!(
        "published {} canonical compatibility declarations",
        model.modules.len()
    );
    Ok(())
}

pub(super) fn write_package(root: &Path, module_id: &str, output: &Path) -> anyhow::Result<()> {
    let model = read_model(root)?;
    let Some(package) = model
        .packages
        .iter()
        .find(|package| super::super::normalize(&package.name) == module_id)
    else {
        return Ok(());
    };
    let prefix = Path::new(&package.root).join("lib");
    for module in model
        .modules
        .iter()
        .filter(|module| module.package_root == package.root)
    {
        let relative = Path::new(&module.output).strip_prefix(&prefix)?;
        write(&output.join(relative), &module.content, false)?;
    }
    Ok(())
}

fn write(path: &Path, content: &str, check: bool) -> anyhow::Result<()> {
    let content = super::super::normalize(content);
    if check {
        anyhow::ensure!(
            std::fs::read_to_string(path)? == content,
            "stale compatibility declaration: {}",
            path.display()
        );
    } else {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("declaration parent absent"))?,
        )?;
        std::fs::write(path, content)?;
    }
    Ok(())
}
