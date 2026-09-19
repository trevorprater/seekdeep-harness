//! The public coverage command: the instrumented Rust lane.

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, coverage_uncovered_locations::run_coverage,
};

fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let code = match run_coverage(compiled_repository_root(), &arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    };
    std::process::exit(code)
}
