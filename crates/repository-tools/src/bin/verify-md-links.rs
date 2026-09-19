//! Command-line relative Markdown link verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    md_links::{inspect_markdown_links, render_markdown_link_report},
};

fn main() -> ExitCode {
    match inspect_markdown_links(compiled_repository_root()) {
        Ok(report) => {
            let passed = report.violations.is_empty();
            if passed {
                print!("{}", render_markdown_link_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_markdown_link_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-md-links: {error:#}");
            ExitCode::FAILURE
        }
    }
}
