use std::{collections::BTreeMap, path::Path};

use seekdeep_repository_tools::{
    npm_baseline::{
        BaselineRunner, InstalledWebProbe, RegistryPublication, normalize_registry,
        parse_dist_tag_listing,
    },
    release_process::{ReleaseCommandResult, ReleaseRunOptions},
};
use serde_json::Value;

use super::{
    bundle::bundle,
    support::{environment, source_call, success},
};

struct RegistryRunner {
    manifest: Value,
    directory: std::path::PathBuf,
    remote: Value,
    calls: Vec<Value>,
    cancelled: bool,
    confirmations: usize,
}

impl BaselineRunner for RegistryRunner {
    fn result(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<ReleaseCommandResult> {
        self.calls
            .push(serde_json::json!({"command":command,"args":arguments}));
        assert_eq!(command, "npm");
        assert_eq!(options.cwd.as_deref(), Some(std::env::temp_dir().as_path()));
        let environment = options.env.as_ref().unwrap();
        assert!(!environment.contains_key(std::ffi::OsStr::new("npm_config_user_agent")));
        assert!(!environment.contains_key(std::ffi::OsStr::new("NPM_CONFIG_USER_AGENT")));
        let tag = self.manifest["distTag"].as_str().unwrap();
        let version = self.manifest["version"].as_str().unwrap();
        match arguments[0].as_str() {
            "ping" => Ok(success("pong")),
            "whoami" => Ok(success("fixture-identity")),
            "view" => {
                let name = arguments[1].rsplit_once('@').unwrap().0;
                let state = &self.remote[name];
                if let Some(failure) = state.get("failure") {
                    return Ok(ReleaseCommandResult {
                        status: failure["status"]
                            .as_i64()
                            .and_then(|status| i32::try_from(status).ok()),
                        stdout: failure["stdout"].as_str().unwrap().to_owned(),
                        stderr: failure["stderr"].as_str().unwrap().to_owned(),
                    });
                }
                if state.get("integrity").is_some_and(|integrity| {
                    integrity.as_str().is_none_or(|value| !value.is_empty())
                }) {
                    Ok(success(&state["integrity"].to_string()))
                } else {
                    Ok(ReleaseCommandResult {
                        status: Some(1),
                        stdout: String::new(),
                        stderr: "E404".to_owned(),
                    })
                }
            }
            "publish" => {
                let package = self.manifest["packages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|package| {
                        self.directory.join(package["tarball"].as_str().unwrap())
                            == Path::new(&arguments[1])
                    })
                    .unwrap();
                self.remote[package["name"].as_str().unwrap()] =
                    serde_json::json!({"integrity":package["integrity"],"tags":{tag:version}});
                Ok(success(""))
            }
            "dist-tag" if arguments[1] == "ls" => {
                let state = &self.remote[&arguments[2]];
                if let Some(listing) = state.get("listing") {
                    return Ok(success(listing.as_str().unwrap()));
                }
                Ok(success(
                    &state
                        .get("tags")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flatten()
                        .map(|(tag, version)| format!("{tag}: {}", version.as_str().unwrap()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ))
            }
            "dist-tag" if arguments[1] == "add" => {
                let name = arguments[2].rsplit_once('@').unwrap().0;
                if self.remote.get(name).is_none() {
                    self.remote[name] = serde_json::json!({"tags":{}});
                }
                if self.remote[name].get("tags").is_none() {
                    self.remote[name]["tags"] = serde_json::json!({});
                }
                self.remote[name]["tags"][&arguments[3]] = version.into();
                Ok(success(""))
            }
            other => anyhow::bail!("unexpected npm operation {other}"),
        }
    }

    fn log(&mut self, _: &str) {}
    fn warn(&mut self, _: &str) {}
    fn confirm(&mut self, _: &str, _: &str, cancellation: &str) -> anyhow::Result<()> {
        self.confirmations += 1;
        if self.cancelled {
            anyhow::bail!("{cancellation}");
        }
        Ok(())
    }
    fn web_probe(&mut self, _: &InstalledWebProbe) -> anyhow::Result<()> {
        panic!("publication must never run a web probe");
    }
}

fn registry_state(manifest: &Value) -> Value {
    let mut remote = serde_json::Map::new();
    for package in manifest["packages"].as_array().unwrap() {
        remote.insert(package["name"].as_str().unwrap().to_owned(), serde_json::json!({"integrity":package["integrity"],"tags":{manifest["distTag"].as_str().unwrap():manifest["version"]}}));
    }
    remote.get_mut("@seekdeep-ai/seekdeep").unwrap()["tags"]["latest"] =
        manifest["version"].clone();
    remote.into()
}

#[test]
fn publish_and_verify_sequences_match_source_for_missing_identical_and_conflicting_versions() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = bundle(directory.path());
    let manifest = serde_json::to_value(&bundle.manifest).unwrap();
    let mut scenarios = vec![
        (true, serde_json::json!({})),
        (true, registry_state(&manifest)),
        (false, registry_state(&manifest)),
        (false, serde_json::json!({})),
    ];
    let mut stale = registry_state(&manifest);
    stale["@seekdeep-ai/seekdeep"]["tags"] = serde_json::json!({"dev-1.2.3":"old", "latest":"old"});
    scenarios.push((true, stale.clone()));
    scenarios.push((false, stale));
    let mut conflict = registry_state(&manifest);
    conflict["@seekdeep-ai/seekdeep"]["integrity"] = "sha512-different".into();
    scenarios.push((true, conflict.clone()));
    scenarios.push((false, conflict));
    let mut missing_latest = registry_state(&manifest);
    missing_latest["@seekdeep-ai/seekdeep"]["tags"]
        .as_object_mut()
        .unwrap()
        .shift_remove("latest");
    scenarios.push((false, missing_latest));
    let mut invalid_integrity = registry_state(&manifest);
    invalid_integrity["@seekdeep-ai/seekdeep"]["integrity"] = "sha1-invalid".into();
    scenarios.push((true, invalid_integrity));
    let mut malformed_tags = registry_state(&manifest);
    malformed_tags["@seekdeep-ai/seekdeep"]["listing"] = "invalid".into();
    scenarios.push((false, malformed_tags));
    let mut duplicate_tags = registry_state(&manifest);
    duplicate_tags["@seekdeep-ai/seekdeep"]["listing"] = "latest: old\nlatest: old".into();
    scenarios.push((true, duplicate_tags));
    for code in ["E401 unauthorized", "NOT_FOUND"] {
        let mut failure = registry_state(&manifest);
        failure["@seekdeep-ai/seekdeep"]["failure"] =
            serde_json::json!({"status":1,"stdout":"", "stderr":code});
        scenarios.push((false, failure));
    }
    for (publish, remote) in scenarios {
        let mut runner = RegistryRunner {
            manifest: manifest.clone(),
            directory: directory.path().to_owned(),
            remote: remote.clone(),
            calls: Vec::new(),
            cancelled: false,
            confirmations: 0,
        };
        let mut environment = environment();
        environment.insert("npm_config_user_agent".into(), "pnpm-fixture".into());
        environment.insert("NPM_CONFIG_USER_AGENT".into(), "pnpm-fixture-upper".into());
        let publication = RegistryPublication::new(&bundle, &std::env::temp_dir(), &environment);
        let result = if publish {
            publication.publish(&mut runner, true)
        } else {
            publication.verify(&mut runner)
        };
        let actual = serde_json::json!({"calls":runner.calls,"remote":runner.remote,"error":result.err().map(|error| error.to_string())});
        let source = source_call(
            &serde_json::json!({"op":"registry","directory":directory.path(),"manifest":manifest,"publish":publish,"remote":remote}),
        );
        assert_eq!(actual, source["value"]);
    }
}

#[test]
fn cancelled_publication_only_pings_and_resolves_identity() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = bundle(directory.path());
    let mut runner = RegistryRunner {
        manifest: serde_json::to_value(&bundle.manifest).unwrap(),
        directory: directory.path().to_owned(),
        remote: serde_json::json!({}),
        calls: Vec::new(),
        cancelled: true,
        confirmations: 0,
    };
    let error = RegistryPublication::new(&bundle, &std::env::temp_dir(), &environment())
        .publish(&mut runner, false)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "publication cancelled");
    assert_eq!(runner.confirmations, 1);
    assert_eq!(
        runner
            .calls
            .iter()
            .map(|call| call["args"][0].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ping", "whoami"]
    );
    assert_eq!(runner.remote, serde_json::json!({}));
}

