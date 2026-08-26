//! Command-line entry for repository build-output cleanup.

use std::{path::PathBuf, process::ExitCode};

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, clean::RepositoryCleaner,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let root = match (arguments.next(), arguments.next()) {
        (None, None) => compiled_repository_root().to_owned(),
        (Some(flag), Some(root)) if flag == "--root" && arguments.next().is_none() => {
            PathBuf::from(root)
        }
        _ => {
            eprintln!("clean: usage: clean [--root <repository>]");
            return ExitCode::FAILURE;
        }
    };
    match RepositoryCleaner::new(root).clean() {
        Ok(removed) if removed.is_empty() => {
            println!("clean: already clean");
            ExitCode::SUCCESS
        }
        Ok(removed) => {
            println!("clean: removed {} paths", removed.len());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
