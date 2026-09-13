//! Command-line built package invariant verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    built_package_invariants::{render_built_invariant_report, verify_built_package_invariants},
};

fn main() -> ExitCode {
    match verify_built_package_invariants(compiled_repository_root(), None) {
        Ok(report) => {
            let passed = report.failures.is_empty();
            if passed {
                print!("{}", render_built_invariant_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_built_invariant_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-built-package-invariants: {error:#}");
            ExitCode::FAILURE
        }
    }
}
