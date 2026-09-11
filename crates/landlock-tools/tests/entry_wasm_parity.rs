//! The shipped Node entry calls compiled Rust/WASM over the source API contract.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[test]
fn built_node_entry_matches_pinned_source_exports_resolution_grants_and_probe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let build = Command::new(env!("CARGO_BIN_EXE_landlock-build-entry"))
        .current_dir(&root)
        .status()
        .expect("start the Rust/WASM entry build");
    assert!(
        build.success(),
        "the native entry package must be built before its runtime proof"
    );
    for (source, name) in [
        ("crates/landlock-run/src/main.rs", "landlock-run.main.rs"),
        ("crates/landlock-run/src/lib.rs", "landlock-run.lib.rs"),
        ("crates/landlock-run/Cargo.toml", "landlock-run.Cargo.toml"),
        ("Cargo.toml", "seekdeep-workspace.Cargo.toml"),
        ("Cargo.lock", "seekdeep-workspace.Cargo.lock"),
        (
            "rust-toolchain.toml",
            "seekdeep-workspace.rust-toolchain.toml",
        ),
        ("LICENSE", "seekdeep-harness.LICENSE"),
    ] {
        assert_eq!(
            std::fs::read(root.join(source)).unwrap(),
            std::fs::read(
                root.join("native/landlock-run/packages/entry/lib")
                    .join(name)
            )
            .unwrap(),
            "packed audit source must match its Rust workspace source"
        );
    }
    let output = Command::new("node")
        .args(["--input-type=module", "-e", DRIVER])
        .arg(root.join("native/landlock-run/packages/entry/lib/index.js"))
        .arg(source_root().join("native/landlock-run/packages/entry/src/index.ts"))
        .output()
        .expect("run the Node entry parity proof");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "entry Rust/WASM parity: 41 cases passed"
    );
}

fn source_root() -> PathBuf {
    std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || {
            PathBuf::from(
                include_str!("../../../SOURCE_SNAPSHOT")
                    .lines()
                    .find_map(|line| line.strip_prefix("repository="))
                    .unwrap(),
            )
        },
        PathBuf::from,
    )
}

const DRIVER: &str = r#"
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [actualPath, sourcePath] = process.argv.slice(1).map(filename => fs.realpathSync(path.resolve(filename)));
const actual = await import(pathToFileURL(actualPath));
const source = await import(pathToFileURL(sourcePath));
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'landlock-entry-parity-'));
let cases = 0;
const equal = (left, right) => { assert.deepEqual(left, right); cases++; };
const normalize = (value, filename) => JSON.stringify(value)
  .replaceAll('@deepseek-ai/', '@seekdeep-ai/')
  .replaceAll(path.dirname(path.dirname(filename)), '$ENTRY');
const compare = (run) => equal(normalize(run(actual), actualPath), normalize(run(source), sourcePath));
try {
  equal(Object.keys(actual).sort(), Object.keys(source).sort());
  compare(api => [api.LAUNCHER_BIN, api.LAUNCHER_FAILURE_EXIT]);
  compare(api => ['launcherPath','grantArgs','probe'].map(name => [api[name].name,api[name].length]));
  for (const grants of [{}, {readOnly:['/']}, {readOnly:['/','/opt'],readWrite:['/tmp/work']}, {readWrite:['/a'],readOnly:['/b']}, {readOnly:null,readWrite:undefined}, {readOnly:['space path','中文/路径','\n'],readWrite:['']}, {readOnly:[null,undefined,7]}, {readOnly:[,'/a',,'/b']}, {readOnly:[],readWrite:[]}]) {
    compare(api => api.grantArgs(grants));
  }
  for (const grants of [true,false,7,'text',Symbol('grants'),1n]) compare(api => api.grantArgs(grants));
  compare(api => {
    let callback;
    const result = api.grantArgs({readOnly:{flatMap(fn){callback=fn;return['custom'];}}});
    return {result,later:callback('/late')};
  });
  compare(api => {
    const trace=[];
    const result=api.grantArgs({readOnly:{flatMap(){return{*[Symbol.iterator](){trace.push('first');yield'--ro';trace.push('second');yield'/value';trace.push('end');}};}}});
    return {result,trace};
  });
  for (const grants of [undefined,null,{readOnly:7},{readOnly:{}},{readOnly:false},{readWrite:''}]) {
    compare(api => {try{api.grantArgs(grants);return{ok:true};}catch(error){return{name:error.name,message:error.message};}});
  }
  compare(api => {try{api.probe('unused',null);return{ok:true};}catch(error){return{name:error.name,message:error.message};}});
  compare(api => {
    const trace = [];
    const result = api.grantArgs({
      get readOnly() { trace.push('readOnly'); return ['/a']; },
      get readWrite() { trace.push('readWrite'); return ['/b']; },
    });
    return {trace,result};
  });
  compare(api => {
    const trace = [], marker = {error:'same-object'};
    try { api.grantArgs({get readOnly(){trace.push('readOnly');throw marker;},get readWrite(){trace.push('readWrite');return[];}}); }
    catch(error) { return {same:error===marker,trace}; }
    throw new Error('expected getter failure');
  });
  compare(api => {
    const requested = [];
    const result = api.launcherPath(specifier => { requested.push(specifier); return path.join('/fake-install', specifier); });
    return {requested,result};
  });
  compare(api => api.launcherPath(() => {throw new Error('not installed');}));
  compare(api => api.launcherPath(() => ({bad:'path'})));
  compare(api => api.launcherPath(null));
  equal(path.isAbsolute(actual.launcherPath()), true);
  equal(actual.launcherPath().endsWith(path.join('bin','landlock-run')), true);
  compare(api => api.probe(path.join(dir,'missing')));
  if (process.platform !== 'win32') {
    const fake = (name, body) => { const file=path.join(dir,name); fs.writeFileSync(file,`#!/bin/sh\n${body}\n`,{mode:0o755}); return file; };
    for (const [name,body,options] of [
      ['full','echo "landlock: fully enforced"; exit 0',{}],
      ['partial','echo "landlock: partially enforced (older ABI)"; exit 0',{}],
      ['failure','exit 125',{}],
      ['empty','exit 0',{}],
      ['timeout','exec sleep 10',{timeoutMs:40}],
    ]) {
      const file=fake(name,body); compare(api => api.probe(file,options));
    }
  } else {
    cases += 5;
  }
  assert.equal(cases,41);
  process.stdout.write(`entry Rust/WASM parity: ${cases} cases passed\n`);
} finally { fs.rmSync(dir,{recursive:true,force:true}); }
"#;
