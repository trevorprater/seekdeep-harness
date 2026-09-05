//! Real-browser protocol verification; no registry, gateway, or transport substitutes.

pub(super) const DRIVER: &str = r#"import { createRequire } from 'node:module';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const [source, host, output, typedConsumer] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright');
const root = process.cwd();
const home = await mkdtemp(join(tmpdir(), 'seekdeep-remote-browser-'));
let server, browser;
try {
  server = spawn(host, ['web', '--host', '127.0.0.1', '--port', '0'], {
    cwd: root, env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  server.stderr.on('data', data => { stderr += data; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = '';
    const deadline = setTimeout(() => reject(new Error('Rust Host readiness timed out: ' + stderr)), 30000);
    server.stdout.on('data', data => { stdout += data; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(deadline); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(deadline); reject(new Error(`Host exited ${code}: ${stderr}`)); });
  });
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  page.setDefaultTimeout(30000);
  page.on('pageerror', error => console.error('browser error:', error.message));
  const requests = [];
  const responseReads = [];
  const hostErrors = [];
  page.on('request', request => { if (request.method() === 'POST' && request.url().startsWith(origin + '/api/')) requests.push(request.url().slice(origin.length)); });
  page.on('response', response => {
    if (response.url().startsWith(origin + '/api/')) responseReads.push(response.json().then(body => { if (body.result && !body.result.ok) hostErrors.push(body.result.error); }).catch(() => {}));
  });
  await page.goto(origin + '/api/__remote_path_probe__');
  await page.setContent('<!doctype html><html><head></head><body></body></html>');
  const assets = {};
  for (const path of ['vendor/cordis/lib/client.js', 'vendor/cordis/lib/index.js', 'packages/typert/registry/lib/client.js', 'packages/client/connection/lib/client.js', 'packages/api/gateway/lib/client.js', 'packages/api/remotes/lib/client.js']) assets[path] = await readFile(join(root, path), 'utf8');
  const bytes = (await readFile(join(root, 'vendor/cordis/lib/client_bg.wasm'))).toString('base64');
  const publicContracts = [];
  let typedSource;
  let zodSource;
  if (typedConsumer) {
    typedSource = await readFile(typedConsumer, 'utf8');
    zodSource = await readFile(join(output, 'zod.mjs'), 'utf8');
    const model = JSON.parse(await readFile(join(root, 'crates/api-remotes-client/contracts/host-model.json'), 'utf8'));
    for (const pkg of model.face.packages) publicContracts.push({ name: pkg.name.replace('@deepseek-ai/dsh-', '@seekdeep-ai/seekdeep-'), code: await readFile(join(root, pkg.root, 'lib/typert.remote-client.js'), 'utf8') });
  }
  await page.evaluate(async ({ assets, bytes, publicContracts, typedSource, zodSource }) => {
    const blob = text => URL.createObjectURL(new Blob([text], { type: 'text/javascript' }));
    const binding = blob(assets['vendor/cordis/lib/client.js']);
    const module = blob(assets['vendor/cordis/lib/index.js'].replace("'./client.js'", JSON.stringify(binding)).replace("new URL('./client_bg.wasm', import.meta.url)", `Uint8Array.from(atob(${JSON.stringify(bytes)}), c => c.charCodeAt(0))`));
    const cordis = await import(module);
    const handoffs = new Map();
    window.__ModuleLoader__ = { load(row) { handoffs.set(row.id, row); } };
    for (const path of Object.keys(assets).filter(path => path.startsWith('packages/'))) {
      const script = document.createElement('script'); script.textContent = assets[path]; document.head.append(script);
    }
    const client = new cordis.Context();
    for (const id of ['@seekdeep-ai/seekdeep-typert-registry', '@seekdeep-ai/seekdeep-client-connection', '@seekdeep-ai/seekdeep-api-gateway', '@seekdeep-ai/seekdeep-api-remotes']) {
      const row = handoffs.get(id); if (!row) throw new Error('missing built module ' + id);
      const plugin = row.factory(); await client.plugin(plugin);
    }
    window.remotePathClient = client;
    if (typedSource) {
      const zodUrl = blob(zodSource);
      const importMap = document.createElement('script');
      importMap.type = 'importmap'; importMap.textContent = JSON.stringify({ imports: { zod: zodUrl } }); document.head.append(importMap);
      const contributions = [];
      for (const contract of publicContracts) {
        const url = blob(contract.code);
        const value = await import(url);
        if (Object.keys(value).sort().join(',') !== 'TYPERT_REMOTE,default' || value.default !== value.TYPERT_REMOTE || value.default.package !== contract.name) throw new Error('invalid public Remote module ' + contract.name);
        contributions.push(value.default);
        URL.revokeObjectURL(url);
      }
      const url = blob(typedSource);
      const typed = await import(url);
      window.remotePathTyped = { Context: cordis.Context, handoffs, contributions, run: typed.typedRemote };
      URL.revokeObjectURL(url);
      URL.revokeObjectURL(zodUrl);
    }
    URL.revokeObjectURL(binding); URL.revokeObjectURL(module);
  }, { assets, bytes, publicContracts, typedSource, zodSource });
  const result = await page.evaluate(async ({ root }) => {
    const client = window.remotePathClient;
    const assert = (value, message) => { if (!value) throw new Error(message); };
    let sequence = 0;
    const callHost = async (method, payload) => {
      const rpcId = `setup-${++sequence}`;
      const response = await fetch('/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) });
      const result = await response.json(); assert(result.rpcId === rpcId && result.result.ok, 'Host setup failed: ' + JSON.stringify(result)); return result.result.value;
    };
    const first = await callHost('session.create', { cwd: root });
    const second = await callHost('session.create', { cwd: root });
    client.typert.contexts.registerClient('agent', { identity: ctx => ctx.agentId });
    const descriptors = client.typert.remotes.list();
    assert(descriptors.length === 24, 'incomplete Remote descriptor inventory');
    assert(descriptors.every(d => d.result.typeSymbol && d.sourceLocation && d.parameters.every(p => p.codec.typeSymbol)), 'incomplete generated descriptor metadata');
    let invalidRejected = false;
    try { await client.remote.goals.create(first.sessionId, { objective: 1 }); } catch { invalidRejected = true; }
    assert(invalidRejected, 'invalid input accepted');
    const created = await client.remote.goals.create(first.sessionId, { objective: 'integrated root goal' });
    assert(created.ok, 'Goal creation failed: ' + JSON.stringify(created));
    const edited = await client.remote.goals.edit(first.sessionId, created.value.ref, { objective: 'edited integrated goal' });
    assert(edited.ok && edited.value.revision === 2, 'Goal edit failed: ' + JSON.stringify(edited));
    const scoped = client.extend({ agentId: second.sessionId });
    const scopedGoal = await scoped.remote.goals.create({ objective: 'integrated scoped goal', maxGoalRounds: 3 });
    assert(scopedGoal.ok, 'scoped Goal creation failed: ' + JSON.stringify(scopedGoal));
    const commands = await client.remote.commands.list(first.sessionId);
    assert(commands.ok && Array.isArray(commands.value), 'command listing failed: ' + JSON.stringify(commands));
    const unknownCommand = await client.remote.commands.execute(first.sessionId, '/__remote_path_unknown_command__');
    assert(unknownCommand.ok && unknownCommand.value === undefined, 'undefined command result was not preserved: ' + JSON.stringify(unknownCommand));
    const cancellation = new AbortController(); cancellation.abort(new Error('remote path cancelled'));
    const cancelled = await client.remote.commands.execute(first.sessionId, '/__remote_path_unknown_command__', cancellation.signal);
    assert(!cancelled.ok && cancelled.error.code === 'internal', 'cancelled call escaped the error branch');
    const failed = await client.remote.goals.edit(first.sessionId, { id: 'missing-goal', revision: 1 }, { objective: 'must fail' });
    assert(!failed.ok, 'missing Goal unexpectedly succeeded');
    const inventory = await client.remote.dynamicCordisRunner.inventory();
    assert(inventory.ok, 'dynamic inventory failed: ' + JSON.stringify(inventory));
    const plugins = await client.remote.pluginInventory.list();
    assert(plugins.ok, 'plugin inventory failed: ' + JSON.stringify(plugins));
    const feedback = await client.remote.messageFeedback.list({ sessionId: first.sessionId });
    assert(feedback.ok, 'feedback listing failed: ' + JSON.stringify(feedback));
    const firstHistory = await callHost('session.history', { sessionId: first.sessionId });
    const secondHistory = await callHost('session.history', { sessionId: second.sessionId });
    const count = history => history.events.filter(entry => entry.event.type === 'goal/change').length;
    assert(count(firstHistory) === 2 && count(secondHistory) === 1, 'durable Goal events differ');
    let typed;
    if (window.remotePathTyped) {
      const runtime = window.remotePathTyped;
      const consumer = new runtime.Context();
      for (const id of ['@seekdeep-ai/seekdeep-typert-registry', '@seekdeep-ai/seekdeep-client-connection', '@seekdeep-ai/seekdeep-api-gateway']) await consumer.plugin(runtime.handoffs.get(id).factory());
      for (const contribution of runtime.contributions) await consumer.remote.$mount(contribution);
      const registry = consumer.typert;
      assert(registry.remotes.list().length === 24, 'public package contributions are incomplete');
      const session = await callHost('session.create', { cwd: root });
      const result = await runtime.run(consumer, session.sessionId);
      assert(result.revision === 2 && result.commands > 0, 'checked public consumer failed');
      const history = await callHost('session.history', { sessionId: session.sessionId });
      assert(count(history) === 2, 'checked public consumer did not persist both Goal operations');
      await consumer.fiber.dispose();
      assert(registry.remotes.list().length === 0, 'public contributions survived teardown');
      typed = { ...result, packages: runtime.contributions.length, goalEvents: count(history), remainingDescriptors: registry.remotes.list().length };
    }
    const registry = client.typert;
    await client.fiber.dispose();
    assert(registry.remotes.list().length === 0, 'Remote descriptors survived teardown');
    return { descriptors: descriptors.length, invalidRejected, undefinedPreserved: true, cancellation: cancelled.error, hostFailure: failed.error, rootGoalEvents: count(firstHistory), scopedGoalEvents: count(secondHistory), commands: commands.value.length, namespaceCalls: 5, remainingDescriptors: registry.remotes.list().length, ...(typed ? { typed } : {}) };
  }, { root });
  const goalCreates = requests.filter(path => path === '/api/goals/create').length;
  if (goalCreates !== (typedConsumer ? 3 : 2)) throw new Error('invalid request reached Host or valid request was lost: ' + JSON.stringify(requests));
  if (typedConsumer && !result.typed) throw new Error('checked consumer was not exercised');
  await Promise.all(responseReads);
  if (!hostErrors.some(error => JSON.stringify(error) === JSON.stringify(result.hostFailure))) throw new Error('gateway did not preserve the Host error verbatim');
  if (requests.filter(path => path === '/api/commands/execute').length !== 1) throw new Error('pre-aborted command reached the Host');
  await page.evaluate(result => { const pre = document.createElement('pre'); pre.textContent = JSON.stringify(result, null, 2); document.body.replaceChildren(pre); }, result);
  await page.screenshot({ path: join(output, 'remote-path.png'), fullPage: true });
  console.log(JSON.stringify({ ...result, browser: await browser.version(), requests }));
} finally {
  if (browser) await browser.close();
  if (server && server.exitCode === null) { server.kill('SIGINT'); await once(server, 'exit'); }
  await rm(home, { recursive: true, force: true });
}
"#;
