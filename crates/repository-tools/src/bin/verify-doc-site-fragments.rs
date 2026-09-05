//! Command-line built documentation fragment verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    doc_site_fragments::{inspect_site_fragments, render_site_fragment_report},
};

fn main() -> ExitCode {
    let root = compiled_repository_root()
        .canonicalize()
        .unwrap_or_else(|_| compiled_repository_root().to_owned());
    let dist_root = root.join("website/.dist");
    match inspect_site_fragments(&dist_root) {
        Ok(report) => {
            let passed = report.broken.is_empty();
            if passed {
                print!("{}", render_site_fragment_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_site_fragment_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
