//! Native node-pty helper: acquire stdin's terminal, select cwd, and exec the requested program.

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    use std::{fs::OpenOptions, os::unix::process::CommandExt as _, process::Command};

    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [cwd, program, rest @ ..] = arguments.as_slice() else {
        return ExitCode::FAILURE;
    };
    if let Ok(terminal) = nix::unistd::ttyname(std::io::stdin()) {
        // The caller creates the session; opening without O_NOCTTY acquires its controlling tty.
        let _ = OpenOptions::new().read(true).write(true).open(terminal);
    }
    if !cwd.is_empty() && std::env::set_current_dir(cwd).is_err() {
        return ExitCode::FAILURE;
    }
    let _ = Command::new(program).args(rest).exec();
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    ExitCode::FAILURE
}
