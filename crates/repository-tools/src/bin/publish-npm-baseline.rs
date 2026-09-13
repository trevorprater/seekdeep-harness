//! Builds, publishes, and verifies commit-addressed npm workspace baselines.

use std::process::ExitCode;

use seekdeep_repository_tools::npm_baseline::{SystemBaselineRunner, baseline_main};

fn main() -> ExitCode {
    let result = std::env::current_dir()
        .map_err(anyhow::Error::from)
        .and_then(|cwd| {
            baseline_main(
                &std::env::args().skip(1).collect::<Vec<_>>(),
                &cwd,
                &std::env::vars_os().collect(),
                chrono::Utc::now(),
                &mut SystemBaselineRunner,
            )
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("publish-npm-baseline: {error}");
            ExitCode::FAILURE
        }
    }
}
