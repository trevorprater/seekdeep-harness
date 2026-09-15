//! Executable resolution shared by the real-Windows integration suites.

use std::{process::Command, sync::OnceLock};

mod pwsh;

pub use pwsh::pwsh_path;

pub fn node_path() -> Option<&'static str> {
    static RESOLVED: OnceLock<Option<String>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let output = Command::new("where.exe").arg("node.exe").output().ok()?;
            if !output.status.success() {
                return None;
            }
            let executable = String::from_utf8(output.stdout)
                .ok()?
                .lines()
                .next()?
                .trim()
                .to_owned();
            Command::new(&executable)
                .args(["-e", "process.exit(0)"])
                .output()
                .is_ok_and(|result| result.status.success())
                .then_some(executable)
        })
        .as_deref()
}

pub fn prerequisites_available() -> bool {
    let available = pwsh_path().is_some() && node_path().is_some();
    if !available {
        eprintln!(
            "skipping real Windows process probes because PowerShell 7 or Node is unavailable"
        );
    }
    available
}
