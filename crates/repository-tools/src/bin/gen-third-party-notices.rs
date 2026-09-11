//! Generates or verifies notices using the Rust dependency-discovery policy.

use std::{path::PathBuf, process::ExitCode};

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, third_party_notices::render,
};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let root = args
        .iter()
        .position(|arg| arg == "--root")
        .and_then(|index| args.get(index + 1))
        .map_or_else(|| compiled_repository_root().to_owned(), PathBuf::from);
    let result = (|| -> anyhow::Result<ExitCode> {
        let content = render(&root)?;
        let output = root.join("THIRD_PARTY_NOTICES.md");
        if args.iter().any(|arg| arg == "--check") {
            if std::fs::read_to_string(&output).ok().as_deref() == Some(content.as_str()) {
                println!("gen-third-party-notices: THIRD_PARTY_NOTICES.md is up to date.");
                return Ok(ExitCode::SUCCESS);
            }
            eprintln!(
                "gen-third-party-notices: THIRD_PARTY_NOTICES.md is stale. Run `pnpm run gen-third-party-notices` and commit THIRD_PARTY_NOTICES.md."
            );
            return Ok(ExitCode::FAILURE);
        }
        std::fs::write(output, content)?;
        println!("gen-third-party-notices: wrote THIRD_PARTY_NOTICES.md.");
        Ok(ExitCode::SUCCESS)
    })();
    match result {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
