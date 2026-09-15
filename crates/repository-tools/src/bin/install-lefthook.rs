//! Worktree-local Git hook and translation-pairing driver installer.

use std::process::ExitCode;

use seekdeep_repository_tools::lefthook_installer::run_lefthook_postinstall;

fn main() -> ExitCode {
    match run_lefthook_postinstall() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[install-lefthook] {error:#}");
            ExitCode::FAILURE
        }
    }
}
