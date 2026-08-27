//! Scoped-event catalog generation and freshness command.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, scoped_events_generator::run_scoped_events_generator,
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let check = match arguments.as_slice() {
        [] => false,
        [argument] if argument == "--check" => true,
        _ => {
            eprintln!("gen-scoped-events: expected no arguments or --check");
            return ExitCode::FAILURE;
        }
    };
    match run_scoped_events_generator(compiled_repository_root(), check) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
