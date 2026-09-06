//! Source settings scenarios through the production Web profile and durable settings service.

use std::{path::Path, process::Command};

pub(super) fn run(source: &Path) -> anyhow::Result<()> {
    run_case(source, "web-settings", super::web_settings_driver::DRIVER)
}

pub(super) fn run_models(source: &Path) -> anyhow::Result<()> {
    run_case(
        source,
        "web-models-settings",
        super::web_models_settings_driver::DRIVER,
    )
}

pub(super) fn run_plugins(source: &Path) -> anyhow::Result<()> {
    run_case(
        source,
        "web-plugin-settings",
        super::web_plugin_settings_driver::DRIVER,
    )
}

fn run_case(source: &Path, name: &str, script: &str) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let host = metadata
        .target_directory
        .join("debug/examples/keyless_web_host");
    anyhow::ensure!(
        host.is_file(),
        "build xtask and keyless_web_host together before web-settings"
    );
    let temporary = tempfile::tempdir()?;
    let world = temporary.path().canonicalize()?;
    let output = metadata.target_directory.join("xtask").join(name);
    std::fs::create_dir_all(&output)?;
    let driver = output.join("browser.mjs");
    std::fs::write(&driver, script)?;
    let status = Command::new("node")
        .arg(driver)
        .arg(source)
        .arg(host)
        .arg(world)
        .arg(output)
        .current_dir(metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "settings browser path failed");
    Ok(())
}
