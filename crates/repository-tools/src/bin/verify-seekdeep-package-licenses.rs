//! Command-line entry point for first-party package license declarations.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, package_licenses::inspect_seekdeep_package_licenses,
};

fn main() -> ExitCode {
    match inspect_seekdeep_package_licenses(compiled_repository_root()) {
        Ok(report) if report.failures.is_empty() => {
            println!(
                "verify-seekdeep-package-licenses: {} SeekDeep package(s) checked; all declare MIT.",
                report.package_count
            );
            ExitCode::SUCCESS
        }
        Ok(report) => {
            eprintln!(
                "verify-seekdeep-package-licenses: non-MIT SeekDeep package declarations found:"
            );
            for failure in report.failures {
                eprintln!("  {failure}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-seekdeep-package-licenses: {error:#}");
            ExitCode::FAILURE
        }
    }
}
