//! Browser consumption of source maps generated from a real Rust/WASM build.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use seekdeep_repository_tools::client_bundle::sourcemaps::{
    attach_source_map_url, write_wasm_source_map,
};

const FIXTURE: &str = "use wasm_bindgen::prelude::*;\n\n#[wasm_bindgen]\npub fn double(value: u32) -> u32 {\n    value * 2\n}\n\n#[wasm_bindgen]\npub fn fail() {\n    panic!(\"mapped fixture failure\");\n}\n";

#[test]
fn a_real_wasm_frame_maps_to_embedded_rust_source_in_chromium() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("rust-toolchain.toml"),
        include_str!("../../../rust-toolchain.toml"),
    )
    .unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"seekdeep-client-map-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\ncrate-type = [\"cdylib\"]\n[dependencies]\nwasm-bindgen = \"=0.2.127\"\n[workspace]\n[profile.release]\nopt-level = 0\ndebug = \"line-tables-only\"\nstrip = \"none\"\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), FIXTURE).unwrap();
    let build = Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(&root)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_BUILD_JOBS", "2")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let output = root.join("lib");
    let bindgen = Command::new("wasm-bindgen")
        .args([
            "--target",
            "web",
            "--keep-debug",
            "--out-name",
            "client",
            "--out-dir",
        ])
        .arg(&output)
        .arg(target.join("wasm32-unknown-unknown/release/seekdeep_client_map_fixture.wasm"))
        .output()
        .unwrap();
    assert!(
        bindgen.status.success(),
        "{}",
        String::from_utf8_lossy(&bindgen.stderr)
    );
    let artifact = output.join("client_bg.wasm");
    let map = write_wasm_source_map(&artifact, &root).unwrap();
    let source_index = map
        .sources
        .iter()
        .position(|source| source == "../../../src/lib.rs")
        .expect("first-party Rust source URL");
    assert_eq!(map.sources_content[source_index].as_deref(), Some(FIXTURE));
    let bytes = std::fs::read(&artifact).unwrap();
    assert_eq!(
        attach_source_map_url(&bytes, "client_bg.wasm.map").unwrap(),
        bytes
    );
    let driver = root.join("browser.mjs");
    std::fs::write(&driver, BROWSER).unwrap();
    let source = std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        PathBuf::from,
    );
    let browser = Command::new("node")
        .arg(&driver)
        .arg(&source)
        .arg(&output)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        browser.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&browser.stdout),
        String::from_utf8_lossy(&browser.stderr)
    );
    println!("{}", String::from_utf8_lossy(&browser.stdout));
}

const BROWSER: &str = r"
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { globSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';
const require = createRequire(join(process.argv[2], 'apps/web/package.json'));
const { chromium } = require('playwright');
const tracePath = [...globSync('node_modules/.pnpm/@jridgewell+trace-mapping@*/node_modules/@jridgewell/trace-mapping/package.json', { cwd: process.argv[2] })].sort()[0];
assert.ok(tracePath, 'source-map consumer dependency is missing');
const { TraceMap, originalPositionFor } = createRequire(join(process.argv[2], tracePath))('./dist/trace-mapping.umd.js');
const root = process.argv[3];
const requests = [];
const server = createServer(async (request, response) => {
  const path = new URL(request.url, 'http://fixture').pathname;
  requests.push(path);
  if (path === '/') { response.setHeader('Content-Type', 'text/html'); response.end('<!doctype html><html><body>Rust Client source-map fixture</body></html>'); return; }
  try {
    const name = path.slice(1);
    if (!['client.js', 'client_bg.wasm', 'client_bg.wasm.map'].includes(name)) throw new Error('missing');
    response.setHeader('Content-Type', name.endsWith('.wasm') ? 'application/wasm' : name.endsWith('.map') ? 'application/json' : 'text/javascript');
    response.end(await readFile(join(root, name)));
  } catch { response.statusCode = 404; response.end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const cdp = await page.context().newCDPSession(page);
  const scripts = [];
  cdp.on('Debugger.scriptParsed', script => scripts.push(script));
  await cdp.send('Debugger.enable');
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const result = await page.evaluate(async () => {
    const module = await import('/client.js');
    await module.default({ module_or_path: '/client_bg.wasm' });
    const result = { value: module.double(21) };
    try { module.fail(); } catch (error) { result.stack = error.stack; }
    return result;
  });
  assert.equal(result.value, 42);
  assert.ok(result.stack.includes('wasm-function'), result.stack);
  const map = JSON.parse(await readFile(join(root, 'client_bg.wasm.map'), 'utf8'));
  const trace = new TraceMap(map);
  const frames = [...result.stack.matchAll(/wasm-function\[\d+\]:(0x[0-9a-f]+)/g)].map(match => originalPositionFor(trace, { line: 1, column: Number.parseInt(match[1], 16) }));
  const own = frames.find(frame => frame.source === '../../../src/lib.rs');
  assert.ok(own, JSON.stringify({ frames, stack: result.stack }));
  assert.equal(own.line, 10, JSON.stringify(own));
  const wasm = scripts.find(script => script.scriptLanguage === 'WebAssembly' && script.sourceMapURL.endsWith('client_bg.wasm.map'));
  assert.ok(wasm, JSON.stringify(scripts.filter(script => script.scriptLanguage === 'WebAssembly')));
  const fetched = await page.evaluate(async () => (await fetch('/client_bg.wasm.map')).json());
  assert.ok(fetched.sourcesContent.some(source => source?.includes('mapped fixture failure')));
  console.log(JSON.stringify({ chromium: await browser.version(), value: result.value, mappedRustFrame: own, sourceMapURL: wasm.sourceMapURL, sources: map.sources.length, mappings: map.mappings.split(',').length, requests }));
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
";
