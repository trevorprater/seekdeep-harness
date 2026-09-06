//! Cold blank-session filtering and resident Hero identity against the real Rust Host.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test');
const exec = promisify(execFile), errors = [], warnings = [], checks = [];
const home = join(world, 'home'), workspace = join(world, 'workspace'), profile = join(world, 'browser');
let server, context, page, cdp, runError, stderr = '', releaseHistory = () => {};
async function checked(name) { assert.deepEqual(errors, []); assert.deepEqual(warnings, []); checks.push(name); console.log('startup: ' + name); }
async function screenshot(name) { await exec('agent-browser', ['--session', 'seekdeep-startup', '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '.png')]); }
try {
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  server = spawn(host, [home, workspace, 'cold-blank-session-web-e2e'], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + stderr)), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr)); });
  });
  const seeded = await fetch(origin + '/fixture/cold-blank', { method: 'POST' }); assert(seeded.ok); const artifact = await seeded.json();
  assert.equal(artifact.listed, true); assert.equal(artifact.cold, true); assert.equal(artifact.compressed, true); assert(artifact.size > 0 && artifact.size <= 1024);
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'en-US', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000);
  page.on('pageerror', error => errors.push(String(error))); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text()); });
  await page.goto(origin); await page.getByRole('button', { name: 'Continue', exact: true }).click();
  const tree = page.getByRole('tree', { name: 'Sessions', exact: true }); await tree.waitFor();
  await expect(tree.getByText('cold-blank-workspace', { exact: true })).toHaveCount(0);
  let previous = (await tree.ariaSnapshot()).trim(); await expect.poll(async () => { const current = (await tree.ariaSnapshot()).trim(); const stable = current === previous; previous = current; return stable; }).toBe(true);
  await writeFile(join(output, 'sidebar.actual.md'), previous + '\n'); assert.equal(previous + '\n', await readFile(join(source, 'apps/web/tests/snapshots/cold-blank-session/sidebar.expected.md'), 'utf8'));
  await checked('verified compressed cold blank session stays out of the sidebar and matches the source golden');

  await page.locator('div[data-phase="hero"]').waitFor();
  const headline = page.getByText('Into the Unknown', { exact: true }), fish = headline.locator('xpath=preceding-sibling::span[1]/*[name()="svg"]');
  assert.equal(await fish.evaluate(node => getComputedStyle(node).color), await headline.evaluate(node => getComputedStyle(node).color)); await fish.locator('..').hover();
  assert.notEqual(await fish.evaluate(node => getComputedStyle(node).animationName), 'none');
  await page.evaluate(() => {
    window.__heroTree = { root: document.querySelector('div[data-phase="hero"]'), workspaceChip: document.querySelector('[aria-label="Choose workspace"]'), scrollBody: document.querySelector('[data-conversation-scroll]'), composerSeat: document.querySelector('[data-composer-seat]'), textarea: document.querySelector('textarea') };
    if (Object.values(window.__heroTree).some(node => node === null)) throw new Error('incomplete initial Hero tree');
    window.__inputAncestors = []; let node = document.querySelector('textarea'); while (node) { window.__inputAncestors.push(node); node = node.parentElement; }
  });
  const selected = join(workspace, 'startup-auto-selection'); await mkdir(selected);
  await page.getByRole('textbox', { name: 'Choose workspace', exact: true }).click(); const picker = page.getByRole('dialog', { name: 'Select Workspace Directory', exact: true });
  await picker.getByRole('button', { name: 'Edit path', exact: true }).click(); const path = picker.getByRole('textbox', { name: 'Edit path', exact: true });
  await path.fill(selected); await path.press('Enter'); await picker.getByRole('button', { name: 'Open', exact: true }).click(); await page.locator('textarea:enabled[placeholder="Describe what you want to build"]').waitFor();
  await writeFile(join(output, 'identity-debug.json'), JSON.stringify(await page.evaluate(() => { const nodes = []; let node = document.querySelector('textarea'); while (node) { nodes.push(node); node = node.parentElement; } return nodes.map((node, index) => ({ tag: node.tagName, class: node.className, attributes: Object.fromEntries([...node.attributes].map(attribute => [attribute.name, attribute.value])), same: node === window.__inputAncestors[index] })); }), null, 2));
  assert.deepEqual(await page.evaluate(() => {
    const before = window.__heroTree;
    return { phase: document.querySelector('div[data-phase]')?.getAttribute('data-phase'), root: document.querySelector('div[data-phase="hero"]') === before.root, workspaceChip: document.querySelector('[aria-label="Choose workspace"]') === before.workspaceChip, scrollBody: document.querySelector('[data-conversation-scroll]') === before.scrollBody, composerSeat: document.querySelector('[data-composer-seat]') === before.composerSeat, textarea: document.querySelector('textarea') === before.textarea, textareaEnabled: !document.querySelector('textarea').disabled };
  }), { phase: 'hero', root: true, workspaceChip: true, scrollBody: true, composerSeat: true, textarea: true, textareaEnabled: true });
  await checked('first workspace connection preserves the resident Hero, workspace chip, scroll body, and composer nodes');

  await page.addInitScript(() => { window.__conversationPhases = []; setInterval(() => { const phase = document.querySelector('div[data-phase]')?.getAttribute('data-phase'); if (phase != null && window.__conversationPhases.at(-1) !== phase) window.__conversationPhases.push(phase); }, 8); });
  const held = new Promise(resolve => { releaseHistory = resolve; }); let requested; const inFlight = new Promise(resolve => { requested = resolve; }); let gated = false;
  await page.route('**/api/session.history', async route => { if (!gated) { gated = true; requested(); await held; } await route.continue(); });
  const warningStart = warnings.length;
  await page.reload({ waitUntil: 'commit' }); let historyTimer;
  try { await Promise.race([inFlight, new Promise((_, reject) => { historyTimer = setTimeout(() => reject(new Error('auto-selection did not request history')), 15000); })]); } finally { clearTimeout(historyTimer); }
  await page.locator('div[data-phase]').first().waitFor(); assert.equal(await page.locator('div[data-phase]').first().getAttribute('data-phase'), 'hero');
  await expect(page.getByText('Into the Unknown', { exact: true })).toBeVisible(); await expect(page.locator('textarea').first()).toBeVisible(); await screenshot('held-history-hero');
  releaseHistory(); await page.locator('textarea:enabled[placeholder="Describe what you want to build"]').waitFor();
  warnings.push(...warnings.splice(warningStart).filter(value => !/connection lost/i.test(value)));
  assert.deepEqual(await page.evaluate(() => window.__conversationPhases), ['hero']); await checked('auto-selected blank-session history keeps the Hero visible and never enters settling');
} catch (error) {
  runError = error; releaseHistory(); await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ errors, warnings, checks, stderr }, null, 2));
} finally {
  releaseHistory(); const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-startup', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try { if (server?.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); } assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0); } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'startup and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ checks, warnings, modelCalls: 0 }, null, 2));
"#;
