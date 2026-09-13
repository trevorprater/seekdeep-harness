//! Unchanged source navigation assertions over the built Rust/WASM client and Rust Host.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join, basename } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test'), ts = require('typescript');
const { strFromU8, unzipSync } = require('fflate');
const { tsImport } = require('tsx/esm/api');
const { parseSessionLog } = await tsImport(join(source, 'packages/test-support/llm-replay/src/index.ts'), { parentURL: import.meta.url, tsconfig: join(source, 'tsconfig.base.json') });
const exec = promisify(execFile), checks = [];
const home = join(world, 'home'), workspace = join(world, 'workspace'), profile = join(world, 'browser');
const SNAPSHOT_DIR = join(source, 'apps/web/tests/snapshots/navigation-panes'), SEED_ID = 'navigation-panes-web-e2e', MODE = 'replay';
let server, context, page, cdp, runError, stderr = '', sequence = 0;
function adaptSelectors(code) {
  for (const local of ['copyButton', 'output', 'line', 'runState', 'runStateLabel', 'prompt', 'promptLine', 'cwd']) code = code.replaceAll('[class*="_' + local + '_"]', '[class~="seekdeep-primitive-terminalblock-' + local + '"]');
  return code.replaceAll('dsh-session-', 'seekdeep-session-');
}
async function declarations(file, names) {
  const body = await readFile(join(source, 'apps/web/tests', file), 'utf8');
  const ast = ts.createSourceFile(file, body, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const selected = ast.statements.filter(node => {
    const declared = ts.isVariableStatement(node) ? node.declarationList.declarations.map(value => value.name.getText(ast)) : node.name ? [node.name.getText(ast)] : [];
    return declared.some(name => names.includes(name));
  });
  assert.equal(selected.length, names.length, 'source helper inventory');
  return selected.map(node => node.getText(ast).replace(/^export\s+/, '')).join('\n');
}
function compile(code, bindings) {
  const emitted = ts.transpileModule(adaptSelectors(code), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
  return new Function(...Object.keys(bindings), emitted)(...Object.values(bindings));
}
async function sourceCases() {
  const body = await readFile(join(source, 'apps/web/tests/navigation-panes.e2e.ts'), 'utf8');
  const ast = ts.createSourceFile('navigation.e2e.ts', body, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS), cases = [];
  function visit(node) {
    if (ts.isCallExpression(node) && node.expression.getText(ast).startsWith('it.') && ts.isStringLiteral(node.arguments[0]) && ts.isArrowFunction(node.arguments[1])) cases.push({ name: node.arguments[0].text, callback: node.arguments[1].getText(ast) });
    ts.forEachChild(node, visit);
  }
  visit(ast); assert.equal(cases.length, 8, 'source navigation scenario inventory');
  assert.equal(cases.filter(value => value.name.startsWith('records the two-turn seed')).length, 1);
  return cases.filter(value => !value.name.startsWith('records the two-turn seed'));
}
try {
  const helpers = await declarations('scaffold.ts', ['normalizeAria', 'captureStableAria', 'watchConsole', 'WELCOME_NOTICE_SETTINGS_NAMESPACE', 'WELCOME_NOTICE_ACK_FIELD', 'WELCOME_NOTICE_VERSION']);
  const navigation = await declarations('navigation-panes.e2e.ts', ['baselineResponse', 'assertBaselineSucceeded', 'ensureSeedOpen', 'PROMPT_TURN1', 'PROMPT_TURN2']);
  const newPage = await declarations('support.ts', ['newEnglishPage']);
  const cases = await sourceCases();
  await mkdir(home, { recursive: true }); await mkdir(join(workspace, 'workspace'), { recursive: true });
  await writeFile(join(workspace, 'workspace/nav-a.md'), '# alpha nav\n'); await writeFile(join(workspace, 'workspace/nav-b.md'), '# beta nav\n');
  const raw = await readFile(join(SNAPSHOT_DIR, 'seed.jsonl'), 'utf8'), seed = join(world, 'navigation-seed.jsonl'), overlay = join(world, 'empty.patch.yml');
  await writeFile(seed, raw); await writeFile(overlay, '[]\n');
  server = spawn(host, [home, workspace, SEED_ID, home, 'route-only', overlay, seed], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + stderr)), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr)); });
  });
  const invoke = async (method, payload) => { const rpcId = 'navigation-' + ++sequence; const response = await fetch(origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) }); assert(response.ok); const body = await response.json(); assert.equal(body.rpcId, rpcId); assert.equal(body.result.ok, true, JSON.stringify(body)); return body.result.value; };
  const sourceValues = compile(helpers + '\n' + navigation + '\nreturn {WELCOME_NOTICE_SETTINGS_NAMESPACE,WELCOME_NOTICE_ACK_FIELD,WELCOME_NOTICE_VERSION,PROMPT_TURN1,PROMPT_TURN2};', { expect });
  const prompts = parseSessionLog(raw).filter(event => event.type === 'user/message').map(event => event.data.content.filter(block => block.type === 'text').map(block => block.text).join(''));
  assert.deepEqual(prompts, [sourceValues.PROMPT_TURN1, sourceValues.PROMPT_TURN2]);
  await invoke('settings.mutate', { ns: sourceValues.WELCOME_NOTICE_SETTINGS_NAMESPACE, ops: [{ op: 'set', path: [sourceValues.WELCOME_NOTICE_ACK_FIELD], value: sourceValues.WELCOME_NOTICE_VERSION }] });
  const seeded = await fetch(origin + '/fixture/seed-log', { method: 'POST' }); assert(seeded.ok, await seeded.text());
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'en-US', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  for (const initial of context.pages()) await initial.close();
  const browser = context.browser();
  for (const scenario of cases) {
    if (process.env.SEEKDEEP_NAVIGATION_CASE && !scenario.name.includes(process.env.SEEKDEEP_NAVIGATION_CASE)) continue;
    page = await browser.newPage({ viewport: { width: 1680, height: 1000 }, locale: 'en-US' }); page.setDefaultTimeout(15000);
    const failureHooks = [], slotErrors = [], allErrors = [];
    page.on('console', message => { if (message.type() === 'error') allErrors.push(message.text()); if (message.type() === 'error' && /slot entry crashed/i.test(message.text())) slotErrors.push(message.text()); });
    const api = compile(helpers + '\n' + navigation + '\nreturn {watchConsole,baselineResponse,assertBaselineSucceeded};', { expect });
    const tripwire = api.watchConsole(page), sessionBaseline = api.baselineResponse(page, 'session.list'), workspaceBaseline = api.baselineResponse(page, 'workspace.list');
    const [, sessionResponse, workspaceResponse] = await Promise.all([page.goto(origin), sessionBaseline, workspaceBaseline]);
    await api.assertBaselineSucceeded(sessionResponse, 'session.list'); await api.assertBaselineSucceeded(workspaceResponse, 'workspace.list');
    await page.getByText('Ungrouped', { exact: true }).waitFor();
    const compareOrRefreshGolden = async (path, actual, mode) => { assert.equal(mode, 'replay'); await writeFile(join(output, basename(path) + '.actual'), actual.trimEnd() + '\n'); assert.equal(actual.trimEnd() + '\n', await readFile(path, 'utf8')); };
    const assertFixtureInventory = async (path, names) => assert.deepEqual((await readdir(path)).sort(), [...names].sort());
    const saveFailureShot = async (target, name) => target.screenshot({ path: join(output, name + '.png'), fullPage: true });
    const callback = compile(helpers + '\n' + navigation + '\n' + newPage + '\nreturn (' + scenario.callback + ');', {
      expect, page, browser, scaffold: { workspaceCwd: workspace, baseUrl: origin }, tripwire, slotErrors, MODE, SEED_ID, SNAPSHOT_DIR,
      TRAJECTORY_EXPECTED: join(SNAPSHOT_DIR, 'trajectory.expected.md'), SEARCH_EXPECTED: join(SNAPSHOT_DIR, 'search-results.expected.md'), TERMINAL_EXPECTED: join(SNAPSHOT_DIR, 'terminal-card.expected.md'),
      compareOrRefreshGolden, assertFixtureInventory, saveFailureShot, onTestFailed: hook => failureHooks.push(hook), readFile, strFromU8, unzipSync, parseSessionLog,
    });
    try {
      await callback(); assert.deepEqual(tripwire, { warnings: [], pageErrors: [] }); assert.deepEqual(slotErrors, []); assert.deepEqual(allErrors, []);
      await exec('agent-browser', ['--session', 'seekdeep-navigation', '--cdp', cdp, 'screenshot', '--annotate', join(output, 'case-' + checks.length + '.png')]);
      checks.push(scenario.name); console.log('navigation: ' + scenario.name);
    } catch (error) {
      await Promise.all(failureHooks.map(hook => hook().catch(() => {})));
      await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot());
      await writeFile(join(output, 'browser-errors.json'), JSON.stringify({ scenario: scenario.name, tripwire, slotErrors, allErrors }, null, 2)); throw error;
    } finally { await page.close(); page = undefined; }
  }
  assert.equal(checks.length, process.env.SEEKDEEP_NAVIGATION_CASE ? 1 : 7, 'all selected source cases must run');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  await writeFile(join(output, 'failures.json'), JSON.stringify({ error: String(error), checks, stderr }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-navigation', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try { if (server) { if (server.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); } assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0); } } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'navigation and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ checks, modelCalls: 0 }, null, 2));
"#;
