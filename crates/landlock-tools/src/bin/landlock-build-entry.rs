//! Build the Rust/WASM compatibility entry for the native package family.

use std::path::PathBuf;

use clap::Parser;
use seekdeep_landlock_tools::{
    entry::build_entry,
    process::{NativeRunner, exit_code},
    repo::{Repository, default_root},
};

#[derive(Parser)]
struct Arguments {
    #[arg(long, default_value_os_t = default_root())]
    root: PathBuf,
}

fn main() {
    let args = Arguments::parse();
    if let Err(error) = build_entry(&Repository::new(args.root), &mut NativeRunner) {
        eprintln!("{error}");
        std::process::exit(exit_code(&error));
    }
}
