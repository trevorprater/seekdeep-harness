//! Native `landlock-run` launcher entry point.

fn main() {
    let result = seekdeep_landlock_run::parse_args(std::env::args_os().skip(1))
        .and_then(seekdeep_landlock_run::execute);
    if let Err(error) = result {
        eprintln!("{}", error.diagnostic());
        std::process::exit(seekdeep_landlock_run::LAUNCHER_FAILURE_EXIT);
    }
}
