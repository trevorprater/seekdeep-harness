//! Repository-wide Loader metadata and package-resolution verifier.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    cordis_config_verifier::{inspect_cordis_config, render_cordis_config_report},
};

fn main() -> ExitCode {
    match inspect_cordis_config(compiled_repository_root()) {
        Ok(report) => {
            let output = render_cordis_config_report(&report);
            if report.errors.is_empty() {
                print!("{output}");
                ExitCode::SUCCESS
            } else {
                eprint!("{output}");
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-cordis-config: {error:#}");
            ExitCode::FAILURE
        }
    }
}
