//! Command-line entry point for the Agent Note format gate.

use std::process::ExitCode;

use seekdeep_repository_tools::agent_note_tree::{
    compiled_repository_root, verify_agent_note_format,
};

fn main() -> ExitCode {
    match verify_agent_note_format(compiled_repository_root()) {
        Ok(result) if result.errors.is_empty() => {
            println!(
                "verify-agent-note-format: {} Agent Note(s) checked, all conform to .agents/notes/README.md § The file format.",
                result.checked
            );
            ExitCode::SUCCESS
        }
        Ok(result) => {
            eprintln!("verify-agent-note-format: violations found:");
            for error in result.errors {
                eprintln!("  {error}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-agent-note-format: {error:#}");
            ExitCode::FAILURE
        }
    }
}
