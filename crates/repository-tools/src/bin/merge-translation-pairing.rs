//! Git merge-driver and explicit conflict-resolver entrypoint for pairing records.

use std::{fs, path::Path, process::ExitCode};

use seekdeep_repository_tools::{
    translation_pairing_git::run_git,
    translation_pairing_merge::{
        merge_translation_pairing_records, resolve_translation_pairing_conflicts,
    },
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("merge-translation-pairing: {error:#}");
            eprintln!(
                "merge-translation-pairing: resolve owner conflicts, then confirm the pair with \
                 `pnpm run verify-translation-pairing --write <pair>`; rerun \
                 `pnpm run resolve-translation-pairing-conflicts` for other safe records"
            );
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> anyhow::Result<()> {
    if arguments
        .first()
        .is_some_and(|argument| argument == "--probe")
    {
        if arguments.len() != 1 {
            anyhow::bail!("--probe takes no other arguments");
        }
        return Ok(());
    }
    let root = repository_root()?;
    if arguments
        .first()
        .is_some_and(|argument| argument == "--resolve")
    {
        if arguments.len() != 1 {
            anyhow::bail!("--resolve takes no paths; it inspects the unmerged index");
        }
        let resolved = resolve_translation_pairing_conflicts(&root)?;
        if resolved.is_empty() {
            println!("merge-translation-pairing: no unresolved pairing records");
        } else {
            for path in resolved {
                println!("merge-translation-pairing: resolved {path}");
            }
        }
        return Ok(());
    }
    let [ancestor_path, current_path, other_path, metadata_path] = arguments else {
        anyhow::bail!("merge-driver mode requires <ancestor> <current> <other> <repository-path>");
    };
    let result = merge_translation_pairing_records(
        &root,
        metadata_path,
        &fs::read_to_string(ancestor_path)?,
        &fs::read_to_string(current_path)?,
        &fs::read_to_string(other_path)?,
    )?;
    fs::write(current_path, result.record)?;
    Ok(())
}

fn repository_root() -> anyhow::Result<std::path::PathBuf> {
    let output = run_git(
        Path::new("."),
        &["rev-parse".to_owned(), "--show-toplevel".to_owned()],
        "locating repository root",
        None,
    )?;
    Ok(std::path::PathBuf::from(
        String::from_utf8_lossy(&output).trim(),
    ))
}
