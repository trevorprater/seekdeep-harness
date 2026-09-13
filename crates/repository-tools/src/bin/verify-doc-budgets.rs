//! Command-line entry point for standing-document word budgets.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, document_budgets::inspect_document_budgets,
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let list_only = arguments.iter().any(|argument| argument == "--list");
    match inspect_document_budgets(compiled_repository_root()) {
        Ok(report) if list_only => {
            println!("{}", report.rows.join("\n"));
            ExitCode::SUCCESS
        }
        Ok(report) if report.failures.is_empty() => {
            println!(
                "verify-doc-budgets: {} budgeted docs within ceiling.",
                report.budgeted_documents
            );
            ExitCode::SUCCESS
        }
        Ok(report) => {
            eprintln!("verify-doc-budgets failed:\n");
            for failure in report.failures {
                eprintln!("  {failure}");
            }
            eprintln!(
                "\nSee docs/AGENTS.md for the documentation standard and the relocation-first rule."
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("verify-doc-budgets: {error:#}");
            ExitCode::FAILURE
        }
    }
}
