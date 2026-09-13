//! Command-line runtime workspace dependency-closure verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    runtime_closure::{inspect_runtime_closure, render_runtime_closure_report},
};

fn main() -> ExitCode {
    let root = compiled_repository_root();
    let mut args = std::env::args().skip(1);
    let mut manifest = None;
    while let Some(argument) = args.next() {
        if argument == "--manifest" {
            let Some(value) = args.next() else {
                eprintln!("verify-runtime-closure: --manifest requires a value");
                return ExitCode::from(2);
            };
            manifest = Some(value);
        } else if let Some(value) = argument.strip_prefix("--manifest=") {
            manifest = Some(value.to_owned());
        } else {
            eprintln!("verify-runtime-closure: unknown argument {argument}");
            return ExitCode::from(2);
        }
    }
    let manifest =
        root.join(manifest.unwrap_or_else(|| "python/sdk-runtime/package.json".to_owned()));
    match inspect_runtime_closure(root, &manifest) {
        Ok(report) => {
            let passed = report.failures.is_empty();
            if passed {
                print!("{}", render_runtime_closure_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_runtime_closure_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-runtime-closure: {error:#}");
            ExitCode::FAILURE
        }
    }
}
