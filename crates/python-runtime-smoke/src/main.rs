//! Standalone installed-Python SDK and executable smoke entry.

use clap::{CommandFactory as _, Parser};
use seekdeep_python_runtime_smoke::{Options, Scenario};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Keyless full-turn and snapshot smoke for the Python SDK runtime.",
    infer_long_args = true,
    args_override_self = true
)]
struct Args {
    #[arg(long, value_enum, default_value = "all")]
    scenario: Scenario,
    #[arg(long)]
    exe: Option<PathBuf>,
    #[arg(long)]
    update_snapshots: bool,
    /// Interpreter whose installed SDK is exercised.
    #[arg(long, default_value = if cfg!(windows) { "python" } else { "python3" })]
    python: PathBuf,
    /// Checkout containing the minimal composition and checked-in snapshots.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))]
    root: PathBuf,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let options = Options {
        scenario: args.scenario,
        executable: args.exe,
        update_snapshots: args.update_snapshots,
        python: args.python,
        root: args.root,
    };
    if let Err(error) = options.validate() {
        Args::command()
            .error(clap::error::ErrorKind::InvalidValue, error.to_string())
            .exit();
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(seekdeep_python_runtime_smoke::run(options)));
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            if seekdeep_python_runtime_smoke::is_interrupted(&error) {
                std::process::ExitCode::from(130)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}
