//! Command-line release-family pack boundary.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    release_families::ReleaseFamily,
    release_pack::{pack_release, render_release_pack_result},
};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut family = None;
    let mut output = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--family" => family = args.next(),
            "--out" => output = args.next(),
            _ if argument.starts_with("--family=") => {
                family = argument.strip_prefix("--family=").map(str::to_owned);
            }
            _ if argument.starts_with("--out=") => {
                output = argument.strip_prefix("--out=").map(str::to_owned);
            }
            _ => {
                eprintln!("release pack: unknown argument {argument}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(family) = family else {
        eprintln!("usage: pack --family <seekdeep|vendor> [--out dist/npm]");
        return ExitCode::from(2);
    };
    let display = output.unwrap_or_else(|| "dist/npm".to_owned());
    let root = compiled_repository_root();
    let result = ReleaseFamily::resolve(&family)
        .and_then(|family| pack_release(root, family, &root.join(&display)));
    match result {
        Ok(result) => {
            print!("{}", render_release_pack_result(&result, &display));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("release pack: {error:#}");
            ExitCode::FAILURE
        }
    }
}
