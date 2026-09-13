//! Source provider-editor scenarios against real settings, credentials, and provider topology.

pub(super) const DRIVER: &str = r"import assert from 'node:assert/strict';
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
const exec = promisify(execFile), errors = [], warnings = [], requests = [], checks = [];
const home = join(world, 'home'), workspace = join(world, 'workspace'), profile = join(world, 'browser');
let server, context, page, cdp, runError, stderr = '';
const document = () => readFile(join(home, 'settings.yaml'), 'utf8').catch(() => '');
const credentials = () => readFile(join(home, '.credentials.yaml'), 'utf8').catch(() => '');
async function checked(name) { assert.deepEqual(errors, []); checks.push(name); await writeFile(join(output, 'result.json'), JSON.stringify({ checks, warnings }, null, 2)); console.log('models: ' + name); }
async function golden(locator, name) {
  let previous = (await locator.ariaSnapshot()).trim();
  await expect.poll(async () => { const current = (await locator.ariaSnapshot()).trim(); const stable = current === previous; previous = current; return stable; }, { timeout: 5000 }).toBe(true);
  const text = previous + '\n'; await writeFile(join(output, name + '.actual.md'), text);
  assert.equal(text, await readFile(join(source, 'apps/web/tests/snapshots/models-settings', name + '.expected.md'), 'utf8'));
}
async function annotated(name) { await exec('agent-browser', ['--session', 'seekdeep-models-parity', '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '.png')]); }
try {
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  const environment = { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' };
  delete environment.MINIMAX_CN_API_KEY;
  server = spawn(host, [home, workspace, 'models-settings'], { cwd: process.cwd(), env: environment, stdio: ['ignore', 'pipe', 'pipe'] });
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
  page.on('request', request => { if (request.url().startsWith(origin + '/api/')) requests.push(new URL(request.url()).pathname); });
  await page.goto(origin); await page.getByRole('button', { name: '继续', exact: true }).click();
  await page.getByRole('button', { name: '设置', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '设置', exact: true });
  await dialog.getByRole('button', { name: '模型', exact: true }).click();
  await dialog.getByText('配置 API 密钥或提供方登录，即可使用其模型。', { exact: true }).waitFor();
  const add = dialog.getByRole('button', { name: '添加提供方', exact: true }); await expect(add).toBeEnabled(); await add.click();
  const pick = dialog.getByLabel('提供方', { exact: true });
  await expect.poll(() => pick.locator('option').count()).toBeGreaterThan(30);
  const options = await pick.locator('option').allTextContents(); for (const name of ['anthropic', 'minimax-cn', 'openai-codex']) assert(options.includes(name));
  await pick.selectOption('minimax-cn'); await dialog.getByRole('textbox', { name: 'API 密钥', exact: true }).waitFor(); await golden(dialog, 'empty');
  await pick.selectOption('openai-codex'); await dialog.getByText(/Codex CLI 保存的 ChatGPT 登录/).waitFor();
  await expect(dialog.getByRole('textbox', { name: 'API 密钥', exact: true })).toHaveCount(0); await golden(dialog, 'codex');
  await pick.selectOption('minimax-cn'); await checked('dormant provider vocabulary and OAuth-only card');

  const key = dialog.getByRole('textbox', { name: 'API 密钥', exact: true }), save = dialog.getByRole('button', { name: '保存', exact: true });
  const beforeSettings = await document(), beforeCredentials = await credentials();
  await key.fill('sk-😀minimax'); await dialog.getByText('该 API 密钥格式错误，请检查。', { exact: true }).waitFor(); await expect(save).toBeDisabled();
  assert.equal(await document(), beforeSettings); assert.equal(await credentials(), beforeCredentials);
  await key.fill(''); await expect(save).toBeEnabled(); await expect(dialog.getByText('该 API 密钥格式错误，请检查。', { exact: true })).toHaveCount(0);
  await checked('illegal header key rejected before writes');
  await save.click(); await dialog.getByText('已保存 minimax-cn。', { exact: true }).waitFor();
  await expect(dialog.getByRole('img', { name: 'API 密钥已配置' })).toHaveCount(0); await expect(dialog.getByRole('img', { name: 'API 密钥缺失' })).toHaveCount(0);
  assert((await document()).includes('minimax-cn: {}')); assert(!(await document()).includes('MINIMAX_CN_API_KEY'));
  await checked('blank key creates reference-free native-auth profile');
  await dialog.getByRole('button', { name: '删除 minimax-cn', exact: true }).click();
  let deletion = page.getByRole('dialog', { name: '删除 minimax-cn？', exact: true }); await deletion.waitFor(); await golden(deletion, 'native-delete');
  await deletion.getByRole('button', { name: '取消', exact: true }).click(); await checked('reference-free deletion wording');

  await dialog.getByRole('button', { name: '编辑 minimax-cn', exact: true }).click();
  await dialog.getByRole('textbox', { name: 'API 密钥', exact: true }).fill('sk-e2e-minimax'); await dialog.getByRole('button', { name: '保存', exact: true }).click();
  await expect(dialog.getByRole('textbox', { name: 'API 密钥', exact: true })).toHaveCount(0); await dialog.getByRole('img', { name: 'API 密钥已配置' }).waitFor();
  await dialog.getByText('已保存 minimax-cn。', { exact: true }).waitFor();
  const saved = await document(); assert(saved.includes('minimax-cn:')); assert(saved.includes('apiKeyEnv: MINIMAX_CN_API_KEY')); assert(!saved.includes('sk-e2e-minimax'));
  await expect.poll(credentials).toContain('MINIMAX_CN_API_KEY: sk-e2e-minimax'); assert(!(await page.content()).includes('sk-e2e-minimax'));
  await checked('write-only credential and derived profile reference');
  await dialog.getByRole('button', { name: '编辑 minimax-cn', exact: true }).click(); await dialog.getByText('自定义设置', { exact: true }).click();
  await dialog.getByLabel('API 地址', { exact: true }).fill('https://gateway.minimax.example/v1'); await dialog.getByRole('button', { name: '保存', exact: true }).click();
  await expect(dialog.getByLabel('API 地址', { exact: true })).toHaveCount(0); await dialog.getByText('已保存 minimax-cn。', { exact: true }).waitFor();
  assert((await document()).includes('baseURL: https://gateway.minimax.example/v1')); assert((await document()).includes('apiKeyEnv: MINIMAX_CN_API_KEY')); await expect(dialog.getByRole('button', { name: '添加自定义提供方', exact: true })).toBeEnabled(); await golden(dialog, 'configured');
  await checked('custom settings merge preserves credential reference');

  await dialog.getByRole('button', { name: '添加自定义提供方', exact: true }).click();
  await dialog.getByLabel('Provider ID', { exact: true }).fill('acme-gateway'); await dialog.getByLabel('显示名称', { exact: true }).fill('Acme Gateway');
  await dialog.getByLabel('API 地址', { exact: true }).fill('https://gateway.acme.example/v1'); await expect(dialog.getByLabel('推理强度', { exact: true })).toHaveCount(0);
  await dialog.getByRole('button', { name: '添加模型', exact: true }).click(); await dialog.getByLabel('模型 ID 1', { exact: true }).fill('acme-large');
  await dialog.getByRole('button', { name: '创建提供方', exact: true }).click(); await dialog.getByText('Acme Gateway', { exact: true }).first().waitFor();
  assert((await document()).includes('acme-gateway:'));
  const row = name => dialog.locator('li').filter({ hasText: name }).first();
  await expect(row('Acme Gateway').getByText('自定义', { exact: true })).toHaveCount(1); await expect(row('minimax-cn').getByText('自定义', { exact: true })).toHaveCount(0);
  await golden(dialog, 'declared'); await checked('custom route and model declaration');
  await dialog.getByRole('button', { name: '编辑 Acme Gateway (acme-gateway)', exact: true }).click(); await dialog.getByText('自定义设置', { exact: true }).click();
  const protocol = dialog.getByLabel('API 协议', { exact: true }), name = dialog.getByLabel('显示名称', { exact: true });
  assert.equal(await protocol.inputValue(), 'openai-completions'); assert.equal(await name.inputValue(), 'Acme Gateway'); await golden(dialog, 'declared-edit');
  await protocol.selectOption('anthropic-messages'); await name.fill('Acme 网关'); await dialog.getByRole('button', { name: '保存', exact: true }).click();
  await expect(dialog.getByLabel('API 协议', { exact: true })).toHaveCount(0); await dialog.getByText('Acme 网关', { exact: true }).first().waitFor();
  await dialog.getByText('已保存 Acme 网关 (acme-gateway)。', { exact: true }).waitFor();
  assert((await document()).includes('api: anthropic-messages')); assert((await document()).includes('displayName: Acme 网关')); await annotated('custom-provider');
  await checked('custom route identity and protocol edit');

  await dialog.getByRole('button', { name: '删除 minimax-cn', exact: true }).click(); deletion = page.getByRole('dialog', { name: '删除 minimax-cn？', exact: true });
  await deletion.waitFor(); await golden(deletion, 'delete'); await deletion.getByRole('button', { name: '取消', exact: true }).click(); assert((await document()).includes('minimax-cn:'));
  await dialog.getByRole('button', { name: '删除 minimax-cn', exact: true }).click(); await deletion.getByRole('button', { name: '删除 minimax-cn', exact: true }).click();
  await expect.poll(document).not.toContain('minimax-cn:'); assert(!(await credentials()).includes('MINIMAX_CN_API_KEY')); await expect(deletion).toHaveCount(0); await page.keyboard.press('Escape');
  await checked('identified deletion, cancel, profile removal, and credential removal');
  assert.deepEqual((await readdir(join(source, 'apps/web/tests/snapshots/models-settings'))).sort(), ['codex.expected.md', 'configured.expected.md', 'declared-edit.expected.md', 'declared.expected.md', 'delete.expected.md', 'empty.expected.md', 'native-delete.expected.md']);
  assert.deepEqual(warnings, []); assert(!requests.includes('/api/session.prompt')); await checked('closed fixture inventory and no prompt traffic');
} catch (error) {
  runError = error;
  await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ errors, warnings, checks, stderr }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-models-parity', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  try {
    if (server?.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, stderr); }
    assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0);
  } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0];
  if (failures.length) throw new AggregateError(failures, 'model settings scenario and cleanup failed');
}
";
