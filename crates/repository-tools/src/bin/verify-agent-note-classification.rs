//! Command-line entry point for the Agent Note tree classification gate.

use std::process::ExitCode;

use seekdeep_repository_tools::agent_note_tree::{
    compiled_repository_root, verify_agent_note_classification,
};

fn main() -> ExitCode {
    match verify_agent_note_classification(compiled_repository_root()) {
        Ok(result) if result.errors.is_empty() => {
            println!(
                "verify-agent-note-classification: {} Agent Note(s) checked, structure consistent.",
                result.checked
            );
            ExitCode::SUCCESS
        }
        Ok(result) => {
            eprintln!("verify-agent-note-classification: violations found:");
            for error in result.errors {
                eprintln!("  {error}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-agent-note-classification: {error:#}");
            ExitCode::FAILURE
        }
    }
}
