//! Source-differential npm identity, retry, integrity, and publication-order contracts.

mod release_support;

use std::{collections::VecDeque, fs, path::Path, time::Duration};

use release_support::{Fixture, RecordingRunner, assert_source_success, output};
use seekdeep_landlock_tools::{
    process::CommandOutput,
    release::{integrity_of, publish_release},
};
use serde_json::json;

fn prepare(fixture: &Fixture, names: &[&str]) -> std::path::PathBuf {
    let directory = fixture.temporary.path().join("packed");
    fs::create_dir_all(&directory).unwrap();
    for name in names {
        fs::write(directory.join(name), b"identical packed archive bytes\n").unwrap();
    }
    fs::write(
        directory.join("publish-order.txt"),
        format!("{}\n", names.join("\n")),
    )
    .unwrap();
    directory
}

fn identity(name: &str, version: &str) -> CommandOutput {
    output(0, &json!({"name":name,"version":version}).to_string(), "")
}

fn absent() -> CommandOutput {
    output(1, "", "npm ERR! code E404")
}

fn present(tarball: &Path) -> CommandOutput {
    output(
        0,
        &serde_json::to_string(&integrity_of(tarball).unwrap()).unwrap(),
        "",
    )
}

fn differential(
    fixture: &Fixture,
    directory: &Path,
    results: Vec<CommandOutput>,
    success: bool,
) -> RecordingRunner {
    let plan = json!({"responses":results.iter().map(|result| json!({"status":result.status,"stdout":result.stdout,"stderr":result.stderr})).collect::<Vec<_>>()});
    let mut responses = VecDeque::from(results);
    let mut runner = RecordingRunner::new(move |spec| {
        Ok(responses
            .pop_front()
            .unwrap_or_else(|| panic!("unplanned Rust call: {spec:?}")))
    });
    let actual = publish_release(directory, fixture.root(), &mut runner);
    let source = fixture.source(
        "publish-release.mjs",
        &[directory.to_string_lossy().into_owned()],
        &[],
        Some(&plan),
    );
    assert_eq!(actual.is_ok(), success, "Rust result: {actual:?}");
    assert_eq!(
        source.status.success(),
        success,
        "source: {}",
        String::from_utf8_lossy(&source.stderr)
    );
    if let Err(error) = actual {
        assert!(
            String::from_utf8_lossy(&source.stderr).contains(&error.to_string()),
            "Rust: {error}\nsource: {}",
            String::from_utf8_lossy(&source.stderr)
        );
    } else {
        assert_source_success(&source);
    }
    assert_eq!(
        String::from_utf8_lossy(&source.stdout),
        if runner.logs.is_empty() {
            String::new()
        } else {
            format!("{}\n", runner.logs.join("\n"))
        }
    );
    let calls = fixture.source_calls();
    let processes = calls
        .iter()
        .filter(|call| call.get("program").is_some())
        .collect::<Vec<_>>();
    assert_eq!(processes.len(), runner.calls.len());
    for (expected, actual) in processes.into_iter().zip(&runner.calls) {
        assert_eq!(expected["program"], actual.program);
        assert_eq!(expected["args"], json!(actual.args));
        assert_eq!(expected["cwd"], actual.cwd.to_string_lossy().as_ref());
    }
    let delays = calls
        .iter()
        .filter_map(|call| call["sleep"].as_u64())
        .map(Duration::from_millis)
        .collect::<Vec<_>>();
    assert_eq!(delays, runner.delays);
    runner
}

#[test]
fn integrity_skip_collision_and_ambiguous_success_match_the_source() {
    for case in 0..4 {
        let fixture = Fixture::new();
        let directory = prepare(&fixture, &["entry.tgz"]);
        let mut responses = vec![identity("@seekdeep-ai/entry", "1.2.3")];
        match case {
            0 => responses.extend([absent(), output(0, "published", "")]),
            1 => responses.push(present(&directory.join("entry.tgz"))),
            2 => responses.push(output(0, "\"sha512-different\"", "")),
            3 => responses.extend([
                absent(),
                output(1, "", "npm ERR! code E409"),
                present(&directory.join("entry.tgz")),
            ]),
            _ => unreachable!(),
        }
        let runner = differential(&fixture, &directory, responses, case != 2);
        assert!(runner.delays.is_empty());
        assert!(
            runner
                .calls
                .iter()
                .all(|call| !call.args.iter().any(|arg| arg == "--access"))
        );
    }
}

