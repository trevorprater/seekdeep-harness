//! Command-line packed install/run verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    release_families::ReleaseFamily, release_verify_packed_install::verify_packed_install,
};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut family = None;
    let mut directories = Vec::new();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--family" => family = args.next(),
            "--from" => {
                if let Some(path) = args.next() {
                    directories.push(std::path::PathBuf::from(path));
                }
            }
            _ if argument.starts_with("--family=") => {
                family = argument.strip_prefix("--family=").map(str::to_owned);
            }
            _ if argument.starts_with("--from=") => {
                if let Some(path) = argument.strip_prefix("--from=") {
                    directories.push(path.into());
                }
            }
            _ => {
                eprintln!("release verify-packed-install: unknown argument {argument}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(family) = family else {
        eprintln!(
            "usage: verify-packed-install --family <seekdeep|vendor> --from <packed directory> [--from ...]"
        );
        return ExitCode::from(2);
    };
    if directories.is_empty() {
        eprintln!(
            "usage: verify-packed-install --family <seekdeep|vendor> --from <packed directory> [--from ...]"
        );
        return ExitCode::from(2);
    }
    match ReleaseFamily::resolve(&family)
        .and_then(|family| verify_packed_install(family, &directories))
    {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("release verify-packed-install: {error:#}");
            ExitCode::FAILURE
        }
    }
}
