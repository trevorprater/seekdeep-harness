//! Command-line entry point for the frozen Agent Note archive verifier.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, archived_agent_notes::verify_archived_agent_notes,
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let write_mode = arguments.as_slice() == ["--write"];
    if !arguments.is_empty() && !write_mode {
        eprintln!("verify-archived-agent-notes: usage: verify-archived-agent-notes [--write]");
        return ExitCode::FAILURE;
    }
    let baseline = std::env::var("SEEKDEEP_ARCHIVE_BASE_REF").unwrap_or_else(|_| "HEAD".into());
    match verify_archived_agent_notes(compiled_repository_root(), write_mode, &baseline) {
        Ok(result) if result.errors.is_empty() && write_mode => {
            println!(
                "verify-archived-agent-notes: sealed {} new artifact(s); existing seals unchanged.",
                result.added
            );
            ExitCode::SUCCESS
        }
        Ok(result) if result.errors.is_empty() => {
            println!(
                "verify-archived-agent-notes: {} frozen artifact(s) checked across {} kind(s).",
                result.artifacts, result.kinds
            );
            ExitCode::SUCCESS
        }
        Ok(result) => {
            eprintln!("verify-archived-agent-notes: archive rules violated:");
            for error in result.errors {
                eprintln!("  {error}");
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-archived-agent-notes: {error:#}");
            ExitCode::FAILURE
        }
    }
}