#[test]
fn registry_transport_and_dist_tag_parser_match_source_edge_cases() {
    for value in [
        "https://registry.example/path///",
        "HTTP://registry.example/",
        "http://127.0.0.1:4873",
        "ftp://example.test/",
    ] {
        let source = source_call(&serde_json::json!({"op":"normalize","value":value}));
        match normalize_registry(value) {
            Ok(value) => assert_eq!(source["value"], value),
            Err(error) => assert_eq!(source["error"], error.to_string()),
        }
    }
    for value in [
        "",
        "latest: 1.2.3\r\ndev: 1.2.4\r\n",
        "next: 1.0.0: unexpected",
        "latest: 1\nlatest: 2",
        ": 1",
        "latest: ",
        " latest: 1",
        " ",
    ] {
        let source =
            source_call(&serde_json::json!({"op":"tags","value":value,"name":"@seekdeep-ai/test"}));
        match parse_dist_tag_listing(value, "@seekdeep-ai/test") {
            Ok(tags) => assert_eq!(source["value"], serde_json::to_value(tags).unwrap()),
            Err(error) => assert_eq!(source["error"], error.to_string()),
        }
    }
    let _: BTreeMap<String, String> = parse_dist_tag_listing("", "unused").unwrap();
}

#[cfg(unix)]
#[test]
fn cli_publish_and_verify_use_only_the_fixture_npm_process_and_persist_expected_tags() {
    use std::{os::unix::fs::PermissionsExt as _, process::Command};
    let root = super::support::workspace();
    super::support::initialize_git(root.path());
    let directory = tempfile::tempdir().unwrap();
    let bundle = bundle(directory.path());
    let tools = tempfile::tempdir().unwrap();
    let npm = tools.path().join("npm");
    std::fs::write(&npm, FIXTURE_NPM).unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let state = tools.path().join("state.json");
    super::support::write_json(&state, &serde_json::json!({"packages":{},"calls":[]}));
    let path = std::env::join_paths(
        std::iter::once(tools.path().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    for (command, flags) in [
        ("publish", vec!["--yes"]),
        ("verify", vec![]),
        ("publish", vec!["--yes"]),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_publish-npm-baseline"))
            .arg(command)
            .arg("--manifest")
            .arg(bundle.directory.join("manifest.json"))
            .args(flags)
            .current_dir(root.path())
            .env("PATH", &path)
            .env("SEEKDEEP_TEST_NPM_STATE", &state)
            .env(
                "SEEKDEEP_TEST_NPM_MANIFEST",
                bundle.directory.join("manifest.json"),
            )
            .env("npm_config_user_agent", "pnpm-fixture")
            .env("NPM_CONFIG_USER_AGENT", "pnpm-fixture-upper")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("verified @seekdeep-ai/seekdeep@latest")
        );
    }
    let state: Value = serde_json::from_str(&std::fs::read_to_string(state).unwrap()).unwrap();
    let calls = state["calls"].as_array().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["arguments"][0] == "publish")
            .count(),
        2
    );
    for package in &bundle.manifest.packages {
        let remote = &state["packages"][package.name.as_str()];
        assert_eq!(remote["integrity"], package.integrity);
        assert_eq!(
            remote["tags"][&bundle.manifest.dist_tag],
            bundle.manifest.version.as_str()
        );
    }
    assert_eq!(
        state["packages"]["@seekdeep-ai/seekdeep"]["tags"]["latest"],
        bundle.manifest.version.as_str()
    );
    assert!(
        calls
            .iter()
            .all(|call| call["cwd"].as_str().unwrap() != root.path().to_str().unwrap())
    );
    assert!(calls.iter().all(|call| {
        call["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|argument| argument == "--registry=http://127.0.0.1:9")
    }));
}

