use seekdeep_repository_tools::{
    npm_baseline::{BaselineRunner, SystemBaselineRunner},
    release_process::ReleaseRunOptions,
};
use serde_json::Value;

use super::support::source_call;

#[test]
fn capture_matches_source_raw_byte_cap_before_decoding_and_nonzero_exit_status() {
    for script in [
        "process.stdout.write('stdout'); process.stderr.write('stderr'); process.exitCode=7;",
        "require('node:fs').writeSync(1,Buffer.alloc(16*1024*1024,120));",
        "require('node:fs').writeSync(1,Buffer.alloc(8*1024*1024,255));",
    ] {
        let root = tempfile::tempdir().unwrap();
        let arguments = vec!["-e".to_owned(), script.to_owned()];
        let source = source_call(&serde_json::json!({
            "op":"capture", "command":"node", "arguments":arguments, "root":root.path(),
        }));
        let output = SystemBaselineRunner
            .result(
                "node",
                &arguments,
                &ReleaseRunOptions {
                    cwd: Some(root.path().to_owned()),
                    env: None,
                },
            )
            .unwrap();
        assert_eq!(
            source["value"],
            serde_json::json!({
                "status":output.status,
                "stdoutLength":output.stdout.encode_utf16().count(),
                "stderrLength":output.stderr.encode_utf16().count(),
                "stdoutPrefix":output.stdout.chars().take(24).collect::<String>(),
                "stderrPrefix":output.stderr.chars().take(24).collect::<String>(),
            })
        );
    }
}

#[cfg(unix)]
#[test]
fn capture_terminates_and_reaps_overflowing_children_before_later_side_effects_like_source() {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    for writes in [
        "fs.writeSync(1,Buffer.alloc(17*1024*1024,120));",
        "fs.writeSync(2,Buffer.alloc(17*1024*1024,120));",
        "fs.writeSync(1,Buffer.alloc(9*1024*1024,120)); fs.writeSync(2,Buffer.alloc(9*1024*1024,120));",
    ] {
        let root = tempfile::tempdir().unwrap();
        let script = format!(
            "const fs=require('node:fs'); fs.writeFileSync('pid',String(process.pid)); {writes} fs.writeFileSync('past-cap','must not run');"
        );
        let arguments = vec!["-e".to_owned(), script];
        let source = source_call(&serde_json::json!({
            "op":"capture", "command":"node", "arguments":arguments, "root":root.path(),
        }));
        assert_eq!(source["error"], "spawnSync node ENOBUFS");
        assert!(!root.path().join("past-cap").exists());
        let error = SystemBaselineRunner
            .result(
                "node",
                &arguments,
                &ReleaseRunOptions {
                    cwd: Some(root.path().to_owned()),
                    env: None,
                },
            )
            .unwrap_err();
        assert_eq!(source["error"], Value::String(error.to_string()));
        assert!(!root.path().join("past-cap").exists());
        let pid = std::fs::read_to_string(root.path().join("pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
    }
}

#[cfg(unix)]
#[test]
fn overflow_uses_sigterm_and_keeps_the_capture_error_after_a_child_handler_exits() {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    let root = tempfile::tempdir().unwrap();
    let script = r"
const fs = require('node:fs');
fs.writeFileSync('pid', String(process.pid));
process.on('SIGTERM', () => { fs.writeFileSync('termination', 'SIGTERM'); process.exit(7); });
process.stdout.on('error', () => {});
setTimeout(() => process.stdout.write('x'.repeat(16*1024*1024+1)), 10);
setInterval(() => {}, 1000);
";
    let arguments = vec!["-e".to_owned(), script.to_owned()];
    let source = source_call(&serde_json::json!({
        "op":"capture", "command":"node", "arguments":arguments, "root":root.path(),
    }));
    assert_eq!(source["error"], "spawnSync node ENOBUFS");
    assert_eq!(
        std::fs::read_to_string(root.path().join("termination")).unwrap(),
        "SIGTERM"
    );
    std::fs::remove_file(root.path().join("termination")).unwrap();
    let error = SystemBaselineRunner
        .result(
            "node",
            &arguments,
            &ReleaseRunOptions {
                cwd: Some(root.path().to_owned()),
                env: None,
            },
        )
        .unwrap_err();
    assert_eq!(source["error"], error.to_string());
    assert_eq!(
        std::fs::read_to_string(root.path().join("termination")).unwrap(),
        "SIGTERM"
    );
    let pid = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
}

#[cfg(unix)]
#[test]
fn captured_and_inherited_spawn_errors_match_source_enoent_and_eacces() {
    let root = tempfile::tempdir().unwrap();
    let nonexecutable = root.path().join("nonexecutable");
    std::fs::write(&nonexecutable, "not executable\n").unwrap();
    for command in [root.path().join("does-not-exist"), nonexecutable] {
        let command = command.to_str().unwrap();
        let options = ReleaseRunOptions {
            cwd: Some(root.path().to_owned()),
            env: None,
        };
        for operation in ["capture", "run"] {
            let source = source_call(&serde_json::json!({
                "op":operation,"command":command,"arguments":[],"root":root.path(),
            }));
            let error = if operation == "capture" {
                SystemBaselineRunner
                    .result(command, &[], &options)
                    .unwrap_err()
            } else {
                SystemBaselineRunner
                    .run(command, &[], &options)
                    .unwrap_err()
            };
            assert_eq!(source["error"], error.to_string());
        }
    }
}
