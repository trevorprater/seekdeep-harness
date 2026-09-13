//! Command-line entry point for unavailable public-repository links.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    public_repository_links::scan_public_repository_links,
};

fn main() -> ExitCode {
    match scan_public_repository_links(compiled_repository_root()) {
        Ok(references) if references.is_empty() => {
            println!(
                "verify-public-repository-links: tracked files reference no unavailable repository."
            );
            ExitCode::SUCCESS
        }
        Ok(references) => {
            eprintln!("verify-public-repository-links: unavailable repository references found:");
            for reference in references {
                eprintln!("  {}:{}", reference.file, reference.line);
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-public-repository-links: {error:#}");
            ExitCode::FAILURE
        }
    }
}
