//! Differential target/flag corpus against the pinned executable builder.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use seekdeep_python_release::executable::{CliOutcome, Host, Target, parse_cli};
use serde_json::{Value, json};

const ORACLE: &str = r"
const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');
const { parseArgs } = require('node:util');
const root = process.argv[1];
const sourceRequire = createRequire(path.join(root,'package.json'));
const ts = sourceRequire('typescript');
const filename = path.join(root,'scripts/build-exe-for-python-sdk.ts');
const file = ts.createSourceFile(filename,fs.readFileSync(filename,'utf8'),ts.ScriptTarget.Latest,true);
const constants = new Set(['DEFAULT_NODE_RANGE','PKG_SPEC','OUT_DIR','PYTHON_RUNTIME_DIR','PYTHON_NODE_SUBDIR','PLATFORMS','ARCHES']);
const statements = file.statements.filter(statement =>
  ts.isClassDeclaration(statement) && ['Target','BuildCli'].includes(statement.name?.text)
  || ts.isFunctionDeclaration(statement) && ['isPlatform','isArch'].includes(statement.name?.text)
  || ts.isVariableStatement(statement) && statement.declarationList.declarations.some(declaration => ts.isIdentifier(declaration.name) && constants.has(declaration.name.text)));
const code = ts.transpileModule(statements.map(statement=>statement.getText(file)).join('\n'), {
  compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.None}
}).outputText;
const cases = JSON.parse(fs.readFileSync(0,'utf8'));
const output = cases.map(item => {
  const diagnostics = [];
  const fakeProcess = {...item.host,exit(code){throw {exitCode:code}}};
  const fakeConsole = {log(){},error(message){diagnostics.push(String(message))}};
  const source = new Function('parseArgs','process','console',code+'\nreturn {Target,BuildCli};')(parseArgs,fakeProcess,fakeConsole);
  try {
    return {ok:item.kind === 'target' ? source.Target.parse(item.value) : source.BuildCli.parse(item.args)};
  } catch(error) {
    if (error.exitCode === 0) return {help:true};
    if (error.exitCode === 1) return {error:diagnostics[0].trimEnd(),usage:true};
    return {error:error.message,usage:false};
  }
});
process.stdout.write(JSON.stringify(output));
";

fn main() -> anyhow::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("pinned source root required"))?;
    let head = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin missing"))?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == pin,
        "oracle differs from SOURCE_SNAPSHOT"
    );
    let cases = cases();
    let mut child = Command::new("node")
        .args(["-e", ORACLE])
        .arg(Path::new(&root))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("oracle input absent"))?
        .write_all(&serde_json::to_vec(&cases)?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(expected.len() == cases.len(), "source result count differs");
    let mut errors = Vec::new();
    for (case, expected) in cases.iter().zip(expected) {
        let actual = native(case);
        if actual != expected {
            errors.push(json!({"case":case,"native":actual,"source":expected}));
        }
    }
    anyhow::ensure!(
        errors.is_empty(),
        "builder differences: {}",
        serde_json::to_string_pretty(&errors)?
    );
    println!(
        "{} executable target and flag cases match the pinned source",
        cases.len()
    );
    Ok(())
}

fn native(case: &Value) -> Value {
    if case["kind"] == "target" {
        return match Target::parse(case["value"].as_str().unwrap_or_default()) {
            Ok(target) => json!({"ok":target}),
            Err(error) => json!({"error":error.to_string(),"usage":false}),
        };
    }
    let arguments = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let host = Host {
        platform: case["host"]["platform"].as_str().unwrap().to_owned(),
        arch: case["host"]["arch"].as_str().unwrap().to_owned(),
    };
    match parse_cli(&arguments, &host) {
        Ok(CliOutcome::Help) => json!({"help":true}),
        Ok(CliOutcome::Build(options)) => json!({"ok":options}),
        Err(error) => json!({"error":error.message,"usage":error.show_usage}),
    }
}

fn cases() -> Vec<Value> {
    let mut cases = Vec::new();
    for value in [
        "node24-linux-x64",
        "node24-linux-arm64",
        "node24-macos-x64",
        "node24-macos-arm64",
        "node0-linux-x64",
        "node024-linux-x64",
        "node999-macos-arm64",
        "node24\n-linux-x64",
        "node24\r-linux-x64",
        "node24\r\n-linux-x64",
        "node24\u{2028}-linux-x64",
        "node٢٤-linux-x64",
        "linux-x64",
        "node24-linux",
        "node24-linux-x64-extra",
        "node24-windows-x64",
        "node24-linux-ia32",
        "latest-linux-x64",
        "NODE24-linux-x64",
        "",
    ] {
        cases.push(
            json!({"kind":"target","value":value,"host":{"platform":"darwin","arch":"arm64"}}),
        );
    }
    for arguments in [
        vec![],
        vec!["--help"],
        vec!["--dry-run"],
        vec!["--skip-build"],
        vec!["--targets=node24-linux-x64"],
        vec![
            "--targets",
            "node24-linux-x64, node24-macos-arm64",
            "--dry-run",
        ],
        vec!["--targets=node22-linux-x64,node24-linux-x64"],
        vec!["--targets="],
        vec!["--targets= , "],
        vec!["--targets=\u{feff}node24-macos-arm64\u{feff}"],
        vec!["--targets=\u{85}node24-macos-arm64"],
        vec!["--targets"],
        vec!["--targets", "--help"],
        vec!["--targets", "-"],
        vec!["--targets=-x"],
        vec!["--dry-run=false"],
        vec!["--skip-build=true"],
        vec!["--help=false"],
        vec!["--wat=1"],
        vec!["-abc"],
        vec!["-h"],
        vec!["value"],
        vec!["--", "value"],
        vec!["--"],
        vec!["--dry-run", "--dry-run"],
        vec!["--targets=invalid", "--targets=node24-linux-arm64"],
        vec!["--targets=invalid", "--help"],
        vec!["--help", "--wat"],
    ] {
        cases.push(
            json!({"kind":"cli","args":arguments,"host":{"platform":"darwin","arch":"arm64"}}),
        );
    }
    for (platform, arch) in [
        ("linux", "x64"),
        ("linux", "arm64"),
        ("darwin", "x64"),
        ("win32", "x64"),
        ("freebsd", "x64"),
        ("darwin", "ia32"),
    ] {
        cases.push(json!({"kind":"cli","args":[],"host":{"platform":platform,"arch":arch}}));
        cases
            .push(json!({"kind":"cli","args":["--help"],"host":{"platform":platform,"arch":arch}}));
    }
    cases
}
