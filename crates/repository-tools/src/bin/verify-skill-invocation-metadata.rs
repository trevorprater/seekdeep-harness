//! Command-line entry point for cross-product Skill invocation policy.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    skill_invocation_metadata::inspect_skill_invocation_metadata,
};

fn main() -> ExitCode {
    match inspect_skill_invocation_metadata(compiled_repository_root()) {
        Ok(report) if report.violations.is_empty() => {
            println!(
                "verify-skill-invocation-metadata: {} cross-product skill policy pair(s) aligned.",
                report.pair_count
            );
            ExitCode::SUCCESS
        }
        Ok(report) => {
            eprintln!("verify-skill-invocation-metadata: violations found:");
            for violation in report.violations {
                eprintln!("  {violation}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-skill-invocation-metadata: {error:#}");
            ExitCode::FAILURE
        }
    }
}
