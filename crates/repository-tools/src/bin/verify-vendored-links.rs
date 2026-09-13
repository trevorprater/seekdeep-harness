//! Command-line entry point for vendored lockfile links.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, vendored_links::inspect_vendored_links,
};

fn main() -> ExitCode {
    match inspect_vendored_links(compiled_repository_root()) {
        Ok(report) if report.violations.is_empty() => {
            println!(
                "verify-vendored-links: all {} vendored package names resolve to workspace links.",
                report.vendored_packages
            );
            ExitCode::SUCCESS
        }
        Ok(report) => {
            eprintln!(
                "verify-vendored-links: {} lockfile resolution(s) bypass the vendored workspaces:",
                report.violations.len()
            );
            for violation in report.violations {
                eprintln!("  - {violation}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-vendored-links: {error:#}");
            ExitCode::FAILURE
        }
    }
}
