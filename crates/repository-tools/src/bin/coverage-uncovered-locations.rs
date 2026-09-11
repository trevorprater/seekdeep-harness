//! Native Istanbul reporter bridge.

use std::{io::Read as _, process::ExitCode};

fn run() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request = serde_json::from_str(&input)?;
    let response = seekdeep_repository_tools::coverage_uncovered_locations::bridge(&request)?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coverage-uncovered-locations: {error:#}");
            ExitCode::FAILURE
        }
    }
}
