//! Command-line release-family verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root, release_families::ReleaseFamily,
    release_verify::verify_release,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let mut family = None;
    while let Some(argument) = arguments.next() {
        if argument == "--family" {
            family = arguments.next();
        } else if let Some(value) = argument.strip_prefix("--family=") {
            family = Some(value.to_owned());
        } else {
            eprintln!("release verify: unknown argument {argument}");
            return ExitCode::from(2);
        }
    }
    let Some(family) = family else {
        eprintln!("usage: verify --family <seekdeep|vendor>");
        return ExitCode::from(2);
    };
    let result = ReleaseFamily::resolve(&family).and_then(|family| {
        verify_release(
            compiled_repository_root(),
            family,
            std::env::var("RELEASE_PUBLISH").as_deref() == Ok("true"),
            &std::env::var("GITHUB_REF").unwrap_or_default(),
        )
    });
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("release verify: {error:#}");
            ExitCode::FAILURE
        }
    }
}
