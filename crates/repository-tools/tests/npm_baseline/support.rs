use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use chrono::{DateTime, Utc};
use seekdeep_repository_tools::{
    npm_baseline::{BaselineRunner, InstalledWebProbe, SystemBaselineRunner},
    release_process::{ReleaseCommandResult, ReleaseRunOptions},
};
use serde_json::Value;

pub(super) fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

pub(super) fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-11T01:02:03.456Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub(super) fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
    )
    .unwrap();
}

pub(super) fn workspace() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_json(
        &root.path().join("package.json"),
        &serde_json::json!({"name":"@seekdeep-ai/seekdeep-root", "version":"1.2.3", "private":true}),
    );
    for (directory, name, version) in [
        ("apps/cli", "@seekdeep-ai/seekdeep", "1.2.3"),
        ("packages/core/base", "@seekdeep-ai/base", "1.2.3"),
        ("vendor/upstream", "@seekdeep-ai/vendor", "9.8.7"),
    ] {
        write_json(
            &root.path().join(directory).join("package.json"),
            &serde_json::json!({"name":name,"version":version,"private":true,"type":"module","files":["lib"],"main":"./lib/index.js"}),
        );
        std::fs::create_dir_all(root.path().join(directory).join("lib")).unwrap();
        std::fs::write(
            root.path().join(directory).join("lib/index.js"),
            "export const value = 42;\n",
        )
        .unwrap();
    }
    let cli = root.path().join("apps/cli/package.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&cli).unwrap()).unwrap();
    manifest["bin"] = serde_json::json!({"seekdeep":"lib/bin.js"});
    manifest["dependencies"] = serde_json::json!({"@seekdeep-ai/base":"workspace:^"});
    manifest["devDependencies"] = serde_json::json!({"@seekdeep-ai/vendor":"workspace:*"});
    manifest["optionalDependencies"] = serde_json::json!({"@seekdeep-ai/vendor":"workspace:~"});
    manifest["peerDependencies"] = serde_json::json!({"@seekdeep-ai/base":"^1.0.0"});
    write_json(&cli, &manifest);
    std::fs::write(root.path().join("apps/cli/lib/bin.js"), INSTALLED_ENTRY).unwrap();
    root
}

pub(super) const INSTALLED_ENTRY: &str = r"#!/usr/bin/env node
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
const version = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8')).version;
const record = event => {
  if (process.env.SEEKDEEP_TEST_PROBE_EVENTS) fs.appendFileSync(process.env.SEEKDEEP_TEST_PROBE_EVENTS, JSON.stringify({event,pid:process.pid,cwd:process.cwd(),stdinTTY:!!process.stdin.isTTY,stdoutTTY:!!process.stdout.isTTY}) + '\n');
};
if (process.argv[2] === '--version') { record('version'); console.log(version); }
else if (process.argv[2] === 'web') {
  if (!process.stdin.isTTY || !process.stdout.isTTY) throw new Error('entry must run in a PTY');
  if (process.env.NODE_OPTIONS || process.env.NODE_PATH || process.env.COLORTERM) throw new Error('inherited workspace injection');
  if (fs.realpathSync(path.dirname(process.env.SEEKDEEP_HOME)) !== fs.realpathSync(process.cwd())) throw new Error('home outside consumer');
  const server = http.createServer((request, response) => response.end('ready'));
  process.on('SIGTERM', () => server.close(() => { record('stopped'); process.exit(0); }));
  server.listen(0, '127.0.0.1', () => { record('ready'); console.log(`seekdeep web: http://127.0.0.1:${server.address().port}`); });
} else throw new Error('unexpected entry arguments');
";

