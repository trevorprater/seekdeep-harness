//! Workspace manifest and compiler-reference verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, workspace_constraints::inspect_workspace_constraints,
};

fn main() -> ExitCode {
    match inspect_workspace_constraints(compiled_repository_root()) {
        Ok(errors) if errors.is_empty() => ExitCode::SUCCESS,
        Ok(errors) => {
            eprintln!("{}", errors.join("\n"));
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("check-workspace-constraints: {error:#}");
            ExitCode::FAILURE
        }
    }
}
