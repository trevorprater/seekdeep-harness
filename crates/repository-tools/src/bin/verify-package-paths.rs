//! Command-line package-reference drift verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    package_paths::{inspect_package_paths, render_package_path_report},
};

fn main() -> ExitCode {
    match inspect_package_paths(compiled_repository_root()) {
        Ok(report) => {
            let passed = report.violations.is_empty();
            if passed {
                print!("{}", render_package_path_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_package_path_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-package-paths: {error:#}");
            ExitCode::FAILURE
        }
    }
}
