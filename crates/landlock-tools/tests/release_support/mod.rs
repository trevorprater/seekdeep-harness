#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use anyhow::Result;
use seekdeep_landlock_tools::{
    process::{CommandOutput, CommandSpec, Runner},
    repo::Repository,
};
use serde_json::{Value, json};

pub(crate) const ENTRY_NAME: &str = "@seekdeep-ai/node-addon-landlock-run";

pub(crate) fn source_root() -> PathBuf {
    std::env::var_os("SEEKDEEP_PARITY_SOURCE")
        .map_or_else(
            || {
                PathBuf::from(
                    include_str!("../../../../SOURCE_SNAPSHOT")
                        .lines()
                        .find_map(|line| line.strip_prefix("repository="))
                        .unwrap(),
                )
            },
            PathBuf::from,
        )
        .join("native/landlock-run")
}

pub(crate) struct Fixture {
    pub(crate) temporary: tempfile::TempDir,
    pub(crate) repository: Repository,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("native/landlock-run");
        fs::create_dir_all(root.join("packages/entry/lib")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        write_json(
            &temporary.path().join("package.json"),
            &json!({"private":true}),
        );
        fs::write(
            temporary.path().join("pnpm-workspace.yaml"),
            "packages:\n  - native/landlock-run/packages/*\n",
        )
        .unwrap();
        fs::write(
            temporary.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .unwrap();
        write_json(
            &root.join("package.json"),
            &json!({"name":"landlock-workspace", "version":"9.9.9", "private":true, "type":"module"}),
        );
        for (cpu, machine) in [("x64", 62_u16), ("arm64", 183_u16)] {
            let directory = root.join(format!("packages/linux-{cpu}"));
            fs::create_dir_all(directory.join("bin")).unwrap();
            write_json(
                &directory.join("package.json"),
                &json!({
                    "name":format!("{ENTRY_NAME}-linux-{cpu}"), "version":"0.1.1", "os":["linux"], "cpu":[cpu],
                    "files":["bin/", "prebuilds.json"], "license":"BSD-3-Clause", "publishConfig":{"access":"public"}
                }),
            );
            write_json(
                &directory.join("prebuilds.json"),
                &json!({
                    "platform":format!("linux-{cpu}"), "binaries":[{"tool":"landlock-run", "kind":"launcher", "path":"bin/landlock-run"}]
                }),
            );
            let mut bytes = vec![0; 64];
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[18..20].copy_from_slice(&machine.to_le_bytes());
            fs::write(directory.join("bin/landlock-run"), bytes).unwrap();
            make_executable(&directory.join("bin/landlock-run"));
        }
        write_json(
            &root.join("packages/entry/package.json"),
            &json!({
                "name":ENTRY_NAME, "version":"0.1.1", "type":"module", "main":"lib/index.js", "types":"lib/index.d.ts",
                "exports":{".":{"types":"./lib/index.d.ts", "default":"./lib/index.js"}, "./package.json":"./package.json"},
                "files":["lib/"], "license":"BSD-3-Clause", "publishConfig":{"access":"public"},
                "optionalDependencies":{
                    format!("{ENTRY_NAME}-linux-arm64"):"workspace:*",
                    format!("{ENTRY_NAME}-linux-x64"):"workspace:*"
                }
            }),
        );
        fs::write(
            root.join("packages/entry/lib/index.d.ts"),
            "export declare function launcherPath(): string;\n",
        )
        .unwrap();
        fs::write(root.join("packages/entry/lib/index.js"), r"import path from 'node:path';
import { fileURLToPath } from 'node:url';
export function launcherPath() { return path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../node-addon-landlock-run-' + process.platform + '-' + process.arch + '/bin/landlock-run'); }
export function probe() { return 'unusable'; }
export function grantArgs(grants) { return [...(grants.readOnly || []).flatMap(root => ['--ro',root]), ...(grants.readWrite || []).flatMap(root => ['--rw',root])]; }
").unwrap();
        Self {
            temporary,
            repository: Repository::new(root),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.repository.root
    }

    pub(crate) fn manifest(&self, directory: &str) -> Value {
        serde_json::from_slice(&fs::read(self.root().join(directory).join("package.json")).unwrap())
            .unwrap()
    }

    pub(crate) fn set_version(&self, directory: &str, version: &str) {
        let mut manifest = self.manifest(directory);
        manifest["version"] = version.into();
        write_json(&self.root().join(directory).join("package.json"), &manifest);
    }

    pub(crate) fn artifacts(&self) -> PathBuf {
        let root = self.temporary.path().join("artifacts");
        for cpu in ["x64", "arm64"] {
            let destination = root.join(format!("prebuild-linux-{cpu}"));
            fs::create_dir_all(&destination).unwrap();
            fs::copy(
                self.root()
                    .join(format!("packages/linux-{cpu}/bin/landlock-run")),
                destination.join("landlock-run"),
            )
            .unwrap();
        }
        root
    }

    pub(crate) fn install_source_scripts(&self, stub: bool) {
        let scripts = self.root().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        for script in [
            "repo.mjs",
            "assemble-prebuilds.mjs",
            "bump-release.mjs",
            "commit-release.mjs",
            "pack-release.mjs",
            "publish-release.mjs",
            "verify-packed-install.mjs",
            "verify-release.mjs",
        ] {
            let mut source = fs::read_to_string(source_root().join("scripts").join(script))
                .unwrap()
                .replace("@deepseek-ai/", "@seekdeep-ai/");
            if stub {
                source = source
                    .replace("from 'node:child_process'", "from './fixture-process.mjs'")
                    .replace(
                        "import { setTimeout as sleep } from 'node:timers/promises';",
                        "import { sleep } from './fixture-process.mjs';",
                    );
            }
            fs::write(scripts.join(script), source).unwrap();
        }
        if stub {
            fs::write(scripts.join("fixture-process.mjs"), SOURCE_RUNNER).unwrap();
        }
    }

    pub(crate) fn source(
        &self,
        script: &str,
        args: &[String],
        environment: &[(&str, &str)],
        plan: Option<&Value>,
    ) -> Output {
        self.install_source_scripts(plan.is_some());
        if let Some(plan) = plan {
            write_json(&self.root().join("fixture-plan.json"), plan);
            fs::write(self.root().join("fixture-calls.jsonl"), "").unwrap();
        }
        let mut command = Command::new("node");
        command
            .arg(self.root().join("scripts").join(script))
            .args(args)
            .current_dir(self.root())
            .env_remove("GITHUB_REF")
            .env_remove("RELEASE_PUBLISH")
            .env_remove("NALR_REQUIRE_LANDLOCK");
        for (key, value) in environment {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    pub(crate) fn source_calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("fixture-calls.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

pub(crate) fn write_json(file: &Path, value: &Value) {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(
        file,
        format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
    )
    .unwrap();
}

pub(crate) fn make_executable(file: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

pub(crate) fn output(status: i32, stdout: &str, stderr: &str) -> CommandOutput {
    CommandOutput {
        status: Some(status),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        spawn_error: None,
        unstarted: false,
    }
}

type Execute = Box<dyn FnMut(&CommandSpec) -> Result<CommandOutput>>;
pub(crate) struct RecordingRunner {
    pub(crate) calls: Vec<CommandSpec>,
    pub(crate) delays: Vec<Duration>,
    pub(crate) logs: Vec<String>,
    execute: Execute,
}

impl RecordingRunner {
    pub(crate) fn new(
        execute: impl FnMut(&CommandSpec) -> Result<CommandOutput> + 'static,
    ) -> Self {
        Self {
            calls: Vec::new(),
            delays: Vec::new(),
            logs: Vec::new(),
            execute: Box::new(execute),
        }
    }

    pub(crate) fn success() -> Self {
        Self::new(|_| Ok(output(0, "", "")))
    }
}

impl Runner for RecordingRunner {
    fn run(&mut self, spec: &CommandSpec) -> Result<CommandOutput> {
        self.calls.push(spec.clone());
        (self.execute)(spec)
    }
    fn sleep(&mut self, duration: Duration) {
        self.delays.push(duration);
    }
    fn log(&mut self, message: &str) {
        self.logs.push(message.to_owned());
    }
}

pub(crate) fn assert_source_success(output: &Output) {
    assert!(
        output.status.success(),
        "source status {:?}\nstdout {}\nstderr {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn normalize(text: &str, fixture: &Fixture) -> String {
    text.replace(
        &fixture
            .temporary
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "<fixture>",
    )
    .replace(
        &fixture.temporary.path().to_string_lossy().into_owned(),
        "<fixture>",
    )
}

const SOURCE_RUNNER: &str = r"import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync as nativeSpawnSync } from 'node:child_process';
const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const planPath = path.join(root, 'fixture-plan.json');
const callPath = path.join(root, 'fixture-calls.jsonl');
function record(value) { fs.appendFileSync(callPath, JSON.stringify(value) + '\n'); }
export async function sleep(ms) { record({ sleep: ms }); }
export function spawnSync(program, args, options = {}) {
  record({program,args,cwd:options.cwd || process.cwd(),ci:options.env?.CI || null});
  if (program === 'node' && args[0]?.startsWith('./scripts/')) return nativeSpawnSync(program,args,options);
  const plan = JSON.parse(fs.readFileSync(planPath,'utf8'));
  if (plan.pack && (program === 'npm' || program === 'pnpm')) {
    const packageDir = program === 'npm' ? args[1] : args[1];
    const manifest = JSON.parse(fs.readFileSync(path.resolve(root, packageDir, 'package.json'), 'utf8'));
    const file = manifest.name.replace(/^@/,'').replace('/','-') + '-' + manifest.version + '.tgz';
    const destination = args[args.indexOf('--pack-destination') + 1];
    fs.writeFileSync(path.join(destination,file),'packed');
    return {status:0,stdout:'',stderr:''};
  }
  const index = plan.index || 0;
  if (!plan.responses || index >= plan.responses.length) throw new Error('unplanned child: ' + program + ' ' + args.join(' '));
  const result = plan.responses[index];
  plan.index = index + 1;
  fs.writeFileSync(planPath, JSON.stringify(plan));
  return result;
}
";
