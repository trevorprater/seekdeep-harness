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
