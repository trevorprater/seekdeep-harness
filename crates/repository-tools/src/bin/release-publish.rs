//! Command-line packed release publication.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    release_families::ReleaseFamily,
    release_publish::{publish_release, render_release_publish_result},
};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut family = None;
    let mut directory = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--family" => family = args.next(),
            "--from" => directory = args.next(),
            _ if argument.starts_with("--family=") => {
                family = argument.strip_prefix("--family=").map(str::to_owned);
            }
            _ if argument.starts_with("--from=") => {
                directory = argument.strip_prefix("--from=").map(str::to_owned);
            }
            _ => {
                eprintln!("release publish: unknown argument {argument}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(family), Some(directory)) = (family, directory) else {
        eprintln!("usage: publish --family <seekdeep|vendor> --from <packed directory>");
        return ExitCode::from(2);
    };
    let result = ReleaseFamily::resolve(&family)
        .and_then(|family| publish_release(family, std::path::Path::new(&directory)));
    match result {
        Ok(result) => {
            print!("{}", render_release_publish_result(&result));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("release publish: {error:#}");
            ExitCode::FAILURE
        }
    }
}
