//! First-run provider configuration against the real credential-less `DeepSeek` adapter.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
const [source, host, world, output] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect } = require('playwright/test');
const { parse: parseYaml } = createRequire(join(source, 'packages/settings/settings-file/package.json'))('yaml');
const exec = promisify(execFile), hosts = [], contexts = [], errors = [], warnings = [], consoles = [], checks = [];
const credentialTitle = '添加一个 API Key 开始使用', version = '2026-08-13.1';
let page, runError, sequence = 0;
async function start(label) {
  const home = join(world, label, 'home'), workspace = join(world, label, 'workspace'), profile = join(world, label, 'browser');
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  const env = { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' };
  delete env.DEEPSEEK_API_KEY; delete env.MINIMAX_CN_API_KEY;
  const child = spawn(host, [home, workspace, label, home, 'missing-credential'], { cwd: process.cwd(), env, stdio: ['ignore', 'pipe', 'pipe'] });
  const item = { child, home, workspace, stderr: '' }; hosts.push(item);
  child.stderr.on('data', value => { item.stderr += value; });
  item.origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + item.stderr)), 30000);
    child.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    child.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + item.stderr)); });
  });
  item.context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'zh-CN', viewport: { width: 1440, height: 960 }, args: ['--remote-debugging-port=0'] });
  contexts.push(item.context); item.page = item.context.pages()[0] ?? await item.context.newPage();
  item.cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; item.browserSession = 'seekdeep-onboarding-' + label;
  item.page.setDefaultTimeout(15000);
  item.page.on('pageerror', error => errors.push(String(error)));
  item.page.on('console', message => { consoles.push(message.text()); if (message.type() === 'error') errors.push(message.text()); if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text()); });
  await item.page.goto(item.origin); return item;
}
async function invoke(item, method, payload) {
  const rpcId = 'onboarding-' + ++sequence;
  const response = await fetch(item.origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) });
  assert(response.ok); const body = await response.json(); assert.equal(body.rpcId, rpcId); assert.equal(body.result.ok, true, JSON.stringify(body)); return body.result.value;
}
async function document(item, filename) { return parseYaml(await readFile(join(item.home, filename), 'utf8')); }
async function reload(target) { const start = warnings.length; await target.reload(); await target.locator('[class*="frame"]').waitFor(); warnings.push(...warnings.splice(start).filter(value => !/connection lost/i.test(value))); }
async function checked(name) { assert.deepEqual(errors, []); checks.push(name); console.log('onboarding: ' + name); }
async function golden(locator, directory, name) {
  let previous = (await locator.ariaSnapshot()).trim();
  await expect.poll(async () => { const current = (await locator.ariaSnapshot()).trim(); const stable = current === previous; previous = current; return stable; }).toBe(true);
  const text = previous + '\n'; await writeFile(join(output, directory + '-' + name + '.actual.md'), text);
  let expected = (await readFile(join(source, 'apps/web/tests/snapshots', directory, name + '.expected.md'), 'utf8')).replaceAll('DeepSeek Harness', 'SeekDeep Harness').replaceAll('DSH 插件生态', 'SeekDeep 插件生态');
  if ((directory === 'onboarding-deepseek-config' && name === 'models') || (directory === 'onboarding-usable-provider' && name === 'dismissed')) {
    const stale = '  - paragraph: 填入各提供方的 API 密钥即可使用其模型。\n';
    assert.equal(expected.split(stale).length, 2, 'revisit the recorded stale-intro exception when the oracle changes');
    const locale = await readFile(join(source, 'packages/client/ui-settings-models/src/client/locales.ts'), 'utf8');
    const current = /export const zh:[\s\S]*?\n  intro: '([^']+)'/.exec(locale)?.[1]; assert(current, 'source Chinese intro missing');
    expected = expected.replace(stale, '  - paragraph: ' + current + '\n');
  }
  if (directory === 'onboarding-usable-provider' && name === 'dismissed') {
    const anchor = '    - option "openai"\n', addition = '    - option "openai-codex"\n';
    const catalog = await readFile(join(source, 'apps/web/tests/snapshots/models-settings/empty.expected.md'), 'utf8');
    assert(catalog.includes(anchor + addition), 'source Models snapshot must pin the Codex option');
    assert(!expected.includes(addition)); assert.equal(expected.split(anchor).length, 2);
    expected = expected.replace(anchor, anchor + addition);
  }
  assert.equal(text, expected);
}
async function assertPrivate(target, secret) { assert(!(await target.content()).includes(secret)); assert(!(await target.locator('body').ariaSnapshot()).includes(secret)); assert(!consoles.some(line => line.includes(secret))); }
async function openModels(target) { await target.getByRole('button', { name: '设置', exact: true }).click(); const settings = target.getByRole('dialog', { name: '设置', exact: true }); await settings.getByRole('button', { name: '模型', exact: true }).click(); return settings; }
try {
  const official = await start('official'); page = official.page;
  const welcome = page.getByRole('dialog', { name: '内测声明', exact: true }); await welcome.waitFor();
  assert.equal(await page.locator('#root').evaluate(root => root.inert), true);
  assert.deepEqual(await welcome.getByRole('button').allTextContents(), ['继续']);
  await golden(welcome, 'onboarding-deepseek-config', 'welcome');
  await reload(page); await welcome.waitFor(); await welcome.getByRole('button', { name: '继续', exact: true }).click(); await expect(welcome).toHaveCount(0);
  const credential = page.getByRole('dialog', { name: credentialTitle, exact: true }); await credential.waitFor();
  await golden(credential, 'onboarding-deepseek-config', 'missing');
  const secret = 'seekdeep_onboarding_fixture_key'; await credential.getByLabel('API 密钥', { exact: true }).fill(secret);
  await credential.getByRole('button', { name: '保存并继续', exact: true }).click(); await expect(credential).toHaveCount(0);
  assert.equal(await page.locator('#root').evaluate(root => root.inert), false);
  assert.equal((await document(official, '.credentials.yaml')).DEEPSEEK_API_KEY, secret);
  assert.equal((await document(official, 'settings.yaml'))['ui-onboarding'].welcomeNoticeVersion, version); await assertPrivate(page, secret);
  let settings = await openModels(page); await settings.getByRole('button', { name: '编辑 DeepSeek (deepseek-official)', exact: true }).click();
  await expect(settings.getByLabel('API 密钥', { exact: true })).toHaveAttribute('placeholder', '已配置——输入新值可替换');
  await reload(page); await expect(welcome).toHaveCount(0); await expect(credential).toHaveCount(0);
  await invoke(official, 'settings.mutate', { ns: 'ui-onboarding', ops: [{ op: 'set', path: ['welcomeNoticeVersion'], value: 'previous-copy-version' }] });
  await reload(page); await welcome.waitFor(); await welcome.getByRole('button', { name: '继续', exact: true }).click(); await expect(welcome).toHaveCount(0); await expect(credential).toHaveCount(0); await assertPrivate(page, secret);
  await checked('welcome version, write-only official credential, immediate readiness, reload, and revised-copy acknowledgement');

  await page.addInitScript(() => {
    window.__takeoverSightings = [];
    setInterval(() => {
      if (document.querySelector('[role="dialog"][aria-label="内测声明"], [role="dialog"][aria-label="添加一个 API Key 开始使用"]')) window.__takeoverSightings.push('chrome');
      if (document.getElementById('root')?.inert) window.__takeoverSightings.push('inert');
    }, 8);
  });
  let released = false; const held = []; const release = () => { released = true; for (const resolve of held.splice(0)) resolve(); };
  await page.route('**/api/settings.describe', async route => { if (!released) await new Promise(resolve => held.push(resolve)); await route.continue(); });
  const warningStart = warnings.length;
  try {
    await page.reload({ waitUntil: 'commit' }); await page.locator('[class*="frame"]').waitFor();
    await page.waitForTimeout(600); release(); await page.waitForTimeout(400);
  } finally { release(); await page.unroute('**/api/settings.describe'); }
  warnings.push(...warnings.splice(warningStart).filter(value => !/connection lost/i.test(value)));
  assert.deepEqual(await page.evaluate(() => window.__takeoverSightings), []);
  await expect(welcome).toHaveCount(0); await expect(credential).toHaveCount(0);
  await checked('configured reload stays interactive while real settings responses are held');

  settings = await openModels(page); await settings.getByRole('button', { name: '编辑 DeepSeek (deepseek-official)', exact: true }).click(); await settings.getByText('自定义设置', { exact: true }).click();
  await settings.getByRole('button', { name: /删除模型/ }).first().click(); await settings.getByRole('button', { name: '添加模型', exact: true }).click();
  const custom = settings.getByLabel('模型 ID 2', { exact: true }); await custom.fill('private-preview'); await settings.getByLabel('显示名称 2', { exact: true }).fill('Private Preview');
  await settings.getByRole('button', { name: '容量 2', exact: true }).click(); await settings.getByLabel('上下文窗口 2', { exact: true }).fill('131072'); await settings.getByLabel('最大输出 token 数 2', { exact: true }).fill('64K');
  await golden(settings, 'onboarding-deepseek-config', 'models'); await settings.getByRole('button', { name: '保存', exact: true }).click(); await expect(custom).toHaveCount(0);
  const storedModels = (await document(official, 'settings.yaml'))['llm-deepseek'].models;
  assert.deepEqual(storedModels.map(model => model.id), ['deepseek-v4-pro', 'private-preview']);
  assert.equal(storedModels[1].name, 'Private Preview'); assert.equal(storedModels[1].contextWindow, 131072); assert.equal(storedModels[1].maxTokens, 64000);
  await page.keyboard.press('Escape'); const selectedWorkspace = join(official.workspace, 'model-fallback-e2e'); await mkdir(selectedWorkspace);
  await page.getByRole('textbox', { name: '选择工作区', exact: true }).click(); const picker = page.getByRole('dialog', { name: '选择工作区目录', exact: true });
  await picker.getByRole('button', { name: '编辑路径', exact: true }).click(); const path = picker.getByRole('textbox', { name: '编辑路径', exact: true });
  await path.fill(selectedWorkspace); await path.press('Enter'); await picker.getByRole('button', { name: '打开', exact: true }).click(); await page.locator('textarea:enabled[placeholder="描述你想要构建的内容"]').waitFor();
  await page.getByRole('button', { name: '选择模型', exact: true }).click(); await page.getByRole('menuitem', { name: /模型/ }).click();
  await expect(page.getByText('deepseek-v4-flash', { exact: true })).toHaveCount(0); await page.getByRole('menuitemradio', { name: 'Private Preview', exact: true }).waitFor();
  await exec('agent-browser', ['--session', official.browserSession, '--cdp', official.cdp, 'screenshot', '--annotate', join(output, 'custom-model.png')]);
  await checked('real DeepSeek model edits persist capacities and replace the removed selection');
  assert.deepEqual((await readdir(join(source, 'apps/web/tests/snapshots/onboarding-deepseek-config'))).sort(), ['missing.expected.md', 'models.expected.md', 'welcome.expected.md']);
  await checked('official-provider fixture inventory remains closed');

  const other = await start('other'); page = other.page;
  await page.getByRole('dialog', { name: '内测声明', exact: true }).getByRole('button', { name: '继续', exact: true }).click();
  const otherCredential = page.getByRole('dialog', { name: credentialTitle, exact: true }); await otherCredential.getByRole('button', { name: '稍后配置', exact: true }).click(); await expect(otherCredential).toHaveCount(0);
  settings = await openModels(page); await settings.getByRole('textbox', { name: 'API 密钥', exact: true }).waitFor();
  await settings.getByRole('button', { name: '添加提供方', exact: true }).click(); await settings.getByLabel('提供方', { exact: true }).selectOption('minimax-cn');
  await expect(settings.getByRole('textbox', { name: 'API 密钥', exact: true })).toHaveCount(2); await settings.getByRole('button', { name: '取消', exact: true }).first().click();
  await expect(settings.getByLabel('提供方', { exact: true })).toHaveCount(1); await expect(settings.getByRole('textbox', { name: 'API 密钥', exact: true })).toHaveCount(1);
  await settings.getByRole('button', { name: '编辑 DeepSeek (deepseek-official)', exact: true }).waitFor(); await golden(settings, 'onboarding-usable-provider', 'dismissed');
  await checked('dismissing the official setup card preserves the independent add-provider draft');
  await settings.getByRole('textbox', { name: 'API 密钥', exact: true }).fill('sk-e2e-minimax'); await settings.getByRole('button', { name: '保存', exact: true }).click(); await settings.getByText('已保存 minimax-cn。', { exact: true }).waitFor();
  assert.equal((await document(other, 'settings.yaml'))['llm-pi-ai'].providers['minimax-cn'].apiKeyEnv, 'MINIMAX_CN_API_KEY');
  const credentials = await document(other, '.credentials.yaml'); assert.equal(credentials.MINIMAX_CN_API_KEY, 'sk-e2e-minimax'); assert(!Object.hasOwn(credentials, 'DEEPSEEK_API_KEY'));
  await reload(page); await expect(otherCredential).toHaveCount(0); assert.equal(await page.locator('#root').evaluate(root => root.inert), false);
  settings = await openModels(page); await settings.getByRole('button', { name: '编辑 DeepSeek (deepseek-official)', exact: true }).waitFor();
  await expect(settings.getByRole('textbox', { name: 'API 密钥', exact: true })).toHaveCount(0); await assertPrivate(page, 'sk-e2e-minimax');
  await checked('another usable provider ends onboarding without an official credential');
  assert.deepEqual(await readdir(join(source, 'apps/web/tests/snapshots/onboarding-usable-provider')), ['dismissed.expected.md']);
  assert.deepEqual(warnings, []); await checked('second-provider fixture inventory and browser wire remain clean');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ errors, warnings, checks, hostErrors: hosts.map(item => item.stderr) }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  for (const item of hosts) if (item.browserSession) await exec('agent-browser', ['--session', item.browserSession, 'close']).catch(error => failures.push(error));
  for (const context of contexts) await context.close().catch(error => failures.push(error));
  for (const item of hosts) try {
    if (item.child.exitCode === null) { const exited = once(item.child, 'exit'); item.child.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, item.stderr); }
    assert.equal(JSON.parse(await readFile(join(item.home, 'model-call-audit.json'), 'utf8')).calls, 0);
  } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'onboarding and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ checks, warnings, modelCalls: 0 }, null, 2));
"#;
