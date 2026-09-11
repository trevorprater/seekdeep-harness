//! Native prerequisite and launcher for the public coverage command.

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, coverage_uncovered_locations::run_coverage,
};

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let code = match run_coverage(compiled_repository_root(), &arguments) {
        Ok(status) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt as _;
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
            }
            #[cfg(not(unix))]
            status.code().unwrap_or(1)
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    };
    std::process::exit(code)
}