#[test]
fn every_transient_code_retries_only_after_a_registry_recheck() {
    for code in [
        "E409",
        "E429",
        "E500",
        "E502",
        "E503",
        "E504",
        "ETIMEDOUT",
        "ECONNRESET",
        "EAI_AGAIN",
    ] {
        let fixture = Fixture::new();
        let directory = prepare(&fixture, &["entry.tgz"]);
        let runner = differential(
            &fixture,
            &directory,
            vec![
                identity("entry", "1.0.0-rc.2"),
                absent(),
                output(1, "", &format!("npm ERR! code {code}")),
                absent(),
                output(0, "", ""),
            ],
            true,
        );
        assert_eq!(runner.delays, [Duration::from_millis(2000)]);
        let publishes = runner
            .calls
            .iter()
            .filter(|call| call.args[0] == "publish")
            .collect::<Vec<_>>();
        assert_eq!(publishes.len(), 2);
        assert!(publishes.iter().all(|call| {
            call.args
                .ends_with(&["--tag".to_owned(), "next".to_owned()])
        }));
    }
}

#[test]
fn attempts_backoff_and_permanent_failure_boundaries_match_source() {
    let fixture = Fixture::new();
    let directory = prepare(&fixture, &["entry.tgz"]);
    let mut responses = vec![identity("entry", "1.2.3"), absent()];
    for _ in 0..4 {
        responses.extend([output(1, "", "npm ERR! code E503"), absent()]);
    }
    let runner = differential(&fixture, &directory, responses, false);
    assert_eq!(
        runner.delays,
        [
            Duration::from_millis(2000),
            Duration::from_millis(4000),
            Duration::from_millis(8000)
        ]
    );
    assert_eq!(
        runner
            .calls
            .iter()
            .filter(|call| call.args[0] == "publish")
            .count(),
        4
    );

    for diagnostic in [
        "npm ERR! code E403",
        "E409 without the npm code prefix",
        "invalid manifest",
    ] {
        let fixture = Fixture::new();
        let directory = prepare(&fixture, &["entry.tgz"]);
        let runner = differential(
            &fixture,
            &directory,
            vec![
                identity("entry", "1.2.3"),
                absent(),
                output(1, "", diagnostic),
                absent(),
            ],
            false,
        );
        assert!(runner.delays.is_empty());
        assert_eq!(
            runner
                .calls
                .iter()
                .filter(|call| call.args[0] == "publish")
                .count(),
            1
        );
    }
}

#[test]
fn registry_manifest_and_archive_errors_fail_before_any_write() {
    for responses in [
        vec![output(2, "", "not an archive")],
        vec![output(0, "{\"name\":1,\"version\":\"1.0.0\"}", "")],
        vec![
            identity("entry", "1.0.0"),
            output(1, "network ", "npm ERR! code E500"),
        ],
        vec![identity("entry", "1.0.0"), output(0, "\"\"", "")],
        vec![identity("entry", "1.0.0"), output(0, "null", "")],
        vec![identity("entry", "1.0.0"), output(0, "{}", "")],
    ] {
        let fixture = Fixture::new();
        let directory = prepare(&fixture, &["entry.tgz"]);
        let runner = differential(&fixture, &directory, responses, false);
        assert!(!runner.calls.iter().any(|call| call.args[0] == "publish"));
    }
    let fixture = Fixture::new();
    let directory = prepare(&fixture, &["entry.tgz"]);
    differential(
        &fixture,
        &directory,
        vec![
            identity("entry", "1.0.0"),
            output(1, "404 Not Found", ""),
            output(0, "", ""),
        ],
        true,
    );
}

#[test]
fn publication_spacing_applies_between_writes_and_never_between_skips() {
    let fixture = Fixture::new();
    let directory = prepare(&fixture, &["first.tgz", "skip.tgz", "last.tgz"]);
    let runner = differential(
        &fixture,
        &directory,
        vec![
            identity("first", "1.0.0"),
            absent(),
            output(0, "", ""),
            identity("skip", "1.0.0"),
            present(&directory.join("skip.tgz")),
            identity("last", "1.0.0"),
            absent(),
            output(0, "", ""),
        ],
        true,
    );
    assert_eq!(runner.delays, [Duration::from_millis(2000)]);
    assert_eq!(
        runner.logs.last().unwrap(),
        "landlock publish: 2 published, 1 already present"
    );

    let fixture = Fixture::new();
    let directory = prepare(&fixture, &["first.tgz", "last.tgz"]);
    let runner = differential(
        &fixture,
        &directory,
        vec![
            identity("first", "1.0.0"),
            present(&directory.join("first.tgz")),
            identity("last", "1.0.0"),
            present(&directory.join("last.tgz")),
        ],
        true,
    );
    assert!(runner.delays.is_empty());
    assert_eq!(
        runner.logs.last().unwrap(),
        "landlock publish: 0 published, 2 already present"
    );
}

