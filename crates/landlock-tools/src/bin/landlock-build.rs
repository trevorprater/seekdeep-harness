//! Native Linux build entry point for the Landlock package family.

use std::path::PathBuf;

use clap::Parser;
use seekdeep_landlock_tools::{
    build::{BuildHost, build_native},
    process::NativeRunner,
    repo::{Repository, default_root},
};

#[derive(Parser)]
struct Arguments {
    #[arg(long, default_value_os_t = default_root())]
    root: PathBuf,
}

fn main() {
    let args = Arguments::parse();
    if let Err(error) = build_native(
        &Repository::new(args.root),
        &BuildHost::default(),
        &mut NativeRunner,
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
