//! Runs repository gates outside Cargo's replaceable executable outputs.
//!
//! Keep the launcher free of production dependencies: workspace feature unification
//! must not relink the Windows executable that waits for the gate processor.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{self, Command, ExitStatus},
};

fn main() {
    let code = match run() {
        Ok(status) => status_code(status),
        Err(error) => {
            eprintln!("run-gates: {error}");
            1
        }
    };
    process::exit(code);
}

fn run() -> io::Result<ExitStatus> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let launcher = env::current_exe()?;
    let profile_directory = launcher
        .parent()
        .ok_or_else(|| io::Error::other("the launcher has no Cargo profile directory"))?;
    let profile = profile_directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("the launcher has no Cargo profile name"))?;
    let mut build = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    build.args([
        "build",
        "--quiet",
        "--package",
        "seekdeep-repository-tools",
        "--bin",
        "run-gates",
    ]);
    if profile != "debug" {
        build.args(["--profile", profile]);
    }
    let status = build.current_dir(&root).status()?;
    if !status.success() {
        return Ok(status);
    }

    let filename = format!("run-gates{}", env::consts::EXE_SUFFIX);
    let compiled = profile_directory.join(&filename);
    let directory = create_runner_directory()?;
    let staged = directory.join(filename);
    let execution = (|| {
        fs::copy(compiled, &staged)?;
        Command::new(&staged)
            .args(env::args_os().skip(1))
            .current_dir(&root)
            .status()
    })();
    if let Err(error) = fs::remove_dir_all(&directory) {
        if execution.as_ref().is_ok_and(ExitStatus::success) {
            return Err(error);
        }
        eprintln!(
            "run-gates: could not remove temporary runner {}: {error}",
            directory.display()
        );
    }
    execution
}

fn create_runner_directory() -> io::Result<PathBuf> {
    for counter in 0..1024 {
        let directory = env::temp_dir().join(format!(
            "seekdeep-repository-gates-{}-{counter}",
            process::id()
        ));
        match create_private_directory(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary repository-gate directory",
    ))
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    fs::create_dir(path)
}

fn status_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
    }
    #[cfg(not(unix))]
    status.code().unwrap_or(1)
}
