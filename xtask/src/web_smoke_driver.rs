//! Pinned CLI and real-model browser smoke cases through the built Rust application.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { spawn as nativeSpawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync as nativeReadFileSync, rmSync, writeFileSync } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
const [source, fixtureHost, world, output] = process.argv.slice(2), root = process.cwd();
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect: baseExpect } = require('playwright/test'), ts = require('typescript');
const live = process.env.SEEKDEEP_WEB_SMOKE_LIVE === '1';
const key = process.env.DEEPSEEK_API_KEY;
assert(!live || key, 'web-smoke --live requires DEEPSEEK_API_KEY');
const redact = text => key ? String(text).replaceAll(key, '[REDACTED]') : String(text);
const executable = resolve(dirname(fixtureHost), '../seekdeep');
assert(existsSync(executable), 'build the Rust CLI before web-smoke');
assert(existsSync(join(root, 'apps/cli/lib/bin.js')), 'build the npm launcher before web-smoke');
const golden = join(source, 'apps/web/tests/snapshots/web-runtime-context/web-surface-prompt.expected.md');
const adapt = text => text.replaceAll('DeepSeek Harness', 'SeekDeep Harness').replaceAll('dsh web', 'seekdeep web').replaceAll('DSH_', 'SEEKDEEP_');
const readFileSync = (path, ...args) => {
  const value = nativeReadFileSync(path, ...args);
  return path === golden ? adapt(value) : value;
};
function inlineSnapshot(value, indent = '') {
  const inner = indent + '  ';
  if (Array.isArray(value)) return value.length === 0 ? '[]' : '[\n' + value.map(item => inner + inlineSnapshot(item, inner) + ',\n').join('') + indent + ']';
  if (value !== null && typeof value === 'object') { const keys = Object.keys(value).sort(); return keys.length === 0 ? '{}' : '{\n' + keys.map(key => inner + '"' + key + '": ' + inlineSnapshot(value[key], inner) + ',\n').join('') + indent + '}'; }
  return typeof value === 'string' ? '"' + value + '"' : String(value);
}
function dedent(text) {
  const lines = text.split('\n');
  if (lines[0].trim() === '') lines.shift();
  if (lines.length && lines[lines.length - 1].trim() === '') lines.pop();
  const width = Math.min(...lines.filter(line => line.trim()).map(line => line.match(/^\s*/)[0].length));
  return lines.map(line => line.slice(width)).join('\n');
}
baseExpect.extend({ toMatchInlineSnapshot(value, expected) {
  const actual = inlineSnapshot(value), wanted = dedent(expected);
  return {pass: actual === wanted, message: () => 'inline snapshot mismatch\n' + actual + '\nexpected:\n' + wanted};
} });
const expect = new Proxy(baseExpect, { get(target, key) { return key === 'poll' ? (probe, options) => target.poll(probe, options?.interval === undefined ? options : {...options, intervals: [options.interval]}) : Reflect.get(target, key); } });
const children = [], browsers = new Set();
function spawn(command, argv, options) {
  assert.equal(command, process.execPath);
  assert.equal(argv[0], join(root, 'apps/cli/lib/bin.js'));
  const env = {...options.env, SEEKDEEP_EXECUTABLE: executable, SEEKDEEP_TELEMETRY_DISABLED: '1', SEEKDEEP_BUNDLED_SKILL_DIR: join(options.cwd, '.bundled-skills')};
  const child = nativeSpawn(command, argv, {...options, env, detached: process.platform !== 'win32'});
  const owned = {child, output: '', closed: false};
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { owned.output += chunk.toString(); });
  owned.done = new Promise(resolveDone => { child.once('close', () => { owned.closed = true; resolveDone(); }); });
  child.kill = signal => {
    if (owned.closed) return false;
    if (process.platform === 'win32') {
      if (!owned.killer) {
        const killer = nativeSpawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], {stdio: 'ignore'});
        owned.killer = new Promise(resolveKill => {
          killer.once('error', error => resolveKill(error));
          killer.once('close', code => resolveKill(code === 0 || owned.closed ? undefined : new Error('taskkill failed: ' + code)));
        });
      }
      return true;
    }
    try { process.kill(-child.pid, signal ?? 'SIGTERM'); return true; }
    catch (error) { if (error.code === 'ESRCH') return false; throw error; }
  };
  children.push(owned);
  return child;
}
const observedChromium = {launch: async (...args) => {
  const browser = await chromium.launch(...args); browsers.add(browser);
  const close = browser.close.bind(browser);
  browser.close = async () => { try { await close(); } finally { browsers.delete(browser); } };
  return browser;
} };
const suites = [], caseResults = [];
let current, failureHooks = [];
function describe(name, body, skipped = false) {
  const suite = {name, skipped, cases: [], before: [], after: []};
  suites.push(suite); const parent = current; current = suite;
  try { body(); } finally { current = parent; }
}
describe.skipIf = skip => (name, body) => describe(name, body, skip);
const it = (name, body, timeout = 180000) => current.cases.push({name, body, timeout});
const beforeAll = (body, timeout = 120000) => current.before.push({body, timeout});
const afterAll = (body, timeout = 120000) => current.after.push({body, timeout});
const onTestFailed = callback => failureHooks.push(callback);
async function deadline(body, timeout, label) {
  let timer;
  try { return await Promise.race([Promise.resolve().then(body), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(label + ' timed out after ' + timeout + 'ms')), timeout); })]); }
  finally { clearTimeout(timer); }
}
const requireDist = () => assert(existsSync(join(root, 'apps/web/dist/index.html')), 'build the Web frontend before web-smoke');
const saveFailureShot = (page, name) => page.screenshot({path: join(output, name + '.png'), fullPage: true});
const sourceFile = join(source, 'apps/web/tests/smoke-real.e2e.ts');
const ast = ts.createSourceFile(sourceFile, await readFile(sourceFile, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
let program = ast.statements.filter(node => !ts.isImportDeclaration(node)).map(node => node.getText(ast)).join('\n');
const launchers = program.match(/const tsxLoader = [^\n]+\n/g) ?? [];
assert.equal(launchers.length, 5, 'source CLI launcher inventory');
program = program.replace(/const tsxLoader = [^\n]+\n/g, '')
  .replaceAll("'--import', tsxLoader, ", '')
  .replaceAll("'apps/cli/src/bin.ts'", "'apps/cli/lib/bin.js'")
  .replaceAll("fileURLToPath(new URL('./pin-browse-picker.overlay.yml', import.meta.url))", "join(REPO_ROOT, 'apps/web/tests/pin-browse-picker.overlay.yml')")
  .replaceAll('import.meta.url', JSON.stringify(pathToFileURL(sourceFile).href));
// The connection's Rust factory exposes clientConnectionPlugin instead of the TS exports.apply assignment.
assert.equal(program.split("!readFileSync(bundle, 'utf8').includes('exports.apply')").length, 2);
program = program.replace("!readFileSync(bundle, 'utf8').includes('exports.apply')", "!(readFileSync(bundle, 'utf8').includes('exports.apply') || (dir === 'connection' && readFileSync(bundle, 'utf8').includes('clientConnectionPlugin'))) ");
// The title provider can reach the mock endpoint before the main code-mode request.
const capture = 'resolveProviderRequest(JSON.parse(body) as CodeModeProviderRequest)';
assert.equal(program.split(capture).length, 2, 'source code-mode request capture inventory');
program = program.replace(capture, `const parsed = JSON.parse(body) as CodeModeProviderRequest & { max_tokens?: number }
        if (parsed.max_tokens !== 64) resolveProviderRequest(parsed)`);
const retryHistory = 'let page: HistoryPage | undefined';
assert.equal(program.split(retryHistory).length, 2, 'source retry history inventory');
program = program.replace(retryHistory, `${retryHistory}
      onTestFailed(() => writeFile(join(output, 'retry-history.json'), redact(JSON.stringify({mainAttempts, page}, null, 2)) + '\\n'))`);
const supportPath = join(source, 'apps/web/tests/support.ts');
const support = ts.createSourceFile(supportPath, await readFile(supportPath, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const names = ['connectFreshWorkspace', 'newEnglishPage', 'probeFreePort'];
const helpers = support.statements.filter(node => ts.isFunctionDeclaration(node) && names.includes(node.name?.text));
assert.equal(helpers.length, names.length, 'source browser helper inventory');
const prefix = helpers.map(node => node.getText(support).replace(/^export\s+/, '')
  .replace('function connectFreshWorkspace(', 'function connectFreshWorkspaceSource(')).join('\n') + `
async function connectFreshWorkspace(page, cwd) {
  const notice = page.getByRole('dialog', {name: 'Internal Testing Notice'});
  await notice.getByRole('button', {name: 'Continue', exact: true}).click();
  await notice.waitFor({state: 'hidden'});
  await connectFreshWorkspaceSource(page, cwd);
}
`;
const bindings = {spawn, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, writeFile, createServer, createRequire, tmpdir, join, fileURLToPath, pathToFileURL, chromium: observedChromium, expect, describe, it, beforeAll, afterAll, onTestFailed, REPO_ROOT: root, requireDist, saveFailureShot, output, redact};
const emitted = ts.transpileModule(adapt(prefix + '\n' + program), {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext}}).outputText;
new Function(...Object.keys(bindings), emitted)(...Object.values(bindings));
assert.equal(suites.length, 2);
assert.deepEqual(suites.map(suite => suite.cases.length), [4, 8]);
mkdirSync(join(root, '.artifacts'), {recursive: true});
const failures = [];
try {
  for (const [index, suite] of suites.entries()) {
    if (index === 1 && !live) continue;
    assert(!suite.skipped, 'requested smoke suite skipped: ' + suite.name);
    try {
      for (const hook of suite.before) await deadline(hook.body, hook.timeout, suite.name + ' setup');
      for (const test of suite.cases) {
        failureHooks = [];
        try {
          await deadline(test.body, test.timeout, test.name);
          caseResults.push(test.name); console.log('web-smoke: ' + test.name);
        } catch (error) {
          for (const hook of failureHooks) await Promise.resolve().then(hook).catch(() => {});
          throw error;
        }
      }
    } catch (error) { failures.push(error); }
    finally { for (const hook of suite.after) await deadline(hook.body, hook.timeout, suite.name + ' teardown').catch(error => failures.push(error)); }
  }
} finally {
  for (const browser of browsers) await browser.close().catch(error => failures.push(error));
  for (const [index, owned] of children.entries()) {
    if (!owned.closed) {
      owned.child.kill('SIGTERM');
      const escalate = setTimeout(() => owned.child.kill('SIGKILL'), 5000);
      await deadline(() => owned.done, 15000, 'CLI process teardown').catch(error => failures.push(error));
      clearTimeout(escalate);
    }
    if (owned.killer) {
      const error = await deadline(() => owned.killer, 15000, 'process-tree teardown').catch(error => error);
      if (error) failures.push(error);
    }
    await writeFile(join(output, 'host-' + index + '.log'), redact(owned.output));
  }
}
await writeFile(join(output, 'result.json'), JSON.stringify({live, cases: caseResults, failures: failures.map(error => redact(error.stack ?? error))}, null, 2) + '\n');
if (failures.length) throw new AggregateError(failures.map(error => new Error(redact(error.stack ?? error))), 'source Web smoke failed');
assert.equal(caseResults.length, live ? 12 : 4);
"#;
