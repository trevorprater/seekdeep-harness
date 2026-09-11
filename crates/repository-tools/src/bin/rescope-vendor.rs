//! Command-line entry for the Rust-owned vendor rescope migration.

use std::{path::PathBuf, process::ExitCode};

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    rescope_vendor::{Mode, run},
};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let root = args
        .iter()
        .position(|arg| arg == "--root")
        .and_then(|index| args.get(index + 1))
        .map_or_else(|| compiled_repository_root().to_owned(), PathBuf::from);
    let mode = if args.iter().any(|arg| arg == "--apply") {
        Mode::Apply
    } else if args.iter().any(|arg| arg == "--check") {
        Mode::Check
    } else {
        Mode::Dry
    };
    match run(&root, mode, args.iter().any(|arg| arg == "--reverse")) {
        Ok(report) => {
            print!("{}", report.stdout);
            eprint!("{}", report.stderr);
            if report.failures.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("rescope-vendor: {error:#}");
            ExitCode::FAILURE
        }
    }
}
