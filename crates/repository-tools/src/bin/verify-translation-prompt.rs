//! Command-line translation prompt verification and snapshot rendering.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    translation_prompt_verifier::verify_translation_prompt,
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let snapshot = match arguments.as_slice() {
        [] => false,
        [argument] if argument == "--snapshot" => true,
        [argument, ..] => {
            eprintln!(
                "verify-translation-prompt: unsupported argument {}",
                serde_json::to_string(argument).unwrap_or_else(|_| "\"\"".to_owned())
            );
            return ExitCode::FAILURE;
        }
    };
    match verify_translation_prompt(compiled_repository_root(), snapshot) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("verify-translation-prompt: {error:#}");
            ExitCode::FAILURE
        }
    }
}
