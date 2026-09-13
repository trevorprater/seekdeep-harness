//! Native Markdown TypeScript snippet compiler.

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    doc_typecheck::{CompileMode, check_documentation},
};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    use_build_output: bool,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let root = arguments
        .root
        .unwrap_or_else(|| compiled_repository_root().to_owned());
    let mode = if arguments.use_build_output
        || std::env::var("SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT")
            .ok()
            .as_deref()
            == Some("1")
        || std::env::var("DSH_DOC_TYPECHECK_USE_BUILD_OUTPUT")
            .ok()
            .as_deref()
            == Some("1")
    {
        CompileMode::BuiltTypes
    } else {
        CompileMode::Standalone
    };
    match check_documentation(&root, mode) {
        Ok(report) => {
            print!("{}", report.stdout);
            eprint!("{}", report.stderr);
            if report.passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
