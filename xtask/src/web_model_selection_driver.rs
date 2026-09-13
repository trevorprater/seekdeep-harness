//! Shared model defaults and declared reasoning through real browser and Host services.

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
const exec = promisify(execFile), hosts = [], checks = [], errors = [], warnings = [];
let context, page, cdp, runError, sequence = 0;
async function start(label, overlay) {
  const home = join(world, label, 'home'), workspace = join(world, label, 'workspace');
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  const child = spawn(host, [home, workspace, label, home, 'route-only', join(source, 'apps/web/tests', overlay)], { cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const item = { child, home, workspace, stderr: '' }; hosts.push(item); child.stderr.on('data', value => { item.stderr += value; });
  item.origin = await new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + item.stderr)), 30000);
    child.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    child.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + item.stderr)); });
  });
  return item;
}
async function invoke(item, method, payload, acceptFailure = false) {
  const rpcId = 'model-selection-' + ++sequence;
  const response = await fetch(item.origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) });
  assert(response.ok); const body = await response.json(); assert.equal(body.rpcId, rpcId);
  if (acceptFailure) return body.result;
  assert.equal(body.result.ok, true, JSON.stringify(body)); return body.result.value;
}
const settingsDocument = async item => parseYaml(await readFile(join(item.home, 'settings.yaml'), 'utf8'));
const createSession = async (item, sessionId) => (await invoke(item, 'session.create', { sessionId, cwd: item.workspace })).sessionId;
const currentOf = async (item, sessionId) => (await invoke(item, 'session.models', { sessionId })).current;
async function open(item) {
  page = await context.newPage(); page.setDefaultTimeout(15000);
  page.on('pageerror', error => errors.push(String(error)));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text()); });
  await page.goto(item.origin); await page.getByRole('button', { name: '继续', exact: true }).click();
  const workspace = join(item.workspace, 'workspace'); await mkdir(workspace);
  await page.getByRole('textbox', { name: '选择工作区', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '选择工作区目录', exact: true });
  await dialog.getByRole('button', { name: '编辑路径', exact: true }).click(); const path = dialog.getByRole('textbox', { name: '编辑路径', exact: true });
  await path.fill(workspace); await path.press('Enter'); await dialog.getByRole('button', { name: '打开', exact: true }).click();
  await page.locator('textarea:enabled[placeholder="描述你想要构建的内容"]').waitFor();
}
async function checked(name) { assert.deepEqual(errors, []); assert.deepEqual(warnings, []); checks.push(name); console.log('model-selection: ' + name); }
async function screenshot(name) { await exec('agent-browser', ['--session', 'seekdeep-model-selection', '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '.png')]); }
try {
  const profile = join(world, 'browser');
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'zh-CN', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  for (const blank of context.pages()) await blank.close();
  const defaults = await start('defaults', 'default-model.overlay.yml');
  await invoke(defaults, 'settings.update', { ns: 'llm-pi-ai', patch: { providers: {
    'origin-gateway': { displayName: 'Origin Gateway', api: 'openai-completions', baseURL: 'https://gateway.origin.example/v1', models: [{ id: 'origin-large', name: 'Origin Large' }] },
    'acme-gateway': { displayName: 'Acme Gateway', api: 'openai-completions', baseURL: 'https://gateway.acme.example/v1', models: [{ id: 'acme-large', name: 'Acme Large' }] },
  } } });
  await open(defaults);
  const logged = await createSession(defaults, 'default-model-logged'); assert.deepEqual(await currentOf(defaults, logged), { provider: 'origin-gateway', model: 'origin-large' });
  const seeded = await fetch(defaults.origin + '/fixture/session/' + logged, { method: 'PUT' }); assert(seeded.ok);
  const events = await seeded.json(); assert.deepEqual(events.findLast(event => event.type === 'request/header')?.data.header.config, { provider: 'origin-gateway', model: 'origin-large' });
  let trigger = page.getByRole('button', { name: /^选择模型/ }); await trigger.click(); await page.getByRole('menuitem', { name: /模型/ }).click(); await page.getByRole('menuitemradio', { name: 'Acme Large', exact: true }).click();
  await expect.poll(async () => (await settingsDocument(defaults))['agent-default-model']).toEqual({ provider: 'acme-gateway', model: 'acme-large' });
  assert.deepEqual(await currentOf(defaults, await createSession(defaults, 'default-model-after')), { provider: 'acme-gateway', model: 'acme-large' });
  assert.deepEqual(await currentOf(defaults, logged), { provider: 'origin-gateway', model: 'origin-large' });
  await checked('composer choice persists the default while a logged session keeps its own route');

  const box = page.locator('textarea[data-input-phase], textarea').first(); await expect(box).toBeEnabled();
  await invoke(defaults, 'settings.replace', { ns: 'llm-pi-ai', section: { providers: {} } });
  await expect(box).toBeDisabled(); await expect(box).toHaveAttribute('placeholder', '当前模型不可用，请先选择模型');
  const refused = await invoke(defaults, 'session.prompt', { sessionId: await createSession(defaults, 'default-model-refusal'), mode: 'queue', content: [{ type: 'text', text: 'hi' }] }, true);
  assert.equal(refused.ok, false); assert.equal(refused.error.code, 'model-unavailable');
  await expect(trigger).toBeEnabled(); await trigger.click(); await page.getByRole('menuitem', { name: /模型/ }).click(); await page.getByRole('menuitemradio').first().click(); await expect(box).toBeEnabled();
  await screenshot('recovered-model'); await checked('unavailable route blocks both composer and Host while model selection permits recovery');
  page.removeAllListeners('console'); page.removeAllListeners('pageerror'); await page.close();

  const reasoning = await start('reasoning', 'declared-reasoning.overlay.yml');
  await invoke(reasoning, 'settings.update', { ns: 'llm-pi-ai', patch: { providers: { 'acme-gateway': { displayName: 'Acme Gateway', api: 'openai-completions', baseURL: 'https://gateway.acme.example/v1', models: [{ id: 'acme-think', name: 'Acme Think', reasoningEfforts: { off: null, high: 'high', max: 'ultra' } }] } } } });
  await open(reasoning); trigger = page.getByRole('button', { name: /^选择模型/ }); await trigger.click(); await page.getByRole('menuitem', { name: /推理等级/ }).click();
  await expect.poll(() => page.getByRole('menuitemradio').allTextContents()).toEqual(['Default', 'Off', 'High', 'Max']);
  const menu = page.getByRole('menu', { name: '模型与推理等级', exact: true }); let previous = (await menu.ariaSnapshot()).trim();
  await expect.poll(async () => { const current = (await menu.ariaSnapshot()).trim(); const stable = current === previous; previous = current; return stable; }).toBe(true);
  await writeFile(join(output, 'reasoning.actual.md'), previous + '\n'); assert.equal(previous + '\n', await readFile(join(source, 'apps/web/tests/snapshots/declared-reasoning/ui.expected.md'), 'utf8'));
  await screenshot('reasoning-levels'); await page.getByRole('menuitemradio', { name: 'High', exact: true }).click();
  await expect.poll(async () => (await settingsDocument(reasoning))['agent-default-model']).toEqual({ provider: 'acme-gateway', model: 'acme-think', reasoningEffort: 'high' });
  await expect(trigger).toHaveAttribute('aria-label', '选择模型，当前 Acme Think，推理等级 High');
  await checked('exact declared reasoning levels and source menu golden, with selection persisted in the Agent default');
  assert.deepEqual(await readdir(join(source, 'apps/web/tests/snapshots/declared-reasoning')), ['ui.expected.md']); await checked('closed reasoning fixture inventory and clean browser wire');
} catch (error) {
  runError = error; await page?.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
  if (page && !page.isClosed()) await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  await writeFile(join(output, 'failures.json'), JSON.stringify({ errors, warnings, checks, hostErrors: hosts.map(item => item.stderr) }, null, 2));
} finally {
  const failures = runError ? [runError] : [];
  if (cdp) await exec('agent-browser', ['--session', 'seekdeep-model-selection', 'close']).catch(error => failures.push(error));
  await context?.close().catch(error => failures.push(error));
  for (const item of hosts) try {
    if (item.child.exitCode === null) { const exited = once(item.child, 'exit'); item.child.kill('SIGINT'); const [code] = await exited; assert.equal(code, 0, item.stderr); }
    assert.equal(JSON.parse(await readFile(join(item.home, 'model-call-audit.json'), 'utf8')).calls, 0);
  } catch (error) { failures.push(error); }
  if (failures.length === 1) throw failures[0]; if (failures.length) throw new AggregateError(failures, 'model selection and cleanup failed');
}
await writeFile(join(output, 'result.json'), JSON.stringify({ checks, warnings, modelCalls: 0 }, null, 2));
"#;
