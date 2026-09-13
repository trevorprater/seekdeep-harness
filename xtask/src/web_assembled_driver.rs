//! Real built application and Host over a durable recorded Session.

pub(super) const DRIVER: &str = r"import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { zstdDecompressSync } from 'node:zlib';
import { join } from 'node:path';
const [source, host, home, workspace, output, sessionId, mode, artifact] = process.argv.slice(2);
const { chromium } = createRequire(join(source, 'apps/web/package.json'))('playwright');
function decodeFrames(bytes) {
  const chunks = [];
  let offset = 0;
  while (offset < bytes.length) {
    const decoded = zstdDecompressSync(bytes.subarray(offset), { info: true });
    const consumed = decoded.engine.bytesWritten;
    if (!Number.isSafeInteger(consumed) || consumed <= 0 || consumed > bytes.length - offset) throw new Error('Zstandard decoder made invalid progress');
    chunks.push(decoded.buffer);
    offset += consumed;
  }
  return Buffer.concat(chunks).toString('utf8');
}
let server, browser;
try {
  server = spawn(host, ['web', '--host', '127.0.0.1', '--port', '0'], {
    cwd: process.cwd(), env: { ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_AGENTS_HOME: join(home, 'agents'), SEEKDEEP_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  server.stderr.on('data', data => { stderr += data; });
  const origin = await new Promise((resolve, reject) => {
    let stdout = '';
    const deadline = setTimeout(() => reject(new Error('Rust Host readiness timed out: ' + stderr)), 30000);
    server.stdout.on('data', data => { stdout += data; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(deadline); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(deadline); reject(new Error(`Host exited ${code}: ${stderr}`)); });
  });
  let sequence = 0;
  const invoke = async (method, payload) => {
    const rpcId = `assembled-${++sequence}`;
    const response = await fetch(origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) });
    const text = await response.text();
    if (!response.ok) throw new Error(`${method}: HTTP ${response.status}: ${text}`);
    const body = JSON.parse(text);
    if (body.rpcId !== rpcId || !body.result.ok) throw new Error(`${method}: ${text}`);
    return body.result.value;
  };
  await invoke('workspace.list', {});
  const created = await invoke('workspace.create', { path: workspace });
  const baseline = await invoke('workspace.list', {});
  if (!baseline.items.some(row => row.workspaceId === created.workspace.workspaceId)) throw new Error('created workspace is absent from the durable baseline');
  const history = await invoke('session.history', { sessionId });
  if (!JSON.stringify(history).includes('FIRST_DONE')) throw new Error('Host did not load the recorded session');
  const search = await invoke('session.search', { query: 'WATERFALL' });
  if (!search.items.some(row => row.sessionId === sessionId)) throw new Error('Host content search did not find the recorded session: ' + JSON.stringify(search));
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ locale: 'en-US', viewport: { width: 1280, height: 900 } });
  page.setDefaultTimeout(30000);
  const failures = [];
  let rejectPage;
  const pageFailure = new Promise((resolve, reject) => { rejectPage = reject; });
  pageFailure.catch(() => {});
  const failed = message => { failures.push(message); rejectPage(new Error(message)); };
  page.on('pageerror', error => failed(error.stack ?? error.message));
  page.on('console', message => { if (message.type() === 'error') failed(message.text()); });
  page.on('response', response => { if (response.status() >= 400) failed(`${response.status()} ${response.url()}`); });
  try {
    await Promise.race([(async () => {
      await page.goto(origin);
      await page.getByRole('button', { name: 'Continue', exact: true }).click();
      await page.getByRole('button', { name: 'Configure later', exact: true }).click();
      await page.getByRole('tree', { name: 'Sessions', exact: true }).waitFor();
      await page.getByRole('button', { name: 'Search sessions', exact: true }).click();
      await page.getByPlaceholder('Search sessions...', { exact: true }).fill('WATERFALL');
      const matches = page.getByRole('tree', { name: 'Search results', exact: true }).getByRole('treeitem');
      await matches.first().waitFor();
      if (await matches.count() !== 1) throw new Error('recorded session search must resolve exactly one result');
      await matches.click();
      await page.getByText('FIRST_DONE', { exact: true }).waitFor();
      await page.getByRole('heading', { name: 'Navigation Summary', exact: true }).waitFor();
      await page.screenshot({ path: join(output, 'conversation.png'), fullPage: true });
      await page.reload();
      await page.getByText('FIRST_DONE', { exact: true }).waitFor();
      let exported;
      if (mode === 'export') {
        exported = [];
        for (const entrypoint of ['header', 'command']) {
          const methods = [];
          const observe = request => { if (new URL(request.url()).pathname === '/api/session.export') methods.push(request.method()); };
          page.context().on('request', observe);
          const pending = page.waitForEvent('download');
          const before = decodeFrames(await readFile(artifact));
          if (entrypoint === 'header') {
            await page.getByRole('button', { name: 'Session log', exact: true }).click();
          } else {
            const input = page.locator('textarea').first();
            await input.fill('/export');
            await input.press('Enter');
          }
          const download = await pending;
          const downloadUrl = new URL(download.url());
          if (downloadUrl.origin !== new URL(origin).origin || downloadUrl.pathname !== '/api/session.export'
            || downloadUrl.searchParams.get('sessionId') !== sessionId || downloadUrl.searchParams.get('includeDescendants') !== 'true') throw new Error('incorrect browser download URL: ' + download.url());
          if (download.suggestedFilename() !== `seekdeep-session-${sessionId}.zip`) throw new Error('incorrect Session archive filename');
          const filename = join(output, `session-${entrypoint}.zip`);
          await download.saveAs(filename);
          const { unzipSync, strFromU8 } = createRequire(join(source, 'apps/web/package.json'))('fflate');
          const entries = unzipSync(await readFile(filename));
          if (Object.keys(entries).join(',') !== 'session.jsonl') throw new Error('unexpected Session archive entries: ' + Object.keys(entries));
          const persisted = decodeFrames(await readFile(artifact));
          if (!persisted.startsWith(await readFile(join(output, 'expected-session.jsonl'), 'utf8'))) throw new Error('opening the session rewrote its recorded history');
          if (strFromU8(entries['session.jsonl']) !== persisted) throw new Error('export changed persisted Session bytes');
          if (entrypoint === 'command') {
            if (!persisted.startsWith(before)) throw new Error('command rewrote persisted history');
            const events = persisted.slice(before.length).trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
            if (events.filter(event => event.type === 'command/run').length !== 1 || events.filter(event => event.type === 'command/done').length !== 1) throw new Error('export must append one durable command lifecycle: ' + JSON.stringify(events));
            if (events.some(event => event.type === 'turn/start' || event.type === 'request/header')) throw new Error('export started a model turn');
            await page.getByText('Session log download requested.', { exact: true }).waitFor();
          }
          const dialog = page.getByRole('dialog', { name: 'Session download started', exact: true });
          await dialog.waitFor();
          await page.screenshot({ path: join(output, `export-${entrypoint}-success.png`), fullPage: true });
          await dialog.getByText('Close', { exact: true }).click();
          await dialog.waitFor({ state: 'detached' });
          // Chromium may omit browser-owned downloads from request events; Download supplies their URL and bytes.
          if (methods.filter(method => method === 'HEAD').length !== 1 || methods.some(method => method !== 'HEAD' && method !== 'GET')
            || methods.filter(method => method === 'GET').length > 1) throw new Error('unexpected export preflight requests: ' + methods);
          page.context().off('request', observe);
          exported.push({ entrypoint, exactBytes: true, preflight: 'HEAD', observedMethods: methods, browserDownload: download.url(), filename: download.suggestedFilename() });
        }
        await page.reload();
        await page.getByText('Session log download requested.', { exact: true }).waitFor();
      }
      if (failures.length) throw new Error(failures.join('\n'));
      console.log(JSON.stringify({ browser: browser.version(), workspace: true, persistedHistory: true, renderedConversation: true, reload: true, search: 'first-search', sessionId, ...(exported ? { exported } : {}) }));
    })(), pageFailure]);
  } catch (error) {
    await page.screenshot({ path: join(output, 'failure.png'), fullPage: true });
    console.error(await page.locator('body').innerText());
    console.error(JSON.stringify({ failures, baseline, historyKeys: Object.keys(history) }, null, 2));
    throw error;
  }
} finally {
  await browser?.close();
  if (server && server.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); await exited; }
}
";
