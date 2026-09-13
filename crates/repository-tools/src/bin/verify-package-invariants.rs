//! Command-line package invariant ownership verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    package_invariants::{
        collect_package_invariant_violations, package_invariant_owners,
        render_package_invariant_report,
    },
};

fn main() -> ExitCode {
    let root = compiled_repository_root();
    match collect_package_invariant_violations(root) {
        Ok(violations) => {
            let owners = package_invariant_owners(root).map_or(0, |owners| owners.len());
            let passed = violations.is_empty();
            if passed {
                print!("{}", render_package_invariant_report(owners, &violations));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_package_invariant_report(owners, &violations));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-package-invariants: {error:#}");
            ExitCode::FAILURE
        }
    }
}
