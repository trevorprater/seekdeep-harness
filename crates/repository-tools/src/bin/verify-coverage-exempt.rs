//! Command-line coverage-exempt roster verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, coverage_exempt::verify_coverage_exempt,
};

fn main() -> ExitCode {
    match verify_coverage_exempt(compiled_repository_root()) {
        Ok(violations) if violations.is_empty() => {
            println!("verify-coverage-exempt: heavy suite roster is exact and disjoint.");
            ExitCode::SUCCESS
        }
        Ok(violations) => {
            eprintln!("verify-coverage-exempt: violations found:");
            for violation in violations {
                eprintln!("  {violation}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-coverage-exempt: {error:#}");
            ExitCode::FAILURE
        }
    }
}
