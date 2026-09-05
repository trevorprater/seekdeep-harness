//! Native executable and developer-carrier packaging entry point.

use std::{path::Path, process::ExitCode};

use seekdeep_python_release::executable::{CliOutcome, Host, build_executables, parse_cli, usage};

fn main() -> ExitCode {
    let host = Host::current();
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let options = match parse_cli(&arguments, &host) {
        Ok(CliOutcome::Help) => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Ok(CliOutcome::Build(options)) => options,
        Err(error) => {
            eprintln!("{error}");
            if error.show_usage {
                eprintln!("\n{}", usage());
            }
            return ExitCode::FAILURE;
        }
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match build_executables(&root, &options, &host) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
