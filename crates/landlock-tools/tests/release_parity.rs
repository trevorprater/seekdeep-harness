//! Differential artifact, version, commit-order, and package-manager release contracts.

mod release_support;

use std::{fs, path::Path, process::Command};

use release_support::{
    Fixture, RecordingRunner, assert_source_success, normalize, output, source_root, write_json,
};
use seekdeep_landlock_tools::{
    process::exit_code,
    release::{
        PackOptions, ReleaseEnvironment, assemble_prebuilds, bump_release, commit_release,
        next_version, pack_release, tarball_name, verify_release,
    },
    repo::host_platform,
};
use serde_json::{Value, json};

#[test]
fn version_syntax_and_number_semantics_match_the_source() {
    let cases = [
        ("1.2.3", "major"),
        ("1.2.3", "minor"),
        ("1.2.3", "patch"),
        ("0.0.0", "patch"),
        ("9.8.7", "2.0.0-rc.4"),
        ("0.1.1", "01.002.0003"),
        ("0.1.1", "1.2.3-alpha-beta.0"),
        ("0.1.1", "1.2.3+build"),
        ("1.2.3-rc.1", "patch"),
        ("01.2.3", "minor"),
        ("1.2", "major"),
        ("1.2.3", ""),
        ("1.2.3", "prepatch"),
        ("1.2.3", "v1.2.3"),
        ("1.2.3", "1.2.3-"),
        ("1.2.3", "1.2.3-x..y"),
        ("1.2.3", "１.２.３"),
        ("9007199254740993.2.3", "major"),
        ("1000000000000000000000.2.3", "minor"),
    ];
    let script = r#"const fs = require('node:fs');
const source = fs.readFileSync(process.argv[1], 'utf8');
const excerpt = source.slice(source.indexOf('function parseVersion('), source.indexOf('function currentPublishedVersion('));
const run = new Function('cases', 'const releaseTypes = new Set(["major","minor","patch"]);' + excerpt + '; return cases.map(([current,release]) => { try { return {ok:nextVersion(current,release)}; } catch (error) { return {error:error.message}; } });');
process.stdout.write(JSON.stringify(run(JSON.parse(process.argv[2]))));
"#;
    let source = Command::new("node")
        .args(["-e", script])
        .arg(source_root().join("scripts/bump-release.mjs"))
        .arg(serde_json::to_string(&cases).unwrap())
        .output()
        .unwrap();
    assert_source_success(&source);
    let expected: Vec<Value> = serde_json::from_slice(&source.stdout).unwrap();
    for ((current, release), expected) in cases.into_iter().zip(expected) {
        let actual = match next_version(current, release) {
            Ok(version) => json!({"ok":version}),
            Err(error) => json!({"error":error.to_string()}),
        };
        assert_eq!(actual, expected, "{current} => {release}");
    }
}

#[test]
fn release_version_and_tag_failures_match_source() {
    for (github_ref, publish, divergent, prebuilds) in [
        ("", false, false, false),
        ("refs/heads/master", false, false, true),
        ("refs/tags/landlock-run-v0.1.1", true, false, true),
        ("refs/heads/master", true, false, false),
        ("refs/tags/v0.1.1", true, false, false),
        ("refs/tags/landlock-run-v0.1.2", false, false, false),
        ("refs/tags/landlock-run-v0.1.1", false, true, false),
    ] {
        let source = Fixture::new();
        let target = Fixture::new();
        if divergent {
            source.set_version("packages/entry", "0.1.2");
            target.set_version("packages/entry", "0.1.2");
        }
        let args = if prebuilds {
            vec!["--prebuilds".to_owned()]
        } else {
            vec![]
        };
        let source_output = source.source(
            "verify-release.mjs",
            &args,
            &[
                ("GITHUB_REF", github_ref),
                ("RELEASE_PUBLISH", if publish { "true" } else { "false" }),
            ],
            None,
        );
        let mut runner = RecordingRunner::success();
        let actual = verify_release(
            &target.repository,
            &ReleaseEnvironment {
                github_ref: github_ref.to_owned(),
                release_publish: publish,
            },
            prebuilds,
            &mut runner,
        );
        match actual {
            Ok(version) => {
                assert_eq!(version, "0.1.1");
                assert_source_success(&source_output);
                assert_eq!(
                    String::from_utf8_lossy(&source_output.stdout),
                    format!("{}\n", runner.logs.join("\n"))
                );
            }
            Err(error) => {
                assert!(!source_output.status.success());
                assert!(
                    String::from_utf8_lossy(&source_output.stderr).contains(&error.to_string()),
                    "source {}\nrust {error}",
                    String::from_utf8_lossy(&source_output.stderr)
                );
            }
        }
    }
}

