//! Command-line bilingual pairing verification and recording.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, translation_pairing_command::run_translation_pairing,
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match run_translation_pairing(compiled_repository_root(), &arguments) {
        Ok(output) => {
            print!("{}", output.stdout);
            eprint!("{}", output.stderr);
            ExitCode::from(output.exit_code)
        }
        Err(error) => {
            eprintln!("verify-translation-pairing: {error:#}");
            ExitCode::from(2)
        }
    }
}
