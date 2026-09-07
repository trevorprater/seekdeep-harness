//! Print the minimal-update briefing for out-of-sync translation pairs:
//! `gen-translation-brief [--apply] [pair paths...]`.

use std::process::ExitCode;

use seekdeep_repository_tools::translation_brief_command::{BriefOutcome, run_translation_brief};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let root = std::env::current_dir().expect("current directory");
    let run = match run_translation_brief(&root, &arguments) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("gen-translation-brief: {error}");
            return ExitCode::from(1);
        }
    };
    for notice in &run.notices {
        eprintln!("{notice}");
    }
    match run.outcome {
        BriefOutcome::UnknownFlags(flags) => {
            eprintln!(
                "gen-translation-brief: unknown flag(s): {} (only --apply is supported)",
                flags.join(", ")
            );
            ExitCode::from(2)
        }
        BriefOutcome::Problems(messages) => {
            for message in messages {
                eprintln!("gen-translation-brief: {message}");
            }
            ExitCode::from(2)
        }
        BriefOutcome::Nothing => {
            println!(
                "gen-translation-brief: every recorded pair matches its consistency record; nothing to brief."
            );
            ExitCode::SUCCESS
        }
        BriefOutcome::Briefs(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
    }
}
