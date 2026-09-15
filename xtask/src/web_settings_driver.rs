//! Settings gestures, source goldens, and cross-Host persistence with real compiled services.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright');
const { expect } = require('playwright/test');
const exec = promisify(execFile), hosts = [], errors = [], warnings = [], checks = [];
let context, page, cdp, runError;
async function startHost(home, label) {
  const workspace = join(world, label), data = join(workspace, '.data');
  await mkdir(data, { recursive: true }); await mkdir(home, { recursive: true });
  const child = spawn(host, [home, workspace, 'settings-permission-before', data], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const item = { child, data, stderr: '', stopped: false };
  hosts.push(item); child.stderr.on('data', value => { item.stderr += value; });
  item.origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('fixture Host readiness: ' + item.stderr)), 30000);
    child.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    child.once('exit', code => { clearTimeout(timer); reject(new Error('fixture Host exit ' + code + ': ' + item.stderr)); });
  });
  return item;
}
async function stopHost(item) {
  if (item.stopped) return;
  item.stopped = true;
  if (item.child.exitCode === null) { const exited = once(item.child, 'exit'); item.child.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, item.stderr); }
  assert.equal(JSON.parse(await readFile(join(item.data, 'model-call-audit.json'), 'utf8')).calls, 0);
}
function tripwire(target, failures = errors, gaps = warnings) {
  target.on('pageerror', error => failures.push(String(error)));
  target.on('console', message => {
    if (message.type() === 'error') failures.push(message.text());
    if (/connection lost|gap repair|discontinuous/i.test(message.text())) gaps.push(message.text());
  });
}
async function checked(name) { assert.deepEqual(errors, []); checks.push(name); await writeFile(join(output, 'result.json'), JSON.stringify({ checks, warnings }, null, 2)); console.log('settings: ' + name); }
async function golden(locator, filename) { const value = (await locator.ariaSnapshot()).trim() + '\n'; await writeFile(join(output, filename + '.actual.md'), value); assert.equal(value, await readFile(join(source, 'apps/web/tests/snapshots/settings-chrome', filename + '.expected.md'), 'utf8')); }
async function annotated(name) { await exec('agent-browser', ['--session', 'seekdeep-settings-parity', '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '.png')]); }
const readTheme = target => target.evaluate(() => {
  const metas = document.head.querySelectorAll('meta[name="theme-color"]'), style = getComputedStyle(document.body);
  return { attr: document.body.hasAttribute('data-ds-dark-theme'), background: style.backgroundColor, legacy: localStorage.getItem('dsh.theme'), themeColor: metas[0]?.content ?? null, count: metas.length, token: style.getPropertyValue('--dsw-alias-bg-base').trim() };
});
function synchronized(state) { assert.equal(state.count, 1); assert.notEqual(state.background, 'rgba(0, 0, 0, 0)'); assert.equal(state.themeColor, state.background); assert.equal(state.legacy, null); }
try {
  const home = join(world, 'home'), first = await startHost(home, 'first');
  const profile = join(world, 'browser-profile');
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'zh-CN', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000); tripwire(page);
  await page.goto(first.origin);
  await page.getByRole('button', { name: '继续', exact: true }).click();
  await page.getByRole('button', { name: '设置', exact: true }).waitFor();
  const document = () => readFile(join(home, 'settings.yaml'), 'utf8');
  const settings = (target = page, name = '设置') => target.getByRole('dialog', { name, exact: true });
  async function open(target = page, name = '设置') { await target.getByRole('button', { name, exact: true }).click(); await settings(target, name).waitFor(); return settings(target, name); }
  async function reload() { const start = warnings.length; await page.reload(); await page.locator('[class*="frame"]').waitFor(); warnings.push(...warnings.splice(start).filter(value => !/connection lost/i.test(value))); }
  async function fixtureSession(id, method = 'GET') { const response = await fetch(first.origin + '/fixture/session/' + id, { method }); assert.equal(response.ok, true); return await response.json(); }
  async function secondOrigin(label, action, locale = 'zh-CN', sharedHome = home) {
    const second = await startHost(sharedHome, label);
    const extra = await context.browser().newContext({ locale, viewport: { width: 1680, height: 1000 } });
    const target = await extra.newPage(), failures = [], gaps = []; tripwire(target, failures, gaps);
    try {
      assert.notEqual(second.origin, first.origin); await target.emulateMedia({ colorScheme: 'light' }); await target.goto(second.origin);
      if (sharedHome !== home) await target.getByRole('button', { name: 'Continue', exact: true }).click();
      await target.locator('[class*="frame"]').waitFor(); await action(target);
      assert.deepEqual(failures, []); assert.deepEqual(gaps, []);
    } finally { await extra.close(); await stopHost(second); }
  }

  const trigger = page.getByRole('button', { name: '设置', exact: true });
  assert.equal(await trigger.getAttribute('aria-haspopup'), 'dialog'); assert.equal(await trigger.getAttribute('aria-expanded'), 'false');
  let dialog = await open(); assert.equal(await trigger.getAttribute('aria-expanded'), 'true');
  await expect(dialog.getByRole('button', { name: '通用设置', exact: true })).toHaveAttribute('aria-current', 'true');
  await expect(dialog.getByText('语言', { exact: true })).toHaveCount(1); await expect(dialog.getByText('外观', { exact: true })).toHaveCount(1);
  const openDocument = dialog.getByRole('button', { name: '打开配置文件', exact: true }); let opened = 0;
  await page.route('**/api/settings.openDocument', route => { const body = route.request().postDataJSON(); assert.deepEqual(body.payload, {}); opened++; return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ type: 'server-response', rpcId: body.rpcId, result: { ok: true, value: { opened: true } } }) }); });
  await openDocument.click(); await expect.poll(() => opened).toBe(1); await expect(openDocument).toBeEnabled(); await page.unroute('**/api/settings.openDocument');
  await golden(dialog, 'dialog'); await annotated('general');
  await dialog.getByRole('button', { name: '模型', exact: true }).click();
  await expect(dialog.getByRole('button', { name: '模型', exact: true })).toHaveAttribute('aria-current', 'true');
  assert.equal(await dialog.getByRole('button', { name: '通用设置', exact: true }).getAttribute('aria-current'), null);
  await dialog.getByRole('button', { name: '插件', exact: true }).click(); await dialog.getByRole('tab', { name: '插件列表', exact: true }).click();
  const count = await (await fetch(first.origin + '/fixture/plugin-count')).json();
  await expect(dialog.locator('[data-plugin-entry]')).toHaveCount(count);
  await expect(dialog.locator('[data-plugin-count]')).toHaveAttribute('data-plugin-count', String(count));
  await expect(dialog.getByRole('searchbox', { name: '搜索插件', exact: true })).toHaveCount(1);
  await expect(dialog.getByRole('button', { name: '插件', exact: true })).toHaveAttribute('aria-current', 'true');
  await expect(dialog.getByRole('tab', { name: '插件列表', exact: true })).toHaveAttribute('aria-selected', 'true');
  assert.equal(await dialog.getByRole('button', { name: '模型', exact: true }).getAttribute('aria-current'), null);
  await golden(dialog.locator('[data-plugin-entry$="ui-settings"]'), 'plugins');
  await page.keyboard.press('Escape'); await expect(dialog).toHaveCount(0); assert.equal(await trigger.getAttribute('aria-expanded'), 'false');
  dialog = await open(); await dialog.getByRole('button', { name: '关闭', exact: true }).click(); await expect(dialog).toHaveCount(0);
  await checked('modal, real plugin inventory, source goldens, and close paths');

  let existing = await fixtureSession('settings-permission-before', 'POST');
  assert.deepEqual(existing.find(event => event.type === 'permission/preset')?.data, { preset: 'workspace-write' });
  dialog = await open(); await dialog.getByRole('button', { name: 'Workspace Write', exact: true }).click(); await page.getByRole('menuitem', { name: 'Read Only', exact: true }).click();
  await dialog.getByRole('button', { name: 'Read Only', exact: true }).waitFor(); await expect.poll(document).toMatch(/permission:\n\s+defaultPreset: read-only/);
  existing = await fixtureSession('settings-permission-before'); assert.deepEqual(existing.find(event => event.type === 'permission/preset')?.data, { preset: 'workspace-write' });
  let created = await fixtureSession('settings-permission-after', 'POST');
  assert.deepEqual(created.map(event => [event.type, event.data]), [['permission/preset', { preset: 'read-only' }], ['sandbox/mode', { mode: 'read-only' }], ['approval/policy', { policy: 'ask' }]]);
  await dialog.getByRole('button', { name: 'Read Only', exact: true }).click(); await page.getByRole('menuitem', { name: 'Full access', exact: true }).click();
  const confirmation = page.getByRole('dialog', { name: '确认启用 Full access？', exact: true }), enable = confirmation.getByRole('button', { name: '启用 Full access', exact: true });
  await expect(enable).toBeDisabled(); await confirmation.getByRole('checkbox').click(); await enable.click(); await dialog.getByRole('button', { name: 'Full access', exact: true }).waitFor();
  await expect.poll(document).toMatch(/defaultPreset: danger-full-access/);
  created = await fixtureSession('settings-permission-confirmed', 'POST');
  assert.deepEqual(created.map(event => [event.type, event.data]), [['permission/preset', { preset: 'danger-full-access' }], ['sandbox/mode', { mode: 'danger-full-access' }], ['approval/policy', { policy: 'never' }]]);
  await page.keyboard.press('Escape'); await checked('permission defaults preserve existing sessions and require Full access confirmation');

  await page.emulateMedia({ colorScheme: 'light' }); dialog = await open(); await dialog.getByRole('button', { name: '深色', exact: true }).click();
  await expect.poll(document).toMatch(/ui-theme:\n\s+preference: dark/); await page.keyboard.press('Escape');
  let release; const held = new Promise(resolve => { release = resolve; });
  await page.route('**/plugins/**', async route => { await held; await route.continue(); });
  const warningStart = warnings.length; let loadingReload;
  try {
    loadingReload = page.reload({ waitUntil: 'domcontentloaded' }); const loading = page.getByText('Loading plugins…', { exact: true }); await loading.waitFor();
    const boot = await loading.evaluate(element => ({ attr: document.body.hasAttribute('data-ds-dark-theme'), background: getComputedStyle(element.parentElement.parentElement).backgroundColor, colorScheme: document.documentElement.style.colorScheme }));
    assert.deepEqual(boot, { attr: true, background: 'rgb(21, 21, 23)', colorScheme: 'dark' });
  } finally { release(); await loadingReload; await page.unroute('**/plugins/**'); }
  await page.locator('[class*="frame"]').waitFor(); warnings.push(...warnings.splice(warningStart).filter(value => !/connection lost/i.test(value)));
  dialog = await open(); const bootSystem = dialog.getByRole('button', { name: '跟随系统', exact: true }); await bootSystem.click(); await expect(bootSystem).toHaveAttribute('aria-pressed', 'true'); await expect.poll(async () => (await readTheme(page)).attr).toBe(false); await page.keyboard.press('Escape');
  await checked('persisted dark preference during held plugin loading');

  const light = await readTheme(page); synchronized(light); assert.equal(light.attr, false);
  dialog = await open(); const dark = dialog.getByRole('button', { name: '深色', exact: true }); await expect(dark).toHaveAttribute('aria-pressed', 'false'); await dark.click(); await expect(dark).toHaveAttribute('aria-pressed', 'true');
  const darkState = await readTheme(page); assert.equal(darkState.attr, true); assert.notEqual(darkState.token, light.token); synchronized(darkState); await page.keyboard.press('Escape');
  await reload(); await expect.poll(async () => (await readTheme(page)).attr).toBe(true); synchronized(await readTheme(page));
  await secondOrigin('theme-peer', async target => { await expect.poll(async () => (await readTheme(target)).attr).toBe(true); synchronized(await readTheme(target)); });
  dialog = await open(); await dialog.getByRole('button', { name: '跟随系统', exact: true }).click(); await expect.poll(async () => (await readTheme(page)).attr).toBe(false); synchronized(await readTheme(page));
  await page.emulateMedia({ colorScheme: 'dark' }); await expect.poll(async () => (await readTheme(page)).attr).toBe(true); synchronized(await readTheme(page));
  await dialog.getByRole('button', { name: '浅色', exact: true }).click(); await expect.poll(async () => (await readTheme(page)).attr).toBe(false); synchronized(await readTheme(page)); await page.keyboard.press('Escape');
  await checked('theme gesture, OS override, synchronized metadata, reload, and second Host');

  dialog = await open(); await dialog.getByRole('button', { name: '排队发送', exact: true }).click(); await page.getByRole('menuitem', { name: '插话发送', exact: true }).click();
  await dialog.getByRole('button', { name: '插话发送', exact: true }).waitFor(); assert.equal(await page.evaluate(() => localStorage.getItem('dsh.conversation.busyEnter')), null);
  await expect.poll(document).toMatch(/ui-conversation:\n\s+busyEnter: steer/); await page.keyboard.press('Escape'); await reload(); dialog = await open(); await dialog.getByRole('button', { name: '插话发送', exact: true }).waitFor();
  await secondOrigin('enter-peer', async target => { const peer = await open(target); await peer.getByRole('button', { name: '插话发送', exact: true }).waitFor(); assert.equal(await target.evaluate(() => localStorage.getItem('dsh.conversation.busyEnter')), null); });
  await dialog.getByRole('button', { name: '插话发送', exact: true }).click(); await page.getByRole('menuitem', { name: '排队发送', exact: true }).click(); await dialog.getByRole('button', { name: '排队发送', exact: true }).waitFor(); assert.equal(await page.evaluate(() => localStorage.getItem('dsh.conversation.busyEnter')), null); await expect.poll(document).toMatch(/ui-conversation:\n\s+busyEnter: queue/); await page.keyboard.press('Escape');
  await checked('busy Enter preference through reload and second Host');

  dialog = await open(); const language = dialog.getByRole('button', { name: '中文', exact: true }); assert.equal(await language.getAttribute('aria-haspopup'), 'menu'); await language.click(); await page.getByRole('menuitem', { name: 'English', exact: true }).click();
  dialog = settings(page, 'Settings'); await dialog.waitFor(); await expect(dialog.getByRole('button', { name: 'General', exact: true })).toHaveAttribute('aria-current', 'true'); await expect(dialog.getByText('Appearance', { exact: true })).toHaveCount(1);
  assert.equal(await page.evaluate(() => localStorage.getItem('dsh.locale')), null); await expect.poll(document).toMatch(/locale:\n\s+preference: en/); await reload();
  await page.getByRole('button', { name: 'Settings', exact: true }).waitFor();
  await secondOrigin('locale-peer', async target => { const peer = await open(target, 'Settings'); await peer.getByRole('button', { name: 'English', exact: true }).waitFor(); assert.equal(await target.evaluate(() => localStorage.getItem('dsh.locale')), null); });
  dialog = await open(page, 'Settings'); await dialog.getByRole('button', { name: 'English', exact: true }).click(); await page.getByRole('menuitem', { name: '中文', exact: true }).click(); await settings().waitFor(); assert.equal(await page.evaluate(() => localStorage.getItem('dsh.locale')), null); await expect.poll(document).toMatch(/locale:\n\s+preference: zh/); await page.keyboard.press('Escape');
  await checked('language switching and persistence across reload and second Host');
  await secondOrigin('fresh-english', async target => { const peer = await open(target, 'Settings'); await peer.getByRole('button', { name: 'English', exact: true }).waitFor(); assert.equal(await target.evaluate(() => localStorage.getItem('dsh.locale')), null); }, 'en-US', join(world, 'fresh-home'));
  await checked('English browser default without stored preference');
  assert.deepEqual(warnings, []); assert.deepEqual((await readdir(join(source, 'apps/web/tests/snapshots/settings-chrome'))).sort(), ['dialog.expected.md', 'plugins.expected.md']);
  await checked('closed source fixture inventory and clean wire');
} catch (error) {
  runError = error;
  await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ errors, warnings, checks, hosts: hosts.map(item => item.stderr) }, null, 2));
} finally {
  const cleanupErrors = [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-settings-parity', 'close']).catch(error => cleanupErrors.push(error));
  await context?.close().catch(error => cleanupErrors.push(error));
  for (const item of hosts.reverse()) {
    try { await stopHost(item); } catch (error) { cleanupErrors.push(error); }
  }
  const failures = [...(runError ? [runError] : []), ...cleanupErrors];
  if (failures.length === 1) throw failures[0];
  if (failures.length) throw new AggregateError(failures, 'settings scenario and cleanup failed');
}
"#;
