//! Print a native CI or release-prebuild matrix.

use std::{
    io::{self, Write as _},
    path::PathBuf,
};

use clap::Parser;
use seekdeep_landlock_tools::{
    matrix::{MatrixKind, github_matrix},
    repo::{Repository, default_root},
};

#[derive(Parser)]
struct Arguments {
    #[arg(long, default_value_os_t = default_root())]
    root: PathBuf,
    target: Option<String>,
}

fn main() {
    let args = Arguments::parse();
    let kind = match args.target.as_deref() {
        Some("ci") => MatrixKind::Ci,
        Some("release-prebuild") => MatrixKind::ReleasePrebuild,
        _ => {
            eprintln!("Usage: landlock-github-matrix <ci|release-prebuild>");
            std::process::exit(1);
        }
    };
    let result = github_matrix(&Repository::new(args.root), kind).and_then(|matrix| {
        io::stdout().write_all(serde_json::to_string(&matrix)?.as_bytes())?;
        Ok(())
    });
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
