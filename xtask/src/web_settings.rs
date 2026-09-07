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

pub(super) fn run_onboarding(source: &Path) -> anyhow::Result<()> {
    run_case(
        source,
        "web-onboarding",
        super::web_onboarding_driver::DRIVER,
    )
}

pub(super) fn run_model_selection(source: &Path) -> anyhow::Result<()> {
    run_case(
        source,
        "web-model-selection",
        super::web_model_selection_driver::DRIVER,
    )
}

pub(super) fn run_startup(source: &Path) -> anyhow::Result<()> {
    run_case(source, "web-startup", super::web_startup_driver::DRIVER)
}

pub(super) fn run_composer(source: &Path) -> anyhow::Result<()> {
    run_case(source, "web-composer", super::web_composer_driver::DRIVER)
}

pub(super) fn run_scrollbars(source: &Path) -> anyhow::Result<()> {
    run_case(
        source,
        "web-scrollbars",
        super::web_scrollbars_driver::DRIVER,
    )
}

pub(super) fn run_navigation(source: &Path) -> anyhow::Result<()> {
    run_case(
        source,
        "web-navigation",
        super::web_navigation_driver::DRIVER,
    )
}

pub(super) fn run_details(source: &Path) -> anyhow::Result<()> {
    run_case(source, "web-details", super::web_details_driver::DRIVER)
}

pub(super) fn run_keyless(source: &Path, scenario: Option<&str>) -> anyhow::Result<()> {
    let environment = scenario
        .map(|scenario| vec![("SEEKDEEP_KEYLESS_SCENARIO", scenario)])
        .unwrap_or_default();
    run_case_with(
        source,
        "web-keyless",
        super::web_keyless_driver::DRIVER,
        &environment,
    )
}

fn run_case(source: &Path, name: &str, script: &str) -> anyhow::Result<()> {
    run_case_with(source, name, script, &[])
}

fn run_case_with(
    source: &Path,
    name: &str,
    script: &str,
    environment: &[(&str, &str)],
) -> anyhow::Result<()> {
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
        .envs(environment.iter().copied())
        .current_dir(metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "settings browser path failed");
    Ok(())
}
