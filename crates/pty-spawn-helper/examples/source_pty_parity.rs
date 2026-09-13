//! Exercise the Rust helper through the pinned source's patched node-pty consumer.

use std::{path::Path, process::Command};

const ORACLE: &str = r"
const { createRequire } = require('node:module');
const path = require('node:path');
const sourceRequire = createRequire(path.join(process.argv[1], 'packages/subprocess/subprocess-local/package.json'));
const pty = sourceRequire('node-pty');
const terminal = pty.spawn('/bin/sh', ['-c', 'exec 3<>/dev/tty || exit 7; printf HELPER_CTTY_OK >&3; pwd'], {
  cwd:process.argv[2], env:{...process.env,TERM:'xterm-256color'}, cols:80, rows:24,
});
let output = '';
const timeout = setTimeout(() => { terminal.kill(); process.stderr.write('helper PTY timed out'); process.exitCode = 1; }, 5000);
terminal.onData(data => { output += data; });
terminal.onExit(event => {
  clearTimeout(timeout);
  process.stdout.write(JSON.stringify({output,exitCode:event.exitCode}));
});
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Err("the source node-pty consumer uses its spawn helper only on macOS".into());
    }
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 {
        return Err("usage: source_pty_parity <pinned-source> <rust-helper>".into());
    }
    let source = Path::new(&arguments[0]);
    let head = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("source pin missing")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pin {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    let temporary = tempfile::tempdir()?;
    let cwd = temporary.path().canonicalize()?;
    let output = Command::new("node")
        .args(["-e", ORACLE])
        .arg(source)
        .arg(&cwd)
        .env(
            "DSH_NODE_PTY_SPAWN_HELPER",
            Path::new(&arguments[1]).canonicalize()?,
        )
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if result["exitCode"] != 0
        || !result["output"].as_str().is_some_and(|text| {
            text.contains("HELPER_CTTY_OK") && text.contains(cwd.to_string_lossy().as_ref())
        })
    {
        return Err(format!("native helper failed the source PTY consumer: {result}").into());
    }
    println!("Rust helper preserves controlling TTY and cwd through the pinned node-pty consumer");
    Ok(())
}
