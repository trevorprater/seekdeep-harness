//! Entry-package prepack gate for compiled compatibility exports.

use std::path::PathBuf;

use clap::Parser;
use seekdeep_landlock_tools::{entry::verify_entry_wasm, repo::verify_entry_lib};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    rust_wasm: bool,
    package_dir: Option<PathBuf>,
}

fn main() {
    let args = Arguments::parse();
    let result = args
        .package_dir
        .map_or_else(std::env::current_dir, Ok)
        .map_err(anyhow::Error::from)
        .and_then(|package| {
            if args.rust_wasm {
                verify_entry_wasm(&package)
            } else {
                verify_entry_lib(&package)
            }
        });
    match result {
        Ok(message) => println!("{message}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
