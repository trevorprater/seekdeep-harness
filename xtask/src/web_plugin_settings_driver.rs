//! Staged plugin configuration through the real browser and Host settings layers.

pub(super) const DRIVER: &str = r"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test');
const exec = promisify(execFile), errors = [], warnings = [], checks = [];
const home = join(world, 'home'), workspace = join(world, 'workspace'), profile = join(world, 'browser');
let server, context, page, cdp, runError, stderr = '';
const document = () => readFile(join(home, 'settings.yaml'), 'utf8').catch(() => '');
async function checked(name) { assert.deepEqual(errors, []); checks.push(name); console.log('plugins: ' + name); }
async function openPlugins() {
  const dialog = page.getByRole('dialog', { name: '设置', exact: true });
  if (await dialog.count()) { await page.keyboard.press('Escape'); await expect(dialog).toHaveCount(0); }
  await page.getByRole('button', { name: '设置', exact: true }).click();
  await dialog.getByRole('button', { name: '插件', exact: true }).click();
  await expect(dialog.getByRole('button', { name: '插件', exact: true })).toHaveAttribute('aria-current', 'true');
  await expect(dialog.getByRole('tab', { name: '插件配置', exact: true })).toHaveAttribute('aria-selected', 'true');
  return dialog;
}
async function openTerminal() {
  const dialog = await openPlugins(); await dialog.getByText('终端', { exact: true }).click();
  const timeout = dialog.getByLabel('命令超时（毫秒）'); await timeout.waitFor();
  return { dialog, timeout, save: dialog.getByRole('button', { name: '保存', exact: true }) };
}
try {
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  server = spawn(host, [home, workspace, 'plugin-settings'], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness timed out: ' + stderr)), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr)); });
  });
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'zh-CN', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000);
  page.on('pageerror', error => errors.push(String(error)));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text()); });
  await page.goto(origin); await page.getByRole('button', { name: '继续', exact: true }).click();
  const section = await openPlugins(); await section.getByText('终端', { exact: true }).waitFor();
  await expect(section.getByText('Agent 循环', { exact: true })).toHaveCount(1);
  await expect(section.getByText('网页搜索', { exact: true })).toHaveCount(1);
  await expect(section.getByLabel('命令超时（毫秒）')).toHaveCount(0);
  let previous = (await section.ariaSnapshot()).trim();
  await expect.poll(async () => { const current = (await section.ariaSnapshot()).trim(); const stable = current === previous; previous = current; return stable; }).toBe(true);
  await writeFile(join(output, 'section.actual.md'), previous + '\n');
  assert.equal(previous + '\n', await readFile(join(source, 'apps/web/tests/snapshots/plugin-config/section.expected.md'), 'utf8'));
  await checked('three exposed namespaces and exact source section golden');

  let { dialog, timeout, save } = await openTerminal();
  await expect(timeout).toHaveValue('60000'); await timeout.fill('12000'); await timeout.blur();
  assert(!(await document()).includes('timeoutMs')); await expect(save).toBeEnabled(); await save.click();
  await expect.poll(document).toContain('timeoutMs: 12000'); await expect(dialog.getByText('已覆盖', { exact: true })).toHaveCount(1);
  await expect(dialog.getByRole('button', { name: '恢复默认', exact: true })).toHaveCount(1); await expect(save).toBeDisabled();
  await expect(dialog.getByText('本部署没有接受这些值，已保留供你修改。', { exact: true })).toHaveCount(0);
  await exec('agent-browser', ['--session', 'seekdeep-plugin-settings', '--cdp', cdp, 'screenshot', '--annotate', join(output, 'saved-override.png')]);
  await checked('staged edit writes only on Save and reports the user override');

  ({ dialog, timeout, save } = await openTerminal()); await timeout.fill('7000');
  await dialog.getByRole('button', { name: '放弃修改', exact: true }).click(); await expect(timeout).toHaveValue('12000');
  assert((await document()).includes('timeoutMs: 12000')); await checked('Discard restores the saved value without writing');

  ({ dialog, timeout, save } = await openTerminal()); await timeout.fill('soon'); await expect(save).toBeDisabled();
  await expect(dialog.getByText('请填数字；留空表示使用默认值。', { exact: true })).toHaveCount(1);
  await dialog.getByRole('button', { name: '放弃修改', exact: true }).click(); await checked('invalid numeric draft cannot be saved');

  ({ dialog, timeout, save } = await openTerminal()); await expect(timeout).toHaveValue('12000');
  await dialog.getByRole('button', { name: '恢复默认', exact: true }).click(); await expect(timeout).toHaveValue('60000');
  assert((await document()).includes('timeoutMs: 12000')); await save.click(); await expect.poll(document).not.toContain('timeoutMs');
  await expect(timeout).toHaveValue('60000'); await expect(dialog.getByText('已覆盖', { exact: true })).toHaveCount(0);
  await checked('Reset stages the composed default and Save removes the user field');

  const loopSettings = await openPlugins(); await loopSettings.getByText('Agent 循环', { exact: true }).click();
  const cap = loopSettings.getByLabel('并行工具调用数'), loopSave = loopSettings.getByRole('button', { name: '保存', exact: true });
  const liveCap = async () => { const response = await fetch(origin + '/fixture/agent-loop-cap'); assert(response.ok); return response.json(); };
  await expect(cap).toHaveValue('10'); assert.equal(await liveCap(), 10);
  await cap.fill('1'); assert.equal(await liveCap(), 10); await loopSave.click();
  await expect.poll(liveCap).toBe(1); await expect.poll(document).toContain('maxParallelToolCalls: 1'); await expect(loopSave).toBeDisabled();
  await expect(loopSettings.getByText('本部署没有接受这些值，已保留供你修改。', { exact: true })).toHaveCount(0);
  await cap.fill('0'); await expect(loopSave).toBeEnabled(); await loopSave.click();
  await loopSettings.getByText('本部署没有接受这些值，已保留供你修改。', { exact: true }).waitFor();
  assert.equal(await liveCap(), 1); await expect(cap).toHaveValue('0');
  await loopSettings.getByRole('button', { name: '放弃修改', exact: true }).click();
  await loopSettings.getByRole('button', { name: '恢复默认', exact: true }).click();
  await expect(cap).toHaveValue('10'); assert.equal(await liveCap(), 1); await loopSave.click();
  await expect.poll(liveCap).toBe(10); await expect.poll(document).not.toContain('maxParallelToolCalls');
  await checked('Agent Loop browser writes reach the real scheduler and invalid writes preserve its cap');
  assert.deepEqual(await readdir(join(source, 'apps/web/tests/snapshots/plugin-config')), ['section.expected.md']);
  assert.deepEqual(warnings, []); await checked('closed fixture inventory and clean browser wire');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ errors, warnings, checks, stderr }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-plugin-settings', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try {
    if (server?.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); }
    assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0);
  } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0];
  if (failures.length) throw new AggregateError(failures, 'plugin settings scenario and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ checks, warnings, modelCalls: 0 }, null, 2));
";