pub(super) fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub(super) fn initialize_git(root: &Path) {
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Baseline Fixture"]);
    git(root, &["config", "user.email", "baseline@example.invalid"]);
    git(root, &["add", "."]);
    git(
        root,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
}

pub(super) fn tarball(
    directory: &Path,
    filename: &str,
    manifest: &Value,
    extra_files: &[(&str, &str)],
) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    let stage = tempfile::tempdir().unwrap();
    write_json(&stage.path().join("package/package.json"), manifest);
    for (path, content) in extra_files {
        let path = stage.path().join("package").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let path = directory.join(filename);
    let output = Command::new("tar")
        .arg("-czf")
        .arg(&path)
        .arg("-C")
        .arg(stage.path())
        .arg("package")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

#[derive(Clone, Debug)]
pub(super) struct Call {
    pub command: String,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Default)]
pub(super) struct NpmFixtureRunner {
    pub calls: Vec<Call>,
    pub logs: Vec<String>,
    pub warnings: Vec<String>,
    pub fail: Option<String>,
    pub real_install: bool,
    pub real_probe: bool,
    pub consumer: Option<PathBuf>,
    pub worktree: Option<PathBuf>,
    pub confirmation_error: Option<String>,
    pub confirmation_count: usize,
}

impl BaselineRunner for NpmFixtureRunner {
    fn result(
        &mut self,
        command: &str,
        arguments: &[String],
        options: &ReleaseRunOptions,
    ) -> anyhow::Result<ReleaseCommandResult> {
        self.calls.push(Call {
            command: command.to_owned(),
            arguments: arguments.to_vec(),
            cwd: options.cwd.clone().unwrap_or_default(),
        });
        let operation = format!("{command} {}", arguments.join(" "));
        if self
            .fail
            .as_ref()
            .is_some_and(|failure| operation.starts_with(failure))
        {
            return Ok(ReleaseCommandResult {
                status: Some(17),
                stdout: String::new(),
                stderr: "fixture failure".to_owned(),
            });
        }
        if command == "git" && arguments.starts_with(&["worktree".to_owned(), "add".to_owned()]) {
            self.worktree = Some(PathBuf::from(&arguments[3]));
        }
        if command == "pnpm" {
            if arguments.first().map(String::as_str) == Some("--filter") {
                let root = options.cwd.as_ref().unwrap();
                let destination = Path::new(arguments.last().unwrap());
                for (path, name) in [
                    ("apps/cli", "cli"),
                    ("packages/core/base", "base"),
                    ("vendor/upstream", "vendor"),
                ] {
                    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(
                        root.join(path).join("package.json"),
                    )?)?;
                    let files = if path == "apps/cli" {
                        vec![
                            ("lib/bin.js", INSTALLED_ENTRY),
                            ("lib/index.js", "export const value = 42;\n"),
                        ]
                    } else {
                        vec![("lib/index.js", "export const value = 42;\n")]
                    };
                    tarball(destination, &format!("{name}.tgz"), &manifest, &files);
                }
            }
            return Ok(success(""));
        }
        if command == "npm" && arguments.first().map(String::as_str) == Some("install") {
            let consumer = options.cwd.as_ref().unwrap();
            self.consumer = Some(consumer.clone());
            let environment = options.env.as_ref().unwrap();
            assert!(!environment.contains_key(std::ffi::OsStr::new("npm_config_user_agent")));
            assert!(!environment.contains_key(std::ffi::OsStr::new("NPM_CONFIG_USER_AGENT")));
            if !self.real_install {
                let manifest: Value =
                    serde_json::from_str(&std::fs::read_to_string(consumer.join("package.json"))?)?;
                let dependencies = manifest["dependencies"].as_object().unwrap();
                for (name, source) in dependencies {
                    let destination = consumer.join("node_modules").join(name);
                    std::fs::create_dir_all(&destination)?;
                    let tarball = url::Url::parse(source.as_str().unwrap())?
                        .to_file_path()
                        .unwrap();
                    let output = Command::new("tar")
                        .arg("-xf")
                        .arg(tarball)
                        .arg("--strip-components=1")
                        .arg("-C")
                        .arg(destination)
                        .output()?;
                    assert!(output.status.success());
                }
                return Ok(success(""));
            }
        }
        if command == "npm" && arguments.first().map(String::as_str) != Some("install") {
            anyhow::bail!("fixture refuses unmocked registry command");
        }
        SystemBaselineRunner.result(command, arguments, options)
    }

    fn log(&mut self, message: &str) {
        self.logs.push(message.to_owned());
    }
    fn warn(&mut self, message: &str) {
        self.warnings.push(message.to_owned());
    }
    fn confirm(&mut self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
        self.confirmation_count += 1;
        if let Some(error) = &self.confirmation_error {
            anyhow::bail!("{error}");
        }
        Ok(())
    }
    fn web_probe(&mut self, probe: &InstalledWebProbe) -> anyhow::Result<()> {
        if self.fail.as_deref() == Some("web_probe") {
            anyhow::bail!("fixture web probe failure");
        }
        if self.real_probe {
            SystemBaselineRunner.web_probe(probe)
        } else {
            Ok(())
        }
    }
}

