//! Responsive scrollbar behavior through source measurements and the real Rust Host.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const focus = process.env.SEEKDEEP_SCROLLBARS_FOCUS ?? 'all';
assert(['all', 'sidebar'].includes(focus), 'unknown scrollbar test focus');
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test'), ts = require('typescript');
const exec = promisify(execFile), errors = [], warnings = [], checks = [];
const home = join(world, 'home'), workspace = join(world, 'workspace'), profile = join(world, 'browser');
let server, context, page, cdp, runError, stderr = '';
const rename = value => value.replaceAll('--dsh-', '--seekdeep-');
async function sourceHelpers(file, names) {
  const body = await readFile(join(source, 'apps/web/tests', file), 'utf8');
  const ast = ts.createSourceFile(file, body, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const selected = ast.statements.filter(node => {
    const declared = ts.isVariableStatement(node) ? node.declarationList.declarations.map(value => value.name.getText(ast)) : node.name ? [node.name.getText(ast)] : [];
    return declared.some(name => names.includes(name));
  });
  assert.equal(selected.length, names.length, 'source measurement declaration inventory');
  const code = ts.transpileModule(rename(selected.map(node => node.getText(ast).replace(/^export\s+/, '')).join('\n')), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
  return new Function('expect', code + '\nreturn { ' + names.join(',') + ' };')(expect);
}
async function checked(name) { assert.deepEqual(errors, []); assert.deepEqual(warnings, []); checks.push(name); console.log('scrollbars: ' + name); }
async function screenshot(name) { await exec('agent-browser', ['--session', 'seekdeep-scrollbars', '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '.png')]); }
async function golden(folder, actual) {
  const directory = join(source, 'apps/web/tests/snapshots', folder);
  assert.deepEqual((await readdir(directory)).sort(), ['geometry.expected.md']);
  await writeFile(join(output, folder + '.actual.md'), actual + '\n');
  assert.equal(actual + '\n', rename(await readFile(join(directory, 'geometry.expected.md'), 'utf8')));
}
try {
  const column = await sourceHelpers('conversation-column-overflow.e2e.ts', ['CONTROL_VIEWPORT', 'WIDTHS', 'CONTROL_STYLE_ID', 'WHEEL_DELTA', 'measureColumn', 'wheelHorizontally', 'horizontalScrollLimit', 'renderGeometry']);
  const sidebar = await sourceHelpers('sidebar-scrollbar.e2e.ts', ['SEED_COUNT', 'NO_THUMB', 'measureList', 'measureRowInset', 'measurePalette', 'renderGeometry', 'resolveThumb', 'pointAt', 'expandSeededSessions']);
  const welcome = await sourceHelpers('scaffold.ts', ['WELCOME_NOTICE_SETTINGS_NAMESPACE', 'WELCOME_NOTICE_ACK_FIELD', 'WELCOME_NOTICE_VERSION']);
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  const seed = join(world, 'sidebar-seed.jsonl'), overlay = join(world, 'empty.patch.yml');
  await writeFile(seed, await readFile(join(source, 'apps/web/tests/snapshots/seeded-history/seed.jsonl'), 'utf8')); await writeFile(overlay, '[]\n');
  server = spawn(host, [home, workspace, 'sidebar-scrollbar-web-e2e-00', home, 'route-only', overlay, seed], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + stderr)), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr)); });
  });
  const method = 'settings.mutate', rpcId = 'scrollbars-welcome-precondition';
  const acknowledged = await fetch(origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload: { ns: welcome.WELCOME_NOTICE_SETTINGS_NAMESPACE, ops: [{ op: 'set', path: [welcome.WELCOME_NOTICE_ACK_FIELD], value: welcome.WELCOME_NOTICE_VERSION }] } }) });
  assert(acknowledged.ok); const acknowledgement = await acknowledged.json(); assert.equal(acknowledgement.rpcId, rpcId); assert.equal(acknowledgement.result.ok, true, JSON.stringify(acknowledgement));
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'en-US', viewport: { width: 1680, height: 900 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000);
  const watch = page => { page.on('pageerror', error => errors.push(String(error))); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text()); }); };
  watch(page); await page.goto(origin);
  if (focus === 'all') {
  await page.locator('[data-conversation-scroll] [class*="heroGlow"]').waitFor();
  async function settleAt(width) {
    await page.setViewportSize({ width, height: 900 }); let previous = -1;
    await expect.poll(async () => { const current = (await column.measureColumn(page, width)).columnWidth; const settled = current === previous; previous = current; return settled; }, { timeout: 10000 }).toBe(true);
    return column.measureColumn(page, width);
  }
  const stops = [];
  for (const width of column.WIDTHS) stops.push({ ...await settleAt(width), scrollLeftAfterWheel: await column.wheelHorizontally(page) });
  assert.deepEqual(stops.filter(value => value.glowBleeds).map(value => value.width), [1200, 1000, 800, column.CONTROL_VIEWPORT]);
  for (const stop of stops) { if (stop.glowBleeds) assert(stop.bleedRange > 0); assert.equal(stop.overflowX, 'hidden'); assert.equal(stop.scrollLeftAfterWheel, 0); assert.equal(stop.scrollsVertically, true); }
  await screenshot('narrow-column'); await checked('five-width sweep retains decorative bleed and vertical scrolling without horizontal wheel movement');
  await page.evaluate(id => { const sheet = document.createElement('style'); sheet.id = id; sheet.textContent = '[data-conversation-scroll] { overflow-x: auto !important; }'; document.head.append(sheet); }, column.CONTROL_STYLE_ID);
  try {
    const before = await settleAt(column.CONTROL_VIEWPORT); assert.equal(before.overflowX, 'auto'); assert(before.bleedRange > 0);
    const limit = await column.horizontalScrollLimit(page); assert(limit > 0 && limit < column.WHEEL_DELTA);
    assert.equal(Math.round(await column.wheelHorizontally(page)), Math.round(limit));
  } finally { await page.evaluate(id => document.getElementById(id)?.remove(), column.CONTROL_STYLE_ID); }
  assert.equal((await settleAt(column.CONTROL_VIEWPORT)).overflowX, 'hidden');
  await golden('conversation-column-overflow', column.renderGeometry(stops));
  await checked('horizontal-wheel mutation control reaches the positive boundary and the source golden matches exactly');
  }

  await page.close();
  for (let index = 0; index < sidebar.SEED_COUNT; index++) {
    const response = await fetch(origin + '/fixture/seed-log/sidebar-scrollbar-web-e2e-' + String(index).padStart(2, '0'), { method: 'POST' });
    assert(response.ok, await response.text());
  }
  page = await context.browser().newPage({ viewport: { width: 1680, height: 800 }, locale: 'en-US' }); page.setDefaultTimeout(15000); watch(page);
  await page.goto(origin); await sidebar.expandSeededSessions(page); await sidebar.pointAt(page, 'list');
  await expect.poll(async () => (await sidebar.measureList(page)).overflows).toBe(true);
  const metrics = await sidebar.measureList(page);
  assert.equal(metrics.gutter, 'stable'); assert(metrics.band > 0); assert.equal(metrics.scrollbarEdgeOffset, 2); assert.equal(metrics.rowEdgeInset, 12);
  assert.equal(metrics.timeCoveredBy, 0); assert(metrics.timeRight <= metrics.clientRight); assert(metrics.clientRight < metrics.borderRight);
  await checked('24 real cold sessions overflow with the source gutter, edge insets, and unobscured relative times');
  const revealed = await sidebar.resolveThumb(page); assert.notEqual(revealed, sidebar.NO_THUMB);
  await sidebar.pointAt(page, 'away'); assert.equal(await sidebar.resolveThumb(page), revealed);
  await expect.poll(() => sidebar.resolveThumb(page), { timeout: 10000 }).toBe(sidebar.NO_THUMB);
  const quiet = await sidebar.measureList(page); assert.equal(quiet.gutter, 'stable'); assert(quiet.band > 0); assert.equal(quiet.timeCoveredBy, 0);
  await page.getByRole('tree', { name: 'Sessions', exact: true }).evaluate(node => { node.scrollTop += 200; });
  await page.waitForTimeout(500); assert.equal(await sidebar.resolveThumb(page), sidebar.NO_THUMB);
  await sidebar.pointAt(page, 'list'); await expect.poll(() => sidebar.resolveThumb(page)).toBe(revealed);
  await checked('pointer reveal, leave linger, and pointer-free scrolling preserve source thumb visibility');
  assert.deepEqual(await sidebar.measureRowInset(page), { overflows: true, rowEdgeInset: 12 });
  await page.getByText('Ungrouped', { exact: true }).locator('..').locator('..').click();
  try { await expect.poll(async () => (await sidebar.measureRowInset(page)).overflows).toBe(false); assert.deepEqual(await sidebar.measureRowInset(page), { overflows: false, rowEdgeInset: 12 }); }
  finally { await sidebar.expandSeededSessions(page); }
  await checked('collapsing the group removes overflow without moving row backgrounds');
  await sidebar.pointAt(page, 'list'); const light = await sidebar.measureList(page);
  assert.equal(light.standardWidth, 'auto'); assert.equal(light.standardColor, 'auto'); assert.equal(light.width, '8px'); assert.equal(light.track, sidebar.NO_THUMB);
  assert.deepEqual(light.hoverRules, ['var(--seekdeep-scrollbar-thumb-hover)']); assert.match(light.token, /^rgba?\(/); assert.notEqual(light.hoverToken, light.token);
  await page.evaluate(() => document.body.setAttribute('data-ds-dark-theme', '')); const dark = await sidebar.measureList(page);
  assert.notEqual(dark.token, light.token); assert.notEqual(dark.hoverToken, dark.token); assert.notEqual(dark.hoverToken, light.hoverToken);
  await page.evaluate(() => document.body.removeAttribute('data-ds-dark-theme')); const restored = await sidebar.measureList(page);
  assert.equal(restored.token, light.token); assert.equal(restored.hoverToken, light.hoverToken);
  const lightPalette = await sidebar.measurePalette(page); await screenshot('light-sidebar');
  await page.evaluate(() => document.body.setAttribute('data-ds-dark-theme', '')); const darkPalette = await sidebar.measurePalette(page); await screenshot('dark-sidebar');
  await page.evaluate(() => document.body.removeAttribute('data-ds-dark-theme'));
  await golden('sidebar-scrollbar', sidebar.renderGeometry(lightPalette, darkPalette));
  await checked('WebKit thumb styling, distinct palettes, and exact source geometry golden with only product-variable renaming');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ error: String(error), errors, warnings, checks, stderr }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-scrollbars', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try { if (server) { if (server.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); } assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0); } } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'scrollbars and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ focus, checks, warnings, modelCalls: 0 }, null, 2));
"#;
