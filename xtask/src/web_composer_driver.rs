//! Real-browser composer geometry using the pinned source's measurement functions.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const focus = process.env.SEEKDEEP_COMPOSER_FOCUS ?? 'all';
assert(['all', 'tabs'].includes(focus), 'unknown composer test focus');
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test'), ts = require('typescript');
const exec = promisify(execFile), errors = [], warnings = [], checks = [];
const home = join(world, 'home'), workspace = join(world, 'workspace'), profile = join(world, 'browser');
let server, context, page, cdp, runError, stderr = '';

// Only test declarations are transplanted; all browser behavior comes from the built WASM.
async function sourceHelpers(file, names) {
  const body = await readFile(join(source, 'apps/web/tests', file), 'utf8');
  const ast = ts.createSourceFile(file, body, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const selected = ast.statements.filter(node => {
    const declared = ts.isVariableStatement(node) ? node.declarationList.declarations.map(value => value.name.getText(ast)) : node.name ? [node.name.getText(ast)] : [];
    return declared.some(name => names.includes(name));
  });
  assert.equal(selected.length, names.length, 'source measurement declaration inventory');
  const code = ts.transpileModule(selected.map(node => node.getText(ast)).join('\n'), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
  return new Function(code + '\nreturn { ' + names.join(',') + ' };')();
}
async function checked(name) { assert.deepEqual(errors, []); assert.deepEqual(warnings, []); checks.push(name); console.log('composer: ' + name); }
async function screenshot(name) { await exec('agent-browser', ['--session', 'seekdeep-composer', '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '.png')]); }
async function connect(name) {
  const selected = join(workspace, name); await mkdir(selected);
  await page.getByRole('textbox', { name: 'Choose workspace', exact: true }).click();
  const picker = page.getByRole('dialog', { name: 'Select Workspace Directory', exact: true });
  await picker.getByRole('button', { name: 'Edit path', exact: true }).click();
  const path = picker.getByRole('textbox', { name: 'Edit path', exact: true });
  await path.fill(selected); await path.press('Enter'); await picker.getByRole('button', { name: 'Open', exact: true }).click();
  await page.locator('textarea:enabled[placeholder="Describe what you want to build"]').waitFor();
}
try {
  const draft = await sourceHelpers('composer-draft-scroll.e2e.ts', ['FIRST_MARKER', 'LAST_MARKER', 'DRAFT_LINES', 'DRAFT', 'DRAFT_TRAILING_NEWLINE', 'measureComposer', 'renderGeometry']);
  const tabs = await sourceHelpers('composer-tab-geometry.e2e.ts', ['WIDE_VIEWPORT', 'NARROW_VIEWPORT', 'CONTROL_STYLE_ID', 'CONTROL_CSS', 'setMeasuredViewport', 'measureTab', 'showTab', 'compareTabs', 'compareTabsWithoutCompensation', 'renderGeometry']);
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  const generated = await exec(process.execPath, ['--import', 'tsx', '--input-type=module', '-e', 'import { createChatScrollFixture } from "./apps/web/tests/chat-scroll-fixture.ts"; const fixture = createChatScrollFixture({markerPrefix:"TAB_GEOMETRY",title:"COMPOSER_TAB_GEOMETRY long session",turns:24}); console.log(JSON.stringify({log:fixture.log,firstUser:fixture.markers.user(1),lastAssistant:fixture.markers.assistant(fixture.turns)}));'], { cwd: source, maxBuffer: 4 * 1024 * 1024 });
  const fixture = JSON.parse(generated.stdout), seedLog = join(world, 'tab-seed.jsonl'), extraOverlay = join(world, 'empty.patch.yml');
  await writeFile(seedLog, fixture.log); await writeFile(extraOverlay, '[]\n');
  server = spawn(host, [home, workspace, 'composer-tab-geometry-web-e2e', home, 'route-only', extraOverlay, seedLog], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + stderr)), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr)); });
  });
  const seeded = await fetch(origin + '/fixture/seed-log', { method: 'POST' }); assert(seeded.ok, await seeded.text());
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'en-US', viewport: { width: 1680, height: 1000 }, ignoreDefaultArgs: ['--hide-scrollbars'], args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000);
  page.on('pageerror', error => errors.push(String(error))); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text()); });
  await page.goto(origin); await page.getByRole('button', { name: 'Continue', exact: true }).click();
  if (focus === 'all') {
  await connect('composer-draft-scroll');
  const input = page.locator('textarea:enabled').first(), measure = () => draft.measureComposer(page);
  const visibleLast = metrics => { assert(metrics.lastLineOffset >= 0); assert(metrics.lastLineOffset < metrics.clientHeight); };
  async function wheel(delta, predicate) { await input.hover(); await page.mouse.wheel(0, delta); await expect.poll(async () => predicate(await measure()), { timeout: 10000 }).toBe(true); }
  async function paste(trailing) {
    await input.fill('one short line'); await input.press('End');
    await input.evaluate((node, text) => { const data = new DataTransfer(); data.setData('text/plain', text); node.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true })); }, '\n' + draft.DRAFT + (trailing ? '\n' : ''));
    await expect.poll(async () => (await measure()).overflows).toBe(true); await expect.poll(async () => (await measure()).scrollTop).toBeGreaterThan(0);
    const metrics = await measure(); visibleLast(metrics); assert.equal(metrics.gapShiftOnScroll, 0); return metrics;
  }
  await input.fill(draft.DRAFT); await expect.poll(async () => (await measure()).overflows).toBe(true);
  await wheel(-2000, value => value.scrollTop === 0); const top = await measure();
  assert.equal(top.visibleLines, 14); assert.equal(top.inputScrollable, 0); assert.equal(top.scrollTop, 0);
  assert(top.firstLineOffset >= 0 && top.firstLineOffset < top.clientHeight); assert(top.lastLineOffset > top.clientHeight);
  assert.equal(top.backdropWrapWidth, top.inputWrapWidth); assert.equal(top.mirrorWrapWidth, top.inputWrapWidth); assert.equal(top.gapShiftOnScroll, 0);
  await checked('14-line cap, single scrollport, equal wrap widths, and same-task caret/glyph coupling');
  await wheel(2000, value => value.scrollTop > 0); const bottom = await measure();
  assert.equal(bottom.caretGlyphGap, top.caretGlyphGap); visibleLast(bottom); assert(bottom.firstLineOffset < 0);
  await checked('wheel scrolls the visible text and caret together');
  await input.press('End'); await wheel(-2000, value => value.scrollTop === 0); await input.pressSequentially(' tail');
  const edited = await measure(); assert(edited.scrollTop > 0); visibleLast(edited);
  await checked('typing at the end reveals the caret after scrolling away');
  await paste(true); await checked('real clipboard paste reveals the end including a trailing newline');
  await input.fill(draft.DRAFT_TRAILING_NEWLINE); await expect.poll(async () => (await measure()).overflows).toBe(true);
  await wheel(4000, value => value.scrollTop === value.scrollMax); const trailing = await measure();
  assert.equal(trailing.gapShiftOnScroll, 0); visibleLast(trailing);
  await checked('trailing-newline draft reaches its true bottom');
  await input.fill(draft.DRAFT); await wheel(-2000, value => value.scrollTop === 0); const goldenTop = await measure();
  await wheel(2000, value => value.scrollTop > 0); const goldenBottom = await measure();
  await input.fill(draft.DRAFT_TRAILING_NEWLINE); await wheel(4000, value => value.scrollTop === value.scrollMax); const goldenTrailing = await measure();
  const pasted = await paste(false); const actual = draft.renderGeometry(goldenTop, goldenBottom, goldenTrailing, pasted) + '\n';
  await writeFile(join(output, 'draft-geometry.actual.md'), actual);
  const snapshots = join(source, 'apps/web/tests/snapshots/composer-draft-scroll');
  assert.equal(actual, await readFile(join(snapshots, 'geometry.expected.md'), 'utf8'));
  assert.deepEqual((await readdir(snapshots)).sort(), ['geometry.expected.md']);
  await screenshot('draft-paste-bottom'); await checked('exact source draft geometry golden and closed fixture inventory');
  }

  await page.getByRole('button', { name: 'Search sessions', exact: true }).click();
  await page.getByRole('textbox', { name: 'Search sessions...', exact: true }).fill(fixture.firstUser);
  const results = page.getByRole('tree', { name: 'Search results', exact: true }).getByRole('treeitem');
  await expect(results).toHaveCount(1, { timeout: 60000 }); await results.click();
  await page.getByText(fixture.lastAssistant, { exact: false }).last().waitFor();
  await tabs.setMeasuredViewport(page, tabs.WIDE_VIEWPORT, false);
  await expect.poll(async () => (await tabs.measureTab(page)).scrolls).toBe(true);
  const wide = await tabs.compareTabs(page);
  await writeFile(join(output, 'wide-metrics.json'), JSON.stringify(wide, null, 2));
  await tabs.showTab(page, 'Trajectory'); await screenshot('wide-trajectory-composer');
  await writeFile(join(output, 'trajectory-layout.json'), JSON.stringify(await page.evaluate(() => {
    const nodes = []; let node = document.querySelector('[data-conversation-composer-overlay]');
    while (node) { const style = getComputedStyle(node); nodes.push({ tag: node.tagName, class: node.className, slot: node.getAttribute('data-slot'), height: node.getBoundingClientRect().height, scrollHeight: node.scrollHeight, clientHeight: node.clientHeight, flex: style.flex, display: style.display, minHeight: style.minHeight, overflow: style.overflow }); node = node.parentElement; } return nodes;
  }), null, 2));
  await tabs.showTab(page, 'Chat');
  assert(wide.chat.band > 0); assert.equal(wide.chat.gutter, 'stable');
  assert.equal(wide.trajectory.gutter, 'auto'); assert.equal(wide.trajectory.band, 0);
  assert.equal(wide.trajectory.overflowY, 'auto'); assert.equal(wide.trajectory.overflowX, 'hidden'); assert.equal(wide.trajectory.scrolls, false);
  function stationary(value) { assert.equal(value.leftShift, 0); assert.equal(value.rightShift, 0); assert.equal(value.widthShift, 0); }
  stationary(wide); await checked('real scrollbar reservation and stable capped composer across Chat and Trajectory');
  await tabs.setMeasuredViewport(page, tabs.NARROW_VIEWPORT, true); const narrow = await tabs.compareTabs(page);
  assert(narrow.chat.cardWidth < wide.chat.cardWidth); stationary(narrow);
  await screenshot('narrow-chat-composer'); await checked('stable shrinking composer at 800px with responsive sidebar');
  await tabs.setMeasuredViewport(page, tabs.WIDE_VIEWPORT, false); const control = await tabs.compareTabsWithoutCompensation(page);
  assert.equal(control.chat.gutter, 'stable'); assert(control.chat.band > 0); assert.equal(control.trajectory.band, 0);
  assert.equal(control.leftShift, control.chat.band / 2); assert.equal(control.rightShift, control.chat.band / 2);
  stationary(await tabs.compareTabs(page)); await checked('source CSS control produces the expected half-scrollbar shift and restores cleanly');
  const tabActual = tabs.renderGeometry(wide, narrow, control) + '\n', tabSnapshots = join(source, 'apps/web/tests/snapshots/composer-tab-geometry');
  await writeFile(join(output, 'tab-geometry.actual.md'), tabActual);
  assert.equal(tabActual, await readFile(join(tabSnapshots, 'geometry.expected.md'), 'utf8'));
  assert.deepEqual((await readdir(tabSnapshots)).sort(), ['geometry.expected.md']);
  await screenshot('wide-chat-composer'); await checked('exact source tab geometry golden and closed fixture inventory');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ error: String(error), errors, warnings, checks, stderr }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-composer', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try { if (server?.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); } assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0); } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'composer and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ focus, checks, warnings, modelCalls: 0 }, null, 2));
"#;
