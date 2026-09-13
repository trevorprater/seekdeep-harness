//! Command-line Oxlint orchestration.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    run_oxlint::{OXLINT_THREADS_ENV, complete_oxlint_process, run_oxlint},
};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let thread_bound = std::env::var(OXLINT_THREADS_ENV).ok();
    match run_oxlint(compiled_repository_root(), &args, thread_bound.as_deref())
        .and_then(complete_oxlint_process)
    {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
