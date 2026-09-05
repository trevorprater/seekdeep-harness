//! Command-line entry point for the repository change-scope report.

use std::{io::Write as _, path::Path, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("change-scope: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let args = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| anyhow::anyhow!("arguments must be valid UTF-8"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let cwd = std::env::current_dir()?;
    let output = seekdeep_change_scope::render_change_scope(&args, Path::new(&cwd))?;
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}
