//! Command-line Mermaid documentation verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    mermaid::{render_mermaid_report, verify_mermaid},
};

fn main() -> ExitCode {
    match verify_mermaid(compiled_repository_root()) {
        Ok(report) => {
            let passed = report.violations.is_empty();
            if passed {
                print!("{}", render_mermaid_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_mermaid_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-mermaid: {error:#}");
            ExitCode::FAILURE
        }
    }
}
