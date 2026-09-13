//! Platform-package prepack gate for executable native payloads.

use std::path::PathBuf;

use clap::Parser;
use seekdeep_landlock_tools::repo::{default_root, verify_platform_binaries};

#[derive(Parser)]
struct Arguments {
    #[arg(long, default_value_os_t = default_root())]
    root: PathBuf,
    package_dir: Option<PathBuf>,
}

fn main() {
    let args = Arguments::parse();
    let result = args
        .package_dir
        .map_or_else(std::env::current_dir, |package| Ok(args.root.join(package)))
        .map_err(anyhow::Error::from)
        .and_then(|package| verify_platform_binaries(&package));
    match result {
        Ok(result) => println!(
            "verify-launcher-binary: {} — {} binaries present with the right ELF architecture.",
            result.name, result.count
        ),
        Err(error) => {
            eprintln!("verify-launcher-binary: {error}");
            std::process::exit(1);
        }
    }
}
