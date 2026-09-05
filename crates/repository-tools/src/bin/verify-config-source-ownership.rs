//! Command-line entry point for shipped configuration source ownership.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    config_source_ownership::collect_config_source_ownership_violations,
};

fn main() -> ExitCode {
    match collect_config_source_ownership_violations(compiled_repository_root()) {
        Ok(failures) if failures.is_empty() => {
            println!(
                "verify-config-source-ownership: no credential or endpoint uses the ordinary inline environment form in shipped configuration."
            );
            ExitCode::SUCCESS
        }
        Ok(failures) => {
            eprintln!("verify-config-source-ownership: configuration source ownership violated:");
            for failure in failures {
                eprintln!("  {failure}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-config-source-ownership: {error:#}");
            ExitCode::FAILURE
        }
    }
}
