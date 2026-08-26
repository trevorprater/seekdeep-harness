//! Command-line release version bump and commit workflow.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    release_bump_command::{ReleaseBumpOptions, bump_release},
    release_families::ReleaseFamily,
};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut family = None;
    let mut prerelease = None;
    let mut dry_run = false;
    let mut positionals = Vec::new();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--family" => family = args.next(),
            "--prerelease" => prerelease = args.next(),
            "--dry-run" => dry_run = true,
            _ if argument.starts_with("--family=") => {
                family = argument.strip_prefix("--family=").map(str::to_owned)
            }
            _ if argument.starts_with("--prerelease=") => {
                prerelease = argument.strip_prefix("--prerelease=").map(str::to_owned)
            }
            _ if argument.starts_with("--") => {
                eprintln!("release bump: unknown argument {argument}");
                return ExitCode::from(2);
            }
            _ => positionals.push(argument),
        }
    }
    let Some(family) = family else {
        eprintln!("usage: bump --family <seekdeep|vendor> [version]");
        return ExitCode::from(2);
    };
    let result = ReleaseFamily::resolve(&family).and_then(|family| {
        bump_release(
            compiled_repository_root(),
            &ReleaseBumpOptions {
                family,
                version: positionals.first().cloned(),
                prerelease,
                dry_run,
            },
        )
    });
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("release bump: {error:#}");
            ExitCode::FAILURE
        }
    }
}
