//! One test binary per crate: every top-level test file is a module here.

mod dump_config_process;
mod headless_application;
mod headless_process;
mod headless_profile_snapshot;
mod headless_signal_process;
mod layered_env_process;
mod plugin_process;
mod profile_boot_parity;
mod profile_signal_process;
mod shipped_cli_contracts;
mod source_launch_compat;
mod web_replay_browser;
mod web_scaffold_contracts;
mod workflow_worker_process;

#[path = "support/web_replay_browser.rs"]
mod browser_driver;

// A production profile owns process-global signal/shutdown hooks and the shared
// Node integration. Keep those in-process fixtures from overlapping; assertions
// within each fixture, including explicit concurrent operations, remain intact.
static PROFILE_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn profile_fixture_guard() -> tokio::sync::MutexGuard<'static, ()> {
    PROFILE_FIXTURE_LOCK.lock().await
}

fn platform_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    ["SystemRoot", "SystemDrive", "WINDIR", "COMSPEC", "PATHEXT"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
        .collect()
}

fn node_current_dir(path: &std::path::Path) -> std::path::PathBuf {
    let output = std::process::Command::new("node")
        .args(["-e", "process.stdout.write(process.cwd())"])
        .current_dir(path)
        .output()
        .expect("Node cwd oracle");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::path::PathBuf::from(String::from_utf8(output.stdout).expect("Node cwd is UTF-8"))
}
