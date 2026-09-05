//! Command-line entry point for source-comment documentation references.

use std::{process::ExitCode, sync::LazyLock};

use regex::Regex;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    repo_files::{find_reference_violations, unique_repo_files},
};

static DOC_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\bdocs|\.agents/notes)/[A-Za-z0-9._/-]+\.md").expect("valid regex")
});

fn main() -> ExitCode {
    match verify() {
        Ok((checked, violations)) if violations.is_empty() => {
            println!(
                "verify-doc-refs: {checked} file(s) checked, all documentation references resolve."
            );
            ExitCode::SUCCESS
        }
        Ok((_, violations)) => {
            eprintln!(
                "verify-doc-refs: broken documentation references found in source comments (target does not exist):"
            );
            for violation in violations {
                eprintln!(
                    "  {}:{}  {}",
                    violation.file, violation.line, violation.reference
                );
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-doc-refs: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn verify() -> anyhow::Result<(
    usize,
    Vec<seekdeep_repository_tools::repo_files::ReferenceViolation>,
)> {
    let root = compiled_repository_root();
    let files = unique_repo_files(root, &["packages/**/*.ts", "examples/**/*.ts"], |path| {
        path.contains("/lib/") || path.ends_with(".d.ts") || path.starts_with("vendor/")
    })?;
    let mut violations = Vec::new();
    for file in &files {
        violations.extend(find_reference_violations(
            root,
            &file.absolute,
            &DOC_REFERENCE,
            str::to_owned,
            |reference| !root.join(reference).exists(),
        )?);
    }
    Ok((files.len(), violations))
}