#[test]
fn prebuild_assembly_replaces_old_payloads_and_matches_source_failures() {
    for case in 0..5 {
        let source = Fixture::new();
        let target = Fixture::new();
        let source_artifacts = source.artifacts();
        let target_artifacts = target.artifacts();
        for (fixture, artifacts) in [(&source, &source_artifacts), (&target, &target_artifacts)] {
            fs::write(
                fixture.root().join("packages/linux-x64/bin/stale"),
                "old payload",
            )
            .unwrap();
            fs::write(
                artifacts.join("README.txt"),
                "non-directory artifacts are ignored",
            )
            .unwrap();
            match case {
                0 => {}
                1 => {
                    fs::create_dir_all(artifacts.join("unknown")).unwrap();
                }
                2 => {
                    fs::remove_file(artifacts.join("prebuild-linux-arm64/landlock-run")).unwrap();
                }
                3 => {
                    fs::write(artifacts.join("prebuild-linux-x64/extra"), "undeclared").unwrap();
                }
                4 => {
                    fs::copy(
                        artifacts.join("prebuild-linux-x64/landlock-run"),
                        artifacts.join("prebuild-linux-arm64/landlock-run"),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
        }
        let source_output = source.source(
            "assemble-prebuilds.mjs",
            &[source_artifacts.to_string_lossy().into_owned()],
            &[],
            None,
        );
        let mut runner = RecordingRunner::success();
        let result = assemble_prebuilds(&target.repository, &target_artifacts, &mut runner);
        assert!(!target.root().join("packages/linux-x64/bin/stale").exists());
        match result {
            Ok(()) => {
                assert_source_success(&source_output);
                let expected = normalize(&String::from_utf8_lossy(&source_output.stdout), &source);
                let actual = normalize(&format!("{}\n", runner.logs.join("\n")), &target);
                assert_eq!(actual, expected);
                for cpu in ["arm64", "x64"] {
                    assert_eq!(
                        fs::read(
                            target
                                .root()
                                .join(format!("packages/linux-{cpu}/bin/landlock-run"))
                        )
                        .unwrap(),
                        fs::read(
                            source
                                .root()
                                .join(format!("packages/linux-{cpu}/bin/landlock-run"))
                        )
                        .unwrap()
                    );
                }
            }
            Err(error) => {
                assert!(!source_output.status.success(), "case {case}");
                assert!(
                    String::from_utf8_lossy(&source_output.stderr).contains(&error.to_string()),
                    "case {case}: {error}"
                );
            }
        }
    }
    let fixture = Fixture::new();
    let missing = fixture.temporary.path().join("missing");
    assert!(
        assemble_prebuilds(
            &fixture.repository,
            &missing,
            &mut RecordingRunner::success()
        )
        .unwrap_err()
        .to_string()
        .contains("prebuild artifact directory does not exist:")
    );
}

#[test]
fn bump_and_commit_match_manifest_bytes_child_order_and_failure_status() {
    for commit in [false, true] {
        let source = Fixture::new();
        let target = Fixture::new();
        let plan = json!({"responses":[{"status":0,"stdout":"","stderr":""},{"status":0,"stdout":"","stderr":""},{"status":0,"stdout":"","stderr":""}]});
        let script = if commit {
            "commit-release.mjs"
        } else {
            "bump-release.mjs"
        };
        let source_output = source.source(script, &["0.2.0-rc.3".into()], &[], Some(&plan));
        assert_source_success(&source_output);
        let mut runner = RecordingRunner::success();
        let version = if commit {
            commit_release(
                &target.repository,
                "0.2.0-rc.3",
                &ReleaseEnvironment::default(),
                &mut runner,
            )
        } else {
            bump_release(
                &target.repository,
                "0.2.0-rc.3",
                &ReleaseEnvironment::default(),
                &mut runner,
            )
        }
        .unwrap();
        assert_eq!(version, "0.2.0-rc.3");
        for directory in [
            "",
            "packages/entry",
            "packages/linux-arm64",
            "packages/linux-x64",
        ] {
            assert_eq!(
                fs::read(target.root().join(directory).join("package.json")).unwrap(),
                fs::read(source.root().join(directory).join("package.json")).unwrap()
            );
        }
        assert_eq!(
            String::from_utf8_lossy(&source_output.stdout),
            format!("{}\n", runner.logs.join("\n"))
        );
        let source_calls = source
            .source_calls()
            .into_iter()
            .filter(|call| call["program"] != "node")
            .collect::<Vec<_>>();
        assert_eq!(source_calls.len(), runner.calls.len());
        for (expected, actual) in source_calls.iter().zip(&runner.calls) {
            assert_eq!(expected["program"], actual.program);
            assert_eq!(expected["args"], json!(actual.args));
            assert_eq!(expected["ci"], actual.env["CI"]);
            assert_eq!(
                normalize(expected["cwd"].as_str().unwrap(), &source).trim_end_matches('/'),
                normalize(&actual.cwd.to_string_lossy(), &target).trim_end_matches('/')
            );
        }
    }
    let fixture = Fixture::new();
    let mut runner = RecordingRunner::new(|_| Ok(output(23, "", "lockfile failure")));
    let error = commit_release(
        &fixture.repository,
        "patch",
        &ReleaseEnvironment::default(),
        &mut runner,
    )
    .unwrap_err();
    assert_eq!(exit_code(&error), 23);
    assert_eq!(runner.calls.len(), 1);
    assert_eq!(fixture.manifest("packages/entry")["version"], "0.1.2");
    fixture.set_version("packages/entry", "0.1.3");
    let mut runner = RecordingRunner::success();
    assert!(
        bump_release(
            &fixture.repository,
            "patch",
            &ReleaseEnvironment::default(),
            &mut runner
        )
        .unwrap_err()
        .to_string()
        .starts_with("published package versions differ:")
    );
    assert!(runner.calls.is_empty());
}

#[test]
fn packing_matches_source_order_manager_split_and_current_host_filter() {
    for current_only in [false, true] {
        let source = Fixture::new();
        let target = Fixture::new();
        let source_destination = source.temporary.path().join("packed");
        let target_destination = target.temporary.path().join("packed");
        for destination in [&source_destination, &target_destination] {
            fs::create_dir_all(destination).unwrap();
            fs::write(destination.join("stale.tgz"), "stale").unwrap();
        }
        let mut args = vec![source_destination.to_string_lossy().into_owned()];
        if current_only {
            args.push("--current-platform-only".to_owned());
        }
        let source_output =
            source.source("pack-release.mjs", &args, &[], Some(&json!({"pack":true})));
        assert_source_success(&source_output);
        let mut runner = RecordingRunner::new(|spec| {
            let directory = &spec.args[1];
            let manifest: Value =
                serde_json::from_slice(&fs::read(spec.cwd.join(directory).join("package.json"))?)?;
            let destination = Path::new(spec.args.last().unwrap());
            fs::write(destination.join(tarball_name(&manifest)?), "packed")?;
            Ok(output(0, "", ""))
        });
        let order = pack_release(
            &target.repository,
            &PackOptions {
                destination: target_destination.clone(),
                current_platform_only: current_only,
                host_platform: host_platform(),
            },
            &mut runner,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(target_destination.join("publish-order.txt")).unwrap(),
            format!("{}\n", order.join("\n"))
        );
        assert_eq!(
            fs::read(source_destination.join("publish-order.txt")).unwrap(),
            fs::read(target_destination.join("publish-order.txt")).unwrap()
        );
        assert!(!target_destination.join("stale.tgz").exists());
        assert_eq!(
            normalize(&String::from_utf8_lossy(&source_output.stdout), &source),
            normalize(&format!("{}\n", runner.logs.join("\n")), &target)
        );
        for (expected, actual) in source.source_calls().iter().zip(&runner.calls) {
            assert_eq!(expected["program"], actual.program);
            assert_eq!(
                normalize(&expected["args"].to_string(), &source),
                normalize(&json!(actual.args).to_string(), &target)
            );
        }
    }
    let fixture = Fixture::new();
    let options = PackOptions {
        destination: fixture.temporary.path().join("packed"),
        current_platform_only: false,
        host_platform: "linux-x64".into(),
    };
    let mut runner = RecordingRunner::success();
    assert!(
        pack_release(&fixture.repository, &options, &mut runner)
            .unwrap_err()
            .to_string()
            .contains("expected pack output not found:")
    );
    assert_eq!(runner.calls.len(), 1);
    assert!(!options.destination.join("publish-order.txt").exists());
    write_json(
        &fixture.root().join("packages/linux-x64/package.json"),
        &json!({"name":"plain-package","version":"1.0.0"}),
    );
    assert_eq!(
        tarball_name(&fixture.manifest("packages/linux-x64")).unwrap(),
        "plain-package-1.0.0.tgz"
    );
}

#[test]
fn commit_release_operates_on_a_real_temporary_git_index_without_creating_tags() {
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        let result = Command::new("git")
            .args(args)
            .current_dir(fixture.temporary.path())
            .output()
            .unwrap();
        assert_source_success(&result);
        String::from_utf8(result.stdout).unwrap()
    };
    git(&["init", "--initial-branch=main"]);
    git(&["config", "user.name", "Landlock fixture"]);
    git(&["config", "user.email", "landlock-fixture@example.invalid"]);
    git(&["config", "core.hooksPath", "/dev/null"]);
    fs::write(fixture.temporary.path().join("unrelated.txt"), "initial\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "fixture initial state"]);
    fs::write(
        fixture.temporary.path().join("unrelated.txt"),
        "preserve this dirty work\n",
    )
    .unwrap();
    let mut runner = RecordingRunner::new(|spec| {
        if spec.program == "pnpm" {
            assert_eq!(
                spec.args,
                ["install", "--ignore-scripts", "--lockfile-only"]
            );
            fs::write(
                spec.cwd.join("pnpm-lock.yaml"),
                "lockfileVersion: '9.0'\n# fixture refreshed\n",
            )?;
            Ok(output(0, "", ""))
        } else {
            assert_eq!(spec.program, "git");
            seekdeep_landlock_tools::process::Runner::run(
                &mut seekdeep_landlock_tools::process::NativeRunner,
                spec,
            )
        }
    });
    assert_eq!(
        commit_release(
            &fixture.repository,
            "patch",
            &ReleaseEnvironment::default(),
            &mut runner
        )
        .unwrap(),
        "0.1.2"
    );
    assert_eq!(
        git(&["log", "-1", "--format=%s"]).trim(),
        "release(landlock-run): 0.1.2"
    );
    let changed = git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]);
    assert_eq!(changed.lines().count(), 5);
    assert!(!changed.contains("unrelated.txt"));
    assert_eq!(git(&["status", "--short"]).trim(), "M unrelated.txt");
    assert!(git(&["tag", "--list"]).trim().is_empty());
    assert!(git(&["remote"]).trim().is_empty());
}

#[test]
fn release_command_line_preserves_tag_validation_and_usage_exit_codes() {
    let fixture = Fixture::new();
    let binary = env!("CARGO_BIN_EXE_landlock-verify-release");
    let result = Command::new(binary)
        .arg("--root")
        .arg(fixture.root())
        .arg("--prebuilds")
        .env("GITHUB_REF", "refs/tags/landlock-run-v0.1.1")
        .env("RELEASE_PUBLISH", "true")
        .output()
        .unwrap();
    assert_source_success(&result);
    assert!(
        String::from_utf8_lossy(&result.stdout).starts_with("Verified release version 0.1.1\n")
    );
    let result = Command::new(binary)
        .arg("--root")
        .arg(fixture.root())
        .env("GITHUB_REF", "refs/heads/master")
        .env("RELEASE_PUBLISH", "true")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("publishing requires running the workflow from a landlock-run-v* tag")
    );
    for binary in [
        env!("CARGO_BIN_EXE_landlock-bump-release"),
        env!("CARGO_BIN_EXE_landlock-commit-release"),
    ] {
        let result = Command::new(binary)
            .arg("--root")
            .arg(fixture.root())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("Usage: pnpm release:"));
        assert_eq!(fixture.manifest("packages/entry")["version"], "0.1.1");
    }
}
