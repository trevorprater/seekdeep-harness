//! Bounded capture, signal handling, and child reaping against Node's synchronous process API.

#![cfg(unix)]

mod release_support;

use std::{collections::BTreeMap, fs, process::Command};

use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
use seekdeep_landlock_tools::process::{CommandSpec, NativeRunner, Runner, run_checked};
use serde_json::Value;

#[test]
fn captured_process_limit_counts_raw_bytes_and_reaps_before_returning() {
    let source = fs::read_to_string(
        release_support::source_root().join("scripts/verify-packed-install.mjs"),
    )
    .unwrap();
    assert!(source.contains("maxBuffer: 64 * 1024 * 1024"));
    for (mode, limit, overflow) in [
        ("exact", 1_048_576, false),
        ("invalid-utf8", 1_048_576, false),
        ("nonzero", 1_048_576, false),
        ("stdout", 1_048_576, true),
        ("stderr", 1_048_576, true),
        ("mixed", 1_048_576, true),
        ("handled", 1_048_576, true),
        ("large", 64 * 1024 * 1024, false),
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let emitter = fixture.path().join("emitter.mjs");
        fs::write(&emitter, EMITTER).unwrap();
        let source_dir = fixture.path().join("source");
        let target_dir = fixture.path().join("target");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&target_dir).unwrap();
        let oracle = Command::new("node")
            .args(["--input-type=module", "-e", ORACLE])
            .arg(&emitter)
            .arg(mode)
            .arg(&source_dir)
            .arg(limit.to_string())
            .output()
            .unwrap();
        release_support::assert_source_success(&oracle);
        let expected: Value = serde_json::from_slice(&oracle.stdout).unwrap();
        let spec = CommandSpec {
            program: "node".to_owned(),
            args: vec![
                emitter.to_string_lossy().into_owned(),
                mode.to_owned(),
                target_dir.to_string_lossy().into_owned(),
            ],
            cwd: fixture.path().to_owned(),
            env: BTreeMap::new(),
            capture: true,
            max_buffer: limit,
            inherit_stdin: false,
        };
        let actual = NativeRunner.run(&spec).unwrap();
        assert_eq!(
            serde_json::to_value(actual.status).unwrap(),
            expected["status"],
            "{mode}"
        );
        assert_eq!(actual.spawn_error.is_some(), overflow, "{mode}");
        assert_eq!(
            expected["overflow"], overflow,
            "Node's maxBuffer behavior changed for {mode}"
        );
        assert_eq!(target_dir.join("marker").exists(), !overflow, "{mode}");
        assert_eq!(expected["marker"], !overflow, "{mode}");
        if overflow {
            assert_eq!(
                actual.spawn_error.as_deref(),
                Some("spawnSync node ENOBUFS")
            );
            assert!(actual.stdout.len() + actual.stderr.len() > limit, "{mode}");
            assert!(
                actual.stdout.len() + actual.stderr.len() <= limit + 65_536,
                "{mode}"
            );
        } else {
            assert_eq!(
                serde_json::json!(actual.stdout.len()),
                expected["stdoutBytes"]
            );
            assert_eq!(
                serde_json::json!(actual.stderr.len()),
                expected["stderrBytes"]
            );
        }
        for directory in [&source_dir, &target_dir] {
            let pid = fs::read_to_string(directory.join("pid"))
                .unwrap()
                .parse::<i32>()
                .unwrap();
            assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH), "{mode}");
        }
        if mode == "handled" {
            assert!(
                run_checked(&mut NativeRunner, &spec)
                    .unwrap_err()
                    .to_string()
                    .contains("ENOBUFS")
            );
        }
    }
}

const ORACLE: &str = r"
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
const [script, mode, directory, limit] = process.argv.slice(1);
const options = {encoding:'utf8'};
if (Number(limit) !== 1048576) options.maxBuffer = Number(limit);
const result = spawnSync(process.execPath, [script, mode, directory], options);
process.stdout.write(JSON.stringify({
  status:result.status,
  overflow:result.error?.code === 'ENOBUFS',
  stdoutBytes:Buffer.byteLength(result.stdout),
  stderrBytes:Buffer.byteLength(result.stderr),
  marker:fs.existsSync(path.join(directory,'marker')),
}));
";

#[test]
fn process_creation_errors_preserve_source_errno_for_capture_and_inherited_stdio() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let denied = directory.path().join("denied");
    fs::write(&denied, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o644)).unwrap();
    for (executable, code) in [
        (directory.path().join("missing"), "ENOENT"),
        (denied, "EACCES"),
    ] {
        for capture in [true, false] {
            let expected = Command::new("node")
                .args(["--input-type=module", "-e", SPAWN_ORACLE])
                .arg(&executable)
                .arg(if capture { "capture" } else { "inherit" })
                .output()
                .unwrap();
            release_support::assert_source_success(&expected);
            let expected: Value = serde_json::from_slice(&expected.stdout).unwrap();
            let spec = CommandSpec {
                program: executable.to_string_lossy().into_owned(),
                args: Vec::new(),
                cwd: directory.path().to_owned(),
                env: BTreeMap::new(),
                capture,
                max_buffer: 1_048_576,
                inherit_stdin: !capture,
            };
            let actual = NativeRunner.run(&spec).unwrap();
            assert!(actual.unstarted);
            assert_eq!(actual.status, None);
            assert_eq!(actual.spawn_error.as_deref(), expected["error"].as_str());
            assert_eq!(expected["code"], code);
            assert!(actual.stdout.is_empty() && actual.stderr.is_empty());
            assert_eq!(
                run_checked(&mut NativeRunner, &spec)
                    .unwrap_err()
                    .to_string(),
                expected["error"].as_str().unwrap()
            );
        }
    }
}

const SPAWN_ORACLE: &str = r"
import { spawnSync } from 'node:child_process';
const [program,mode] = process.argv.slice(1);
const options = mode === 'capture' ? {encoding:'utf8'} : {stdio:'inherit'};
const result = spawnSync(program, [], options);
process.stdout.write(JSON.stringify({error:result.error.message,code:result.error.code}));
";

const EMITTER: &str = r"
import fs from 'node:fs';
import path from 'node:path';
const [mode,directory] = process.argv.slice(2);
fs.writeFileSync(path.join(directory,'pid'), String(process.pid));
if (mode === 'handled') process.on('SIGTERM', () => process.exit(0));
function write(fd, bytes) {
  let offset = 0;
  while (offset < bytes.length) offset += fs.writeSync(fd, bytes, offset, bytes.length-offset);
}
if (mode === 'mixed') {
  write(1, Buffer.alloc(600000, 120));
  write(2, Buffer.alloc(600000, 121));
} else if (mode === 'invalid-utf8') {
  write(1, Buffer.alloc(524288, 255));
} else {
  const size = mode === 'large' ? 2097152 : mode === 'exact' ? 1048576 : mode === 'nonzero' ? 7 : 1048577;
  write(mode === 'stderr' ? 2 : 1, Buffer.alloc(size, 120));
}
setTimeout(() => {
  fs.writeFileSync(path.join(directory,'marker'), 'after output');
  process.exit(mode === 'nonzero' ? 23 : 0);
}, 200);
";
