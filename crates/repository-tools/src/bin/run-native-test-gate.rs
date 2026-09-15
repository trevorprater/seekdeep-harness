//! Compiled smoke and uninstrumented-heavy repository suite entrypoint.

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    native_test_gates::{parse_native_test_arguments, run_native_test_gate},
};

fn main() -> std::process::ExitCode {
    let arguments =
        match parse_native_test_arguments(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
            Ok(arguments) => arguments,
            Err(error) => error.exit(),
        };
    match run_native_test_gate(
        compiled_repository_root(),
        arguments.gate,
        arguments.workers,
    ) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