pub(super) fn success(stdout: &str) -> ReleaseCommandResult {
    ReleaseCommandResult {
        status: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

pub(super) fn environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().collect()
}

pub(super) fn source_call(input: &Value) -> Value {
    use std::io::Write as _;
    let source_root = repository_root().parent().unwrap().join("deepseek-harness");
    let source =
        std::fs::read_to_string(source_root.join("scripts/publish-npm-baseline.ts")).unwrap();
    let source = source.split("\ntry {\n  await main()\n}").next().unwrap();
    let source = source
        .replace(
            "'./publication-payload.ts'",
            &serde_json::to_string(
                source_root
                    .join("scripts/publication-payload.ts")
                    .to_str()
                    .unwrap(),
            )
            .unwrap(),
        )
        .replace("@deepseek-ai", "@seekdeep-ai")
        .replace("DSH_", "SEEKDEEP_")
        .replace("dsh", "seekdeep");
    let temporary = tempfile::tempdir().unwrap();
    let module = temporary.path().join("baseline.mts");
    std::fs::write(&module, format!("{source}\nexport {{ WorkspacePackageSet, ReleaseBundle, CommandRunner, BaselinePackager, RegistryPublication, normalizeRegistry, parseDistTagListing, main }};\n")).unwrap();
    let driver = temporary.path().join("driver.mjs");
    std::fs::write(&driver, ORACLE).unwrap();
    let mut child = Command::new("node")
        .arg("--import")
        .arg(source_root.join("node_modules/tsx/dist/loader.mjs"))
        .arg(driver)
        .arg(&module)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "source oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "oracle output was not JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

const ORACLE: &str = r"
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const baseline = await import(pathToFileURL(process.argv[2]).href);
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const logs = [];
console.log = (...values) => logs.push(values.join(' '));
let value;
try {
  if (input.op === 'discover') value = baseline.WorkspacePackageSet.discover(input.root);
  else if (input.op === 'stage') {
    const set = baseline.WorkspacePackageSet.discover(input.root);
    set.stage(input.root, input.version);
    value = set.packages.map(pkg => ({directory: pkg.directory, manifest: JSON.parse(fs.readFileSync(path.join(input.root,pkg.directory,'package.json'),'utf8'))}));
  } else if (input.op === 'load') value = baseline.ReleaseBundle.load(input.path,new baseline.CommandRunner()).manifest;
  else if (input.op === 'create') value = baseline.ReleaseBundle.create(input.directory,input.packages,input.commit,input.version,input.distTag,input.registry,new baseline.CommandRunner()).manifest;
  else if (input.op === 'plan') value = new baseline.BaselinePackager(input.root,new baseline.CommandRunner(),() => new Date(input.now)).plan({ref:input.ref ?? 'HEAD',registry:input.registry ?? 'https://registry.npm.harnessment.com',outputDirectory:input.output});
  else if (input.op === 'capture') {
    const output = new baseline.CommandRunner().result(input.command,input.arguments,input.root);
    value = {status:output.status,stdoutLength:output.stdout.length,stderrLength:output.stderr.length,stdoutPrefix:output.stdout.slice(0,24),stderrPrefix:output.stderr.slice(0,24)};
  }
  else if (input.op === 'run') value = new baseline.CommandRunner().run(input.command,input.arguments,input.root);
  else if (input.op === 'registry') {
    const calls = [];
    const remote = structuredClone(input.remote ?? {});
    const bundle = {directory:input.directory,manifest:input.manifest,tarballPath(pkg){return path.resolve(this.directory,pkg.tarball)}};
    const result = (command,args,cwd,environment) => {
      calls.push({command,args});
      if(command !== 'npm') throw Error('unexpected command');
      if(args[0] === 'ping') return {status:0,stdout:'pong',stderr:''};
      if(args[0] === 'whoami') return {status:0,stdout:'fixture-identity',stderr:''};
      if(args[0] === 'view') {
        const name=args[1].slice(0,args[1].lastIndexOf('@'));
        const state=remote[name];
        if(state?.failure) return state.failure;
        return state?.integrity ? {status:0,stdout:JSON.stringify(state.integrity),stderr:''} : {status:1,stdout:'',stderr:'E404'};
      }
      if(args[0] === 'dist-tag' && args[1] === 'ls') {
        const state=remote[args[2]];
        return {status:0,stdout:state?.listing ?? Object.entries(state?.tags ?? {}).map(([tag,version])=>`${tag}: ${version}`).join('\n'),stderr:''};
      }
      if(args[0] === 'publish') {
        const pkg=input.manifest.packages.find(pkg=>path.resolve(input.directory,pkg.tarball)===args[1]);
        remote[pkg.name]={integrity:pkg.integrity,tags:{[input.manifest.distTag]:input.manifest.version}};
      } else if(args[0] === 'dist-tag' && args[1] === 'add') {
        const name=args[2].slice(0,args[2].lastIndexOf('@'));
        remote[name] ??= {tags:{}};
        remote[name].tags ??= {};
        remote[name].tags[args[3]]=input.manifest.version;
      } else throw Error('unexpected npm command');
      return {status:0,stdout:'',stderr:''};
    };
    const runner={result, capture(...args){const r=result(...args);if(r.status!==0)throw Error('capture failure');return r.stdout.trim()},run(...args){const r=result(...args);if(r.status!==0)throw Error('run failure')}};
    let error=null;
    try {const publication=new baseline.RegistryPublication(bundle,runner);if(input.publish)await publication.publish(true);else publication.verify()}catch(e){error=e.message}
    value={calls,remote,error};
  } else if (input.op === 'normalize') value = baseline.normalizeRegistry(input.value);
  else if (input.op === 'tags') value = Object.fromEntries(baseline.parseDistTagListing(input.value,input.name));
  else if (input.op === 'main') {process.chdir(input.root);process.argv=['node','baseline',...input.arguments];await baseline.main();}
  else throw Error('unknown oracle operation');
  process.stdout.write(JSON.stringify({value:value??null,logs}));
} catch(error) { process.stdout.write(JSON.stringify({error:error.message,logs})); }
";
