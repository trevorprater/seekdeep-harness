//! Source details-ownership assertions driven by the real Rust Agent and replay adapter.

pub(super) const DRIVER: &str = r"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdirSync } from 'node:fs';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2), require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test'), ts = require('typescript'), { tsImport } = require('tsx/esm/api');
const { parseSessionLog } = await tsImport(join(source, 'packages/test-support/llm-replay/src/index.ts'), { parentURL: import.meta.url, tsconfig: join(source, 'tsconfig.base.json') });
const exec = promisify(execFile), home = join(world, 'home'), workspace = join(world, 'workspace');
const SNAPSHOT_DIR = join(source, 'apps/web/tests/snapshots/details-session-lifecycle'), HANDLES_EXPECTED = join(SNAPSHOT_DIR, 'handles.expected.md'), MODE = 'replay';
let server, context, page, cdp, runError, stderr = '', sequence = 0;
const driven = [];
async function sourceText(file) { const body = await readFile(join(source, 'apps/web/tests', file), 'utf8'); return ts.createSourceFile(file, body, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS); }
function declarations(ast, names) {
  const selected = ast.statements.filter(node => {
    const declared = ts.isVariableStatement(node) ? node.declarationList.declarations.map(value => value.name.getText(ast)) : node.name ? [node.name.getText(ast)] : [];
    return declared.some(name => names.includes(name));
  });
  assert.equal(selected.length, names.length, 'source helper inventory');
  return selected.map(node => node.getText(ast).replace(/^export\s+/, '')).join('\n');
}
function compile(code, bindings) {
  const emitted = ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
  return new Function(...Object.keys(bindings), emitted)(...Object.values(bindings));
}
try {
  const sourceAst = await sourceText('details-session-lifecycle.e2e.ts'), cases = [];
  function visit(node) { if (ts.isCallExpression(node) && node.expression.getText(sourceAst) === 'it' && ts.isStringLiteral(node.arguments[0])) cases.push(node.arguments[1].getText(sourceAst)); ts.forEachChild(node, visit); }
  visit(sourceAst); assert.equal(cases.length, 1);
  const details = declarations(sourceAst, ['PROMPT', 'detailsTrack', 'sidebarTrack', 'appFrame', 'handleSnapshot']);
  const helpers = declarations(await sourceText('scaffold.ts'), ['watchConsole', 'acknowledgeReloadConnectionLoss', 'WELCOME_NOTICE_SETTINGS_NAMESPACE', 'WELCOME_NOTICE_ACK_FIELD', 'WELCOME_NOTICE_VERSION']);
  const support = declarations(await sourceText('support.ts'), ['connectFreshWorkspace']);
  const values = compile(helpers + '\n' + details + '\nreturn {PROMPT,WELCOME_NOTICE_SETTINGS_NAMESPACE,WELCOME_NOTICE_ACK_FIELD,WELCOME_NOTICE_VERSION};', { expect });
  const replay = join(source, 'apps/web/tests/snapshots/lifecycle-chrome/session.jsonl');
  const replayEvents = parseSessionLog(await readFile(replay, 'utf8'));
  assert.deepEqual(replayEvents.filter(event => event.type === 'user/message').map(event => event.data.content.filter(block => block.type === 'text').map(block => block.text).join('')), [values.PROMPT]);
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  const seed = join(world, 'seed.jsonl'), overlay = join(world, 'empty.patch.yml');
  await writeFile(seed, await readFile(join(source, 'apps/web/tests/snapshots/seeded-history/seed.jsonl'), 'utf8')); await writeFile(overlay, '[]\n');
  server = spawn(host, [home, workspace, 'details-session-lifecycle-seed', home, 'replay', overlay, seed, replay], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + stderr)), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr)); });
  });
  async function invoke(method, payload) { const rpcId = 'details-' + ++sequence; const response = await fetch(origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) }); assert(response.ok); const body = await response.json(); assert.equal(body.rpcId, rpcId); assert.equal(body.result.ok, true, JSON.stringify(body)); return body.result.value; }
  await invoke('settings.mutate', { ns: values.WELCOME_NOTICE_SETTINGS_NAMESPACE, ops: [{ op: 'set', path: [values.WELCOME_NOTICE_ACK_FIELD], value: values.WELCOME_NOTICE_VERSION }] });
  const seeded = await fetch(origin + '/fixture/seed-log', { method: 'POST' }); assert(seeded.ok, await seeded.text());
  const profile = join(world, 'browser');
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'en-US', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000);
  const api = compile(helpers + '\n' + details + '\n' + support + '\nreturn {watchConsole,appFrame,connectFreshWorkspace};', { expect, mkdirSync, join });
  const tripwire = api.watchConsole(page); await page.goto(origin); await api.appFrame(page).waitFor(); await api.connectFreshWorkspace(page, workspace);
  const scaffold = { workspaceCwd: workspace, whenTurnSettled() {
    const pending = (async () => {
      const response = await page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/session.prompt', { timeout: 30000 });
      assert(response.ok()); const accepted = await response.json(); assert.equal(accepted.result.ok, true, JSON.stringify(accepted));
      const { sessionId } = response.request().postDataJSON().payload; assert.equal(typeof sessionId, 'string');
      const settled = await fetch(origin + '/fixture/idle/' + encodeURIComponent(sessionId), { method: 'POST', signal: AbortSignal.timeout(30000) }); assert(settled.ok, 'idle barrier HTTP ' + settled.status + ': ' + await settled.clone().text());
      const events = await settled.json(); assert.equal(events.filter(event => event.type === 'turn/start').length, 1);
      const ends = events.filter(event => event.type === 'turn/end'); assert.equal(ends.length, 1); assert.equal(ends[0].data.reason.kind, 'completed');
      const messages = events.filter(event => event.type === 'user/message' && event.data.source?.kind === 'user'); assert.equal(messages.length, 1); assert.equal(messages[0].data.content[0].text, values.PROMPT);
      driven.push({ sessionId, events }); return sessionId;
    })(); pending.catch(() => {}); return pending;
  } };
  const hooks = [], compareOrRefreshGolden = async (path, actual, mode) => { assert.equal(mode, 'replay'); await writeFile(join(output, 'handles.actual.md'), actual + '\n'); assert.equal(actual + '\n', await readFile(path, 'utf8')); };
  const callback = compile(helpers + '\n' + details + '\nreturn (' + cases[0] + ');', { expect, page, scaffold, tripwire, MODE, HANDLES_EXPECTED, SNAPSHOT_DIR, compareOrRefreshGolden,
    onTestFailed: hook => hooks.push(hook), saveFailureShot: (target, name) => target.screenshot({ path: join(output, name + '.png'), fullPage: true }),
    assertFixtureInventory: async (path, names) => assert.deepEqual((await readdir(path)).sort(), [...names].sort()),
  });
  try { await callback(); } catch (error) { await Promise.all(hooks.map(hook => hook().catch(() => {}))); throw error; }
  assert.equal(driven.length, 1); assert.deepEqual(tripwire, { warnings: [], pageErrors: [] });
  await exec('agent-browser', ['--session', 'seekdeep-details', '--cdp', cdp, 'screenshot', '--annotate', join(output, 'session-switch-closed.png')]);
  await writeFile(join(output, 'driven-session.json'), JSON.stringify(driven[0], null, 2));
  console.log('details: real replayed turn, source handle golden, sidebar drag, reload, and Session ownership switches passed');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ error: String(error), stderr }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-details', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try { if (server) { if (server.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); } const audit = JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')); assert.equal(audit.calls, 0); assert.equal(audit.replayConsumed, true); } } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'details and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ sourceAssertions: true, replayConsumed: true, nonReplayCalls: 0 }, null, 2));
";
