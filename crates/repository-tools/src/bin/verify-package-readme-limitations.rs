//! Command-line package-README limitations verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    package_readme_limitations::{
        inspect_package_readme_limitations, render_package_readme_limitations_report,
    },
};

fn main() -> ExitCode {
    match inspect_package_readme_limitations(compiled_repository_root()) {
        Ok(report) => {
            let passed = report.failures.is_empty();
            if passed {
                print!("{}", render_package_readme_limitations_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_package_readme_limitations_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-package-readme-limitations: {error:#}");
            ExitCode::FAILURE
        }
    }
}
