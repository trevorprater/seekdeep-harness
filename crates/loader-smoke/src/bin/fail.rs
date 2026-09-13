//! Non-zero subprocess fixture for the Loader-smoke harness.

fn main() -> std::process::ExitCode {
    eprintln!("fixture failed");
    std::process::ExitCode::from(7)
}
