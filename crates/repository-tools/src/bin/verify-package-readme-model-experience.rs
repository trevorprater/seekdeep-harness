//! Native package Model Experience checker.

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    package_readme_model_experience::{inspect_package_readme_model_experience, render_report},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    root: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match inspect_package_readme_model_experience(
        args.root
            .as_deref()
            .unwrap_or_else(|| compiled_repository_root()),
    ) {
        Ok(report) if report.failures.is_empty() => {
            print!("{}", render_report(&report));
            ExitCode::SUCCESS
        }
        Ok(report) => {
            eprint!("{}", render_report(&report));
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-package-readme-model-experience: {error:#}");
            ExitCode::FAILURE
        }
    }
}