#[test]
fn integrity_digest_matches_node_crypto_for_the_actual_archive_bytes() {
    let fixture = Fixture::new();
    let directory = prepare(&fixture, &["entry.tgz"]);
    let tarball = directory.join("entry.tgz");
    let result=std::process::Command::new("node").args(["-e","const fs=require('node:fs');const c=require('node:crypto');process.stdout.write('sha512-'+c.createHash('sha512').update(fs.readFileSync(process.argv[1])).digest('base64')); "]).arg(&tarball).output().unwrap();
    assert_source_success(&result);
    assert_eq!(
        integrity_of(&tarball).unwrap(),
        String::from_utf8(result.stdout).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn publication_cli_rechecks_persistent_fake_registry_and_never_republishes_identical_bytes() {
    use std::{os::unix::fs::PermissionsExt as _, process::Command};

    let fixture = Fixture::new();
    let directory = prepare(&fixture, &["entry.tgz"]);
    let payload = fixture.temporary.path().join("payload/package");
    fs::create_dir_all(&payload).unwrap();
    release_support::write_json(
        &payload.join("package.json"),
        &json!({"name":"@fixture/landlock-entry","version":"1.2.3-rc.1"}),
    );
    let packed = Command::new("tar")
        .args(["-czf"])
        .arg(directory.join("entry.tgz"))
        .arg("-C")
        .arg(payload.parent().unwrap())
        .arg("package")
        .output()
        .unwrap();
    assert_source_success(&packed);

    let executables = fixture.temporary.path().join("executables");
    fs::create_dir(&executables).unwrap();
    let fake_npm = executables.join("npm");
    fs::write(&fake_npm, FAKE_NPM).unwrap();
    fs::set_permissions(&fake_npm, fs::Permissions::from_mode(0o755)).unwrap();
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(executables.clone()).chain(std::env::split_paths(&inherited)),
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_landlock-publish-release"))
            .arg("--root")
            .arg(fixture.root())
            .arg(&directory)
            .env("PATH", &path)
            .current_dir(fixture.temporary.path())
            .output()
            .unwrap()
    };
    let first = run();
    assert_source_success(&first);
    assert!(
        String::from_utf8(first.stdout)
            .unwrap()
            .contains("landlock publish: 1 published, 0 already present")
    );
    let second = run();
    assert_source_success(&second);
    assert!(
        String::from_utf8(second.stdout)
            .unwrap()
            .contains("landlock publish: 0 published, 1 already present")
    );
    fs::write(executables.join("integrity"), "sha512-collision").unwrap();
    let collision = run();
    assert_eq!(collision.status.code(), Some(1));
    assert!(
        String::from_utf8(collision.stderr)
            .unwrap()
            .contains("already published with different content")
    );
    let calls = fs::read_to_string(executables.join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls[1],
        json!(["publish", directory.join("entry.tgz"), "--tag", "next"])
    );
    for call in [&calls[0], &calls[2], &calls[3]] {
        assert_eq!(
            call,
            &json!([
                "view",
                "@fixture/landlock-entry@1.2.3-rc.1",
                "dist.integrity",
                "--json"
            ])
        );
    }
}

#[cfg(unix)]
const FAKE_NPM: &str = r"#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const args = process.argv.slice(2);
fs.appendFileSync(path.join(__dirname, 'calls.jsonl'), JSON.stringify(args) + '\n');
const state = path.join(__dirname, 'integrity');
if (args[0] === 'view') {
  if (!fs.existsSync(state)) { process.stderr.write('npm ERR! code E404\n'); process.exit(1); }
  process.stdout.write(JSON.stringify(fs.readFileSync(state, 'utf8')));
} else if (args[0] === 'publish') {
  if (fs.existsSync(state)) throw Error('duplicate publication');
  if (args[2] !== '--tag' || args[3] !== 'next') throw Error('missing prerelease tag');
  fs.writeFileSync(state, 'sha512-' + crypto.createHash('sha512').update(fs.readFileSync(args[1])).digest('base64'));
} else {
  throw Error('unplanned npm operation: ' + JSON.stringify(args));
}
";
