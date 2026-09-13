use std::{os::unix::fs::PermissionsExt as _, path::Path, process::Command};

use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
use serde_json::Value;

use super::support::{git, initialize_git, workspace, write_json};

#[test]
fn cli_release_connects_real_local_npm_pack_install_pty_and_fake_publication() {
    let root = workspace();
    let manifest_path = root.path().join("apps/cli/package.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest
        .as_object_mut()
        .unwrap()
        .shift_remove("optionalDependencies");
    write_json(&manifest_path, &manifest);
    initialize_git(root.path());
    std::fs::write(root.path().join("unrelated-dirty"), "preserve caller\n").unwrap();
    let before = git(root.path(), &["status", "--porcelain=v1"]);
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let artifacts = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let original_path = std::env::var_os("PATH").unwrap();
    let real_npm = std::env::split_paths(&original_path)
        .map(|directory| directory.join("npm"))
        .find(|candidate| candidate.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let state_path = tools.path().join("state.json");
    let events_path = tools.path().join("events.jsonl");
    write_json(&state_path, &serde_json::json!({"packages":{},"calls":[]}));
    for name in ["pnpm", "npm"] {
        let script = tools.path().join(name);
        std::fs::write(&script, PROCESS_FIXTURE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(
        std::iter::once(tools.path().to_owned()).chain(std::env::split_paths(&original_path)),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_publish-npm-baseline"))
        .args(["release", "--yes", "--registry", "http://127.0.0.1:9"])
        .arg("--output-dir")
        .arg(artifacts.path())
        .current_dir(root.path())
        .env("PATH", path)
        .env("SEEKDEEP_TEST_REAL_NPM", real_npm)
        .env("SEEKDEEP_TEST_NPM_STATE", &state_path)
        .env("SEEKDEEP_TEST_PROBE_EVENTS", &events_path)
        .env("npm_config_cache", tools.path().join("npm-cache"))
        .env("npm_config_offline", "true")
        .env("npm_config_user_agent", "pnpm-fixture")
        .env("NPM_CONFIG_USER_AGENT", "pnpm-fixture-upper")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for marker in [
        "planned pack",
        "installing 3 local tarballs",
        "installed seekdeep entry and Web startup probes passed",
        "packed 3 packages",
        "verified @seekdeep-ai/seekdeep@latest",
    ] {
        assert!(stdout.contains(marker), "{marker}: {stdout}");
    }
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    let bundle_path = Path::new(state["manifest"].as_str().unwrap());
    let bundle: Value =
        serde_json::from_str(&std::fs::read_to_string(bundle_path).unwrap()).unwrap();
    assert_eq!(bundle["commit"], head);
    assert_eq!(bundle["packages"].as_array().unwrap().len(), 3);
    assert_command_boundaries(&state);
    for package in bundle["packages"].as_array().unwrap() {
        let remote = &state["packages"][package["name"].as_str().unwrap()];
        assert_eq!(remote["integrity"], package["integrity"]);
        assert_eq!(remote["tags"]["dev-1.2.3"], bundle["version"]);
    }
    assert_eq!(
        state["packages"]["@seekdeep-ai/seekdeep"]["tags"]["latest"],
        bundle["version"]
    );
    let pid = assert_installed_lifecycle(&events_path);
    assert_eq!(git(root.path(), &["status", "--porcelain=v1"]), before);
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(root.path(), &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    eprintln!(
        "release fixture: version={}, packages=3, real npm pack/install=passed, PTY pid={pid} ready/stopped/reaped, fake publications=3, latest=verified, caller=preserved, worktree/consumer=removed",
        bundle["version"]
    );
}

fn assert_command_boundaries(state: &Value) {
    let calls = state["calls"].as_array().unwrap();
    let pnpm_calls = calls
        .iter()
        .filter(|call| call["command"] == "pnpm")
        .collect::<Vec<_>>();
    assert_eq!(pnpm_calls.len(), 6);
    assert_eq!(
        pnpm_calls[..5]
            .iter()
            .map(|call| call["arguments"].clone())
            .collect::<Vec<_>>(),
        serde_json::json!([
            ["install", "--frozen-lockfile"],
            ["run", "constraints"],
            ["run", "build"],
            ["run", "publint"],
            ["run", "verify-built-package-invariants"]
        ])
        .as_array()
        .unwrap()
        .clone()
    );
    let worktree = Path::new(pnpm_calls[0]["cwd"].as_str().unwrap());
    assert!(
        pnpm_calls
            .iter()
            .all(|call| call["cwd"] == pnpm_calls[0]["cwd"])
    );
    assert!(!worktree.exists());
    let npm_calls = calls
        .iter()
        .filter(|call| call["command"] == "npm")
        .collect::<Vec<_>>();
    assert_eq!(npm_calls[0]["arguments"][0], "install");
    let consumer = Path::new(npm_calls[0]["cwd"].as_str().unwrap());
    assert!(!consumer.exists());
    assert_eq!(
        npm_calls
            .iter()
            .filter(|call| call["arguments"][0] == "publish")
            .count(),
        3
    );
    assert!(npm_calls.iter().all(|call| {
        call["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|argument| argument == "--registry=http://127.0.0.1:9")
    }));
}

fn assert_installed_lifecycle(events_path: &Path) -> i32 {
    let events = std::fs::read_to_string(events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .map(|event| event["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["version", "ready", "stopped"]
    );
    assert_eq!(events[1]["pid"], events[2]["pid"]);
    assert_eq!(events[1]["stdinTTY"], true);
    assert_eq!(events[1]["stdoutTTY"], true);
    let pid = i32::try_from(events[1]["pid"].as_i64().unwrap()).unwrap();
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
    pid
}

const PROCESS_FIXTURE: &str = r"#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const statePath = process.env.SEEKDEEP_TEST_NPM_STATE;
const state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
const command = path.basename(process.argv[1]);
const args = process.argv.slice(2);
state.calls.push({command,arguments:args,cwd:process.cwd()});
const save = () => fs.writeFileSync(statePath, JSON.stringify(state));
const realNpm = args => {
  const result = spawnSync(process.env.SEEKDEEP_TEST_REAL_NPM, args, {stdio:'inherit'});
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
};
if (command === 'pnpm') {
  if (args[0] === '--filter') {
    const destination = args.at(-1);
    state.manifest = path.join(destination, 'manifest.json');
    for (const directory of ['apps/cli', 'packages/core/base', 'vendor/upstream']) {
      const cwd = process.cwd();
      process.chdir(directory);
      realNpm(['pack', '--ignore-scripts', '--offline', '--json', '--pack-destination', destination]);
      process.chdir(cwd);
    }
  } else if (JSON.stringify(args) !== JSON.stringify(['install','--frozen-lockfile']) &&
             !(args[0] === 'run' && ['constraints','build','publint','verify-built-package-invariants'].includes(args[1]))) {
    throw Error('fixture refuses unexpected pnpm operation');
  }
  save();
} else {
  if (process.env.npm_config_user_agent || process.env.NPM_CONFIG_USER_AGENT) throw Error('npm environment leaked');
  if (!args.includes('--registry=http://127.0.0.1:9')) throw Error('fixture registry escaped');
  if (args[0] === 'install') {
    const consumer = JSON.parse(fs.readFileSync('package.json','utf8'));
    if (!Object.values(consumer.dependencies).every(value => value.startsWith('file:'))) throw Error('non-local fixture install');
    save();
    realNpm(args);
  } else {
    const manifest = JSON.parse(fs.readFileSync(state.manifest,'utf8'));
    const name = value => value.slice(0,value.lastIndexOf('@'));
    let output = '';
    let status = 0;
    if (args[0] === 'ping') output = 'pong';
    else if (args[0] === 'whoami') output = 'fixture-account';
    else if (args[0] === 'view') {
      const pkg = state.packages[name(args[1])];
      if (pkg) output = JSON.stringify(pkg.integrity);
      else { process.stderr.write('E404'); status = 1; }
    } else if (args[0] === 'publish') {
      const pkg = manifest.packages.find(pkg => path.join(path.dirname(state.manifest),pkg.tarball) === args[1]);
      if (!pkg) throw Error('unexpected tarball');
      state.packages[pkg.name] = {integrity:pkg.integrity,tags:{[manifest.distTag]:manifest.version}};
    } else if (args[0] === 'dist-tag' && args[1] === 'ls') {
      output = Object.entries(state.packages[args[2]]?.tags??{}).map(([tag,version]) => `${tag}: ${version}`).join('\n');
    } else if (args[0] === 'dist-tag' && args[1] === 'add') {
      state.packages[name(args[2])].tags[args[3]] = manifest.version;
    } else throw Error('fixture refuses unexpected npm operation');
    save();
    process.stdout.write(output);
    process.exitCode = status;
  }
}
";
