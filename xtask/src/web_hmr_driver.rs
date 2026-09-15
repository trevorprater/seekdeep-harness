//! Source browser HMR assertions over the real development watcher and built npm launcher.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import { readFile, writeFile, readdir, mkdir, rm } from 'node:fs/promises';
import { join, dirname, resolve } from 'node:path';
const [source, fixtureHost, world, output] = process.argv.slice(2);
const root = process.cwd(), require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test'), ts = require('typescript');
const sourcePath = join(root, 'crates/client-ui-conversation/data/conversation-locales.json');
const library = join(root, 'packages/client/ui-conversation/lib');
const executable = resolve(dirname(fixtureHost), '../seekdeep');
const originalSource = await readFile(sourcePath);
const oldText = 'Into the Unknown', newText = `HMR UPDATED ${'x'.repeat(80)}`;
const needle = '"hero.headline": "' + oldText + '"';
assert.equal(originalSource.toString().split(needle).length, 2, 'one English headline input');
const updatedSource = originalSource.toString().replace(needle, '"hero.headline": "' + newText + '"');

// Rust owns the locale data through include_str!, so the same edit rebuilds WebAssembly.
// The source's browser statements, including its 30-second update deadline, run unchanged.
const sourceText = await readFile(join(source, 'apps/web/tests/hmr-live.e2e.ts'), 'utf8');
const ast = ts.createSourceFile('hmr-live.e2e.ts', sourceText, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const cases = ast.statements.filter(node => ts.isExpressionStatement(node) && ts.isCallExpression(node.expression) && node.expression.expression.getText(ast) === 'it');
assert.equal(cases.length, 1, 'source HMR case inventory');
const body = cases[0].expression.arguments[1].body;
const sourceTry = body.statements.find(ts.isTryStatement);
assert(sourceTry, 'source HMR lifecycle block');
const browserIndex = sourceTry.tryBlock.statements.findIndex(node => node.getText(ast).startsWith('browser = await chromium.launch()'));
assert(browserIndex >= 0, 'source browser assertions');
const browserSource = sourceTry.tryBlock.statements.slice(browserIndex).map(node => node.getText(ast)).join('\n');
assert(browserSource.includes('expect(pageErrors).toEqual([])'));
assert(browserSource.includes('.__dshHmrPageIdentity'));
const browserJavaScript = ts.transpileModule(browserSource, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;

async function files(directory) {
  const entries = [];
  for (const item of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, item.name);
    if (item.isDirectory()) entries.push(...await files(path));
    else if (item.isFile()) entries.push(path);
    else throw new Error('unexpected Client artifact type: ' + path);
  }
  return entries;
}
// Every emitted sidecar is restored after the watcher stops, including WebAssembly and maps.
const originalArtifacts = new Map(await Promise.all((await files(library)).map(async path => [path, await readFile(path)])));
const diagnostics = { console: [], responses: [], failedRequests: [], events: [] };
const observedChromium = { launch: async () => {
  const browser = await chromium.launch();
  const newPage = browser.newPage.bind(browser);
  browser.newPage = async (...args) => {
    const page = await newPage(...args);
    page.on('console', message => diagnostics.console.push({ type: message.type(), text: message.text() }));
    page.on('response', response => { if (response.url().includes('/plugins/')) diagnostics.responses.push({ url: response.url(), status: response.status() }); });
    page.on('requestfailed', request => diagnostics.failedRequests.push({ url: request.url(), failure: request.failure() }));
    const session = await page.context().newCDPSession(page);
    await session.send('Network.enable');
    session.on('Network.eventSourceMessageReceived', event => diagnostics.events.push(event));
    return page;
  };
  return browser;
} };
const children = [];
function start(label, argv, cwd, overrides = {}) {
  const env = { ...process.env, SEEKDEEP_TELEMETRY_DISABLED: '1', ...overrides };
  delete env.DEEPSEEK_API_KEY;
  if (overrides.DEEPSEEK_API_KEY !== undefined) env.DEEPSEEK_API_KEY = overrides.DEEPSEEK_API_KEY;
  const child = spawn(argv[0], argv.slice(1), { cwd, env, detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'] });
  const owned = { child, label, output: '', ended: false, status: undefined, listeners: new Set() };
  const changed = () => { for (const listener of owned.listeners) listener(); };
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => {
    const text = chunk.toString(); owned.output += text; changed();
    if (label === 'dev-web' && text.includes('built @')) console.log('web-hmr: ' + text.trim());
  });
  owned.done = new Promise(resolveDone => {
    child.once('error', error => { owned.ended = true; owned.status = String(error); changed(); resolveDone(); });
    child.once('close', (code, signal) => { owned.ended = true; owned.status = { code, signal }; changed(); resolveDone(); });
  });
  children.push(owned);
  return owned;
}
function ready(owned, pattern, timeoutMs) {
  return new Promise((resolveReady, reject) => {
    const finish = (error, match) => { clearTimeout(timer); owned.listeners.delete(check); if (error) reject(error); else resolveReady(match[1] ?? match[0]); };
    const check = () => {
      const match = pattern.exec(owned.output);
      if (match) finish(undefined, match);
      else if (owned.ended) finish(new Error(owned.label + ' exited before ready: ' + JSON.stringify(owned.status) + '\n' + owned.output.slice(-8000)));
    };
    const timer = setTimeout(() => finish(new Error(owned.label + ' readiness timeout\n' + owned.output.slice(-8000))), timeoutMs);
    owned.listeners.add(check); check();
  });
}
function signal(owned, signal) {
  if (owned.ended) return;
  try {
    if (process.platform === 'win32') owned.child.kill(signal);
    else process.kill(-owned.child.pid, signal);
  } catch (error) { if (error.code !== 'ESRCH') throw error; }
}
async function stop(owned) {
  if (owned.ended) return;
  if (process.platform === 'win32') {
    await new Promise((resolveKill, reject) => {
      const killer = spawn('taskkill', ['/PID', String(owned.child.pid), '/T', '/F'], { stdio: 'ignore' });
      killer.once('error', reject);
      killer.once('close', code => code === 0 || owned.ended ? resolveKill() : reject(new Error('taskkill failed for ' + owned.label + ': ' + code)));
    });
  } else {
    signal(owned, 'SIGTERM');
  }
  const escalate = setTimeout(() => signal(owned, 'SIGKILL'), 5000);
  let deadline;
  try {
    await Promise.race([owned.done, new Promise((_, reject) => { deadline = setTimeout(() => reject(new Error(owned.label + ' did not stop after escalation')), 15000); })]);
  } finally { clearTimeout(escalate); clearTimeout(deadline); }
}
const failures = [];
let watcher, host;
try {
  // The initial Rust build has a separate compilation allowance; browser deadlines stay pinned.
  watcher = start('dev-web', ['pnpm', 'run', 'dev:web'], root);
  await ready(watcher, /dev-web: watching/, 600000);
  console.log('web-hmr: real development watcher ready');
  const home = join(world, 'home');
  await mkdir(home, { recursive: true });
  host = start('built npm launcher', [process.execPath, join(root, 'apps/cli/lib/bin.js'), 'web', '--port', '0'], world, {
    SEEKDEEP_EXECUTABLE: executable,
    DEEPSEEK_API_KEY: 'keyless-hmr-no-call',
    SEEKDEEP_HOME: home,
    SEEKDEEP_AGENTS_HOME: join(world, 'agents'),
    SEEKDEEP_BUNDLED_SKILL_DIR: join(world, 'bundled-skills'),
  });
  const baseUrl = await ready(host, /seekdeep web: (http:\/\/[^\s]+)/, 60000);
  const run = new Function('chromium', 'expect', 'baseUrl', 'oldText', 'newText', 'writeFile', 'sourcePath', 'updatedSource', 'output',
    'return (async () => { let browser; try {\n' + browserJavaScript + '\nawait page.screenshot({path: output + "/updated.png", fullPage: true}); return {pageIdentity, pageErrors};\n' +
    '} catch (error) { await browser?.contexts()[0]?.pages()[0]?.screenshot({path: output + "/failure.png", fullPage: true}).catch(() => {}); throw error; } finally { await browser?.close(); } })();');
  let editStarted;
  const writeInput = (...args) => { editStarted = Date.now(); console.log('web-hmr: locale edit written at ' + new Date(editStarted).toISOString()); return writeFile(...args); };
  const result = await run(observedChromium, expect, baseUrl, oldText, newText, writeInput, sourcePath, updatedSource, output);
  const editToVisibleMs = Date.now() - editStarted;
  const bundle = await readFile(join(library, 'client.js'));
  assert(!bundle.equals(originalArtifacts.get(join(library, 'client.js'))), 'Client bundle was rebuilt');
  await writeFile(join(output, 'result.json'), JSON.stringify({ sourceCase: cases[0].expression.arguments[0].text, ...result, bundleChanged: true, editToVisibleMs }, null, 2) + '\n');
  console.log('web-hmr: source headline, page identity, and browser-error assertions passed');
} catch (error) {
  failures.push(error);
} finally {
  await writeFile(sourcePath, originalSource).catch(error => failures.push(error));
  if (watcher) await stop(watcher).catch(error => failures.push(error));
  if (host) await stop(host).catch(error => failures.push(error));
  for (const path of await files(library)) if (!originalArtifacts.has(path)) await rm(path).catch(error => failures.push(error));
  for (const [path, bytes] of originalArtifacts) await writeFile(path, bytes).catch(error => failures.push(error));
  assert((await readFile(sourcePath)).equals(originalSource), 'locale input restored');
  for (const [path, bytes] of originalArtifacts) assert((await readFile(path)).equals(bytes), 'Client artifact restored: ' + path);
  for (const child of children) await writeFile(join(output, child.label.replaceAll(' ', '-') + '.log'), child.output).catch(error => failures.push(error));
  await writeFile(join(output, 'diagnostics.json'), JSON.stringify(diagnostics, null, 2) + '\n').catch(error => failures.push(error));
  console.log('web-hmr: source and all Client sidecars restored; owned processes stopped');
}
if (failures.length) throw new AggregateError(failures, 'browser HMR or cleanup failed');
"#;