#[cfg(unix)]
const FIXTURE_NPM: &str = r"#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
if (process.env.npm_config_user_agent || process.env.NPM_CONFIG_USER_AGENT) throw Error('npm environment leaked');
const statePath = process.env.SEEKDEEP_TEST_NPM_STATE;
const manifestPath = process.env.SEEKDEEP_TEST_NPM_MANIFEST;
const state = JSON.parse(fs.readFileSync(statePath,'utf8'));
const manifest = JSON.parse(fs.readFileSync(manifestPath,'utf8'));
const args = process.argv.slice(2);
state.calls.push({arguments:args,cwd:process.cwd()});
const name = value => value.slice(0,value.lastIndexOf('@'));
let output = '';
let status = 0;
if (args[0] === 'ping') output = 'pong';
else if (args[0] === 'whoami') output = 'fixture-account';
else if (args[0] === 'view') {
  const pkg=state.packages[name(args[1])];
  if(pkg) output=JSON.stringify(pkg.integrity);
  else {process.stderr.write('E404');status=1;}
} else if(args[0] === 'publish') {
  const pkg=manifest.packages.find(pkg=>path.join(path.dirname(manifestPath),pkg.tarball)===args[1]);
  if(!pkg) throw Error('unexpected tarball');
  state.packages[pkg.name]={integrity:pkg.integrity,tags:{[manifest.distTag]:manifest.version}};
} else if(args[0] === 'dist-tag' && args[1] === 'ls') {
  output=Object.entries(state.packages[args[2]]?.tags??{}).map(([tag,version])=>`${tag}: ${version}`).join('\n');
} else if(args[0] === 'dist-tag' && args[1] === 'add') {
  state.packages[name(args[2])].tags[args[3]]=manifest.version;
} else throw Error('fixture refuses unexpected npm operation');
fs.writeFileSync(statePath,JSON.stringify(state));
process.stdout.write(output);
process.exitCode=status;
";
