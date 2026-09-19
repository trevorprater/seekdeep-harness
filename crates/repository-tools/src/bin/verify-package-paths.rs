//! Command-line package-reference drift verification.

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    package_paths::{inspect_package_paths, render_package_path_report},
};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    root: Option<PathBuf>,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let root = arguments
        .root
        .unwrap_or_else(|| compiled_repository_root().to_owned());
    match inspect_package_paths(&root) {
        Ok(report) => {
            let passed = report.violations.is_empty();
            if passed {
                print!("{}", render_package_path_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_package_path_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-package-paths: {error:#}");
            ExitCode::FAILURE
        }
    }
}
