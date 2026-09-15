//! Native manifest-backed TypeScript declaration equivalence gate.

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, type_equiv::verify_type_equiv,
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
    match verify_type_equiv(&root) {
        Ok(report) => {
            if report.passed() {
                print!("{}", report.render());
                ExitCode::SUCCESS
            } else {
                eprint!("{}", report.render());
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
