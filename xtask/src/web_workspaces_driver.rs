//! Source workspace gestures and durable assertions over an isolated real Host.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdir, readFile, writeFile, stat, rename, readdir } from 'node:fs/promises';
import { join, sep } from 'node:path';
const [source, host, home, workspace, output, sessionId, artifact] = process.argv.slice(2);
const require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright');
const { expect } = require('playwright/test');
const exec = promisify(execFile);
const failures = [], warnings = [], requests = [], checkpoints = [];
let server, context, page, cdpPort, stderr = '';
async function checkpoint(name) {
  assert.deepEqual(failures, []);
  checkpoints.push(name);
  await writeFile(join(output, 'result.json'), JSON.stringify({ checkpoints, requests }, null, 2));
  console.log('workspace: ' + name);
}
async function annotated(name) {
  await exec('agent-browser', ['--session', 'seekdeep-workspace-parity', '--cdp', cdpPort, 'screenshot', '--annotate', join(output, name + '.png')]);
}
async function startHost() {
  server = spawn(host, [home, workspace, sessionId], {
    cwd: process.cwd(), env: { ...process.env, HOME: workspace, SEEKDEEP_HOME: home, SEEKDEEP_AGENTS_HOME: join(home, 'agents'), SEEKDEEP_TELEMETRY_DISABLED: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  server.stderr.on('data', data => { stderr += data; });
  return await new Promise((resolve, reject) => {
    let stdout = '';
    const deadline = setTimeout(() => reject(new Error('Host readiness timed out: ' + stderr)), 30000);
    server.stdout.on('data', data => { stdout += data; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(deadline); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(deadline); reject(new Error('Host exited ' + code + ': ' + stderr)); });
  });
}
async function stopHost() {
  if (server && server.exitCode === null) {
    const exited = once(server, 'exit'); server.kill('SIGINT');
    const [code] = await exited;
    if (code !== 0) throw new Error('fixture Host shutdown failed: ' + stderr);
  }
}
try {
  // Source seeding follows Host boot, so startup migration must not adopt its cwd.
  const stagedSeed = join(home, 'staged-seed');
  await rename(artifact, stagedSeed);
  let origin = await startHost();
  let sequence = 0;
  async function invoke(method, payload = {}) {
    const rpcId = 'workspace-check-' + ++sequence;
    const response = await fetch(origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) });
    const body = await response.json();
    assert.equal(response.ok, true, JSON.stringify(body));
    assert.equal(body.rpcId, rpcId);
    assert.equal(body.result.ok, true, JSON.stringify(body));
    return body.result.value;
  }
  const list = async () => (await invoke('workspace.list')).items;
  const byPath = async path => (await list()).find(row => row.path === path);
  assert.deepEqual(await list(), []);
  await rename(stagedSeed, artifact);
  const profile = join(home, 'browser-profile');
  context = await chromium.launchPersistentContext(profile, { headless: true, locale: 'en-US', viewport: { width: 1680, height: 1000 }, args: ['--remote-debugging-port=0'] });
  const browserVersion = context.browser().version();
  cdpPort = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  page = context.pages()[0] ?? await context.newPage();
  page.setDefaultTimeout(15000);
  page.on('pageerror', error => failures.push(error.stack ?? error.message));
  page.on('console', message => {
    if (message.type() === 'error') failures.push(message.text());
    if (/connection lost|gap repair|discontinuous/i.test(message.text())) warnings.push(message.text());
  });
  page.on('request', request => { const path = new URL(request.url()).pathname; if (path.startsWith('/api/')) requests.push({ path, method: request.method(), payload: request.postData() }); });
  await page.goto(origin);
  await page.getByRole('button', { name: 'Continue', exact: true }).click();
  await page.getByRole('tree', { name: 'Sessions', exact: true }).waitFor();
  async function reload() {
    const start = warnings.length;
    await page.reload();
    await page.getByRole('tree', { name: 'Sessions', exact: true }).waitFor();
    warnings.push(...warnings.splice(start).filter(text => !/connection lost/i.test(text)));
  }
  async function browseTo(path) {
    await page.getByRole('button', { name: 'Add workspace', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Select Workspace Directory', exact: true });
    await dialog.waitFor();
    await dialog.getByRole('button', { name: 'Edit path', exact: true }).click();
    await dialog.getByLabel('Edit path', { exact: true }).fill(path);
    await dialog.getByLabel('Edit path', { exact: true }).press('Enter');
    return dialog;
  }
  async function adopt(path) {
    const dialog = await browseTo(path);
    await dialog.getByRole('button', { name: 'Open', exact: true }).click();
    await dialog.waitFor({ state: 'hidden' });
    await expect.poll(async () => (await byPath(path))?.sessionIds.length ?? 0).toBeGreaterThan(0);
    return await byPath(path);
  }
  async function addFolder(parent, name) {
    const dialog = await browseTo(parent);
    await dialog.getByRole('button', { name: 'New folder', exact: true }).click();
    await page.getByLabel('Folder name', { exact: true }).fill(name);
    await page.getByRole('button', { name: 'Create', exact: true }).click();
    await dialog.getByRole('button', { name: 'Open', exact: true }).click();
    await dialog.waitFor({ state: 'hidden' });
    await expect.poll(async () => (await byPath(join(parent, name)))?.sessionIds.length ?? 0).toBeGreaterThan(0);
  }
  const workspaceRow = title => page.getByRole('treeitem').filter({ has: page.locator('button[aria-label=' + JSON.stringify('Workspace actions for ' + title) + ']') }).first();
  async function hoverAction(row, name) {
    const button = row.getByRole('button', { name, exact: true });
    await expect.poll(async () => { await row.hover(); return await button.isVisible(); }).toBe(true);
    await button.click();
  }
  async function deleteWorkspace(row, title) {
    await hoverAction(row, 'Workspace actions for ' + title);
    await page.getByRole('menuitem', { name: 'Delete workspace', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Delete workspace', exact: true });
    await expect(dialog).toContainText('workspace list');
    await expect(dialog).toContainText('folder and session logs will be kept');
    await expect(dialog).toContainText('sessions will appear under Ungrouped');
    await dialog.getByRole('button', { name: 'Delete workspace', exact: true }).click();
    await dialog.waitFor({ state: 'hidden' });
  }
  await addFolder(workspace, 'alpha-ws');
  await expect(workspaceRow('alpha-ws')).toBeVisible();
  await addFolder(workspace, 'beta-ws');
  await expect(workspaceRow('beta-ws')).toBeVisible();
  assert.deepEqual((await list()).slice(0, 2).map(row => row.title), ['beta-ws', 'alpha-ws']);
  await annotated('created-workspaces');
  await checkpoint('create folders and adopt workspaces');

  await hoverAction(workspaceRow('alpha-ws'), 'Workspace actions for alpha-ws');
  await page.getByRole('menuitem', { name: 'Rename', exact: true }).click();
  const renameDialog = page.getByRole('dialog', { name: 'Rename workspace', exact: true });
  const beforeRename = requests.filter(row => row.path === '/api/workspace.rename').length;
  await renameDialog.getByLabel('Workspace name', { exact: true }).fill('beta-ws');
  await expect(renameDialog.getByRole('alert')).toHaveCount(1);
  await expect(renameDialog.getByRole('button', { name: 'Rename', exact: true })).toBeDisabled();
  assert.equal(requests.filter(row => row.path === '/api/workspace.rename').length, beforeRename);
  await renameDialog.getByLabel('Workspace name', { exact: true }).fill('gamma-ws');
  await expect(renameDialog.getByRole('alert')).toHaveCount(0);
  await expect(renameDialog.getByRole('button', { name: 'Rename', exact: true })).toBeEnabled();
  await renameDialog.getByRole('button', { name: 'Rename', exact: true }).click();
  await renameDialog.waitFor({ state: 'hidden' });
  await expect.poll(async () => (await byPath(join(workspace, 'alpha-ws')))?.title).toBe('gamma-ws');
  await reload();
  await expect(workspaceRow('gamma-ws')).toBeVisible();
  await checkpoint('rename duplicate preflight and reload');

  const transient = [];
  await page.exposeFunction('recordWorkspaceError', message => transient.push(message));
  const observeErrors = () => {
    const collect = () => { for (const node of document.querySelectorAll('[data-slot-error], [role="alert"]')) { const message = node.dataset.slotError ?? node.textContent?.trim(); if (message) window.recordWorkspaceError(message); } };
    new MutationObserver(collect).observe(document, { childList: true, subtree: true });
    collect();
  };
  await page.addInitScript(observeErrors);
  await page.evaluate(observeErrors);
  const registered = await adopt(workspace);
  const attached = await fetch(origin + '/fixture/attach-seed', { method: 'POST' });
  assert.equal(attached.ok, true, await attached.text());
  await expect.poll(async () => (await byPath(workspace))?.sessionIds.includes(sessionId)).toBe(true);
  let group = workspaceRow(registered.title);
  if (await group.getAttribute('aria-expanded') !== 'true') await group.click();
  let section = group.locator('xpath=ancestor::*[contains(@class, "groupSection")][1]');
  let seedRow = section.getByRole('treeitem').filter({ has: page.locator('button[aria-label^="Session actions for "]') }).first();
  await seedRow.click();
  await expect(seedRow).toHaveAttribute('aria-selected', 'true');
  const selectedId = () => page.evaluate(() => JSON.parse(localStorage.getItem('seekdeep.sessions.current') ?? '{}').sessionId);
  await expect.poll(selectedId).toBe(sessionId);
  await deleteWorkspace(group, registered.title);
  await expect.poll(async () => await byPath(workspace)).toBeUndefined();
  await expect(page.getByText('Ungrouped', { exact: true })).toBeVisible();
  await expect(page.locator('[role="treeitem"][aria-selected="true"]')).toHaveCount(1);
  await expect.poll(selectedId).toBe(sessionId);
  assert.equal(await readFile(join(workspace, 'workspace/a.txt'), 'utf8'), 'alpha\n');
  await stat(artifact);
  const reregistered = await adopt(workspace);
  assert.notEqual(reregistered.workspaceId, registered.workspaceId);
  assert(!reregistered.sessionIds.includes(sessionId));
  await invoke('workspace.delete', { workspaceId: reregistered.workspaceId });
  await reload();
  await expect(page.getByText('Ungrouped', { exact: true })).toBeVisible();
  await expect(page.locator('[role="treeitem"][aria-selected="true"]')).toHaveCount(1);
  assert.equal(await readFile(join(workspace, 'workspace/a.txt'), 'utf8'), 'alpha\n');
  await stat(artifact);
  assert.deepEqual(transient, []);
  await checkpoint('delete registration, retain current session and files, re-register');

  const oldPath = join(workspace, 'adopted/same-name');
  await mkdir(oldPath, { recursive: true });
  const old = await adopt(oldPath);
  await deleteWorkspace(workspaceRow('same-name'), 'same-name');
  await expect.poll(async () => await byPath(oldPath)).toBeUndefined();
  await addFolder(workspace, 'same-name');
  assert.notEqual((await byPath(join(workspace, 'same-name'))).workspaceId, old.workspaceId);
  assert.deepEqual(transient, []);
  await checkpoint('reuse deleted title at a new path');

  await page.getByRole('button', { name: 'View options', exact: true }).click();
  await page.getByRole('menuitem', { name: 'In one list', exact: true }).click();
  await expect(page.getByText('Ungrouped', { exact: true })).toHaveCount(0);
  assert((await page.evaluate(() => localStorage.getItem('dsh.workspace.view.v5'))).includes('flat'));
  await reload();
  await expect(page.getByText('Ungrouped', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'View options', exact: true }).click();
  await page.getByRole('menuitem', { name: 'WorkSpace', exact: true }).click();
  await expect(page.getByText('Ungrouped', { exact: true })).toBeVisible();
  await checkpoint('grouping preference and reload');

  const staged = join(workspace, 'browse-golden');
  await mkdir(join(staged, 'alpha'), { recursive: true });
  await mkdir(join(staged, 'beta'), { recursive: true });
  let dialog = await browseTo(staged);
  await expect(dialog.getByText('alpha', { exact: true })).toBeVisible();
  const aria = (await dialog.ariaSnapshot()).trim() + '\n';
  await writeFile(join(output, 'directory-browser.actual.md'), aria);
  assert.equal(aria, await readFile(join(source, 'apps/web/tests/snapshots/workspace-management/directory-browser.expected.md'), 'utf8'));
  await annotated('directory-browser');
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  await checkpoint('directory browser source aria golden');
  await mkdir(join(staged, 'alpha/only-under-alpha'), { recursive: true });
  dialog = await browseTo(staged);
  await dialog.getByRole('button', { name: 'Edit path', exact: true }).click();
  const pathInput = dialog.getByLabel('Edit path', { exact: true });
  await pathInput.fill(join(staged, 'alpha') + sep);
  await expect(dialog.getByText('only-under-alpha', { exact: true })).toBeVisible();
  await expect(dialog.getByRole('list')).toHaveCount(2);
  assert.equal(await pathInput.inputValue(), join(staged, 'alpha') + sep);
  await pathInput.fill(staged + sep + 'al');
  await expect(dialog.getByText('only-under-alpha', { exact: true })).toHaveCount(0);
  await expect(dialog.getByText('alpha', { exact: true })).toHaveCount(1);
  await expect(dialog.getByText('beta', { exact: true })).toHaveCount(0);
  await expect(dialog.getByRole('list')).toHaveCount(2);
  await pathInput.fill(staged + sep + 'zzz');
  await expect(dialog.getByText('alpha', { exact: true })).toHaveCount(1);
  await expect(dialog.getByText('beta', { exact: true })).toHaveCount(1);
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  await checkpoint('typed path pane navigation and miss fallback');

  async function seededRow() {
    const header = page.getByRole('treeitem').filter({ hasText: 'Ungrouped' }).first();
    if (await header.getAttribute('aria-expanded') !== 'true') await header.click();
    const region = header.locator('xpath=ancestor::*[contains(@class, "groupSection")][1]');
    const rows = region.getByRole('treeitem').filter({ has: page.locator('button[aria-label^="Session actions for "]') });
    await expect(rows).toHaveCount(1);
    return rows.first();
  }
  seedRow = await seededRow();
  const rowTitle = await seedRow.locator('[class*="title"]').innerText();
  await seedRow.hover();
  const card = page.getByRole('button', { name: 'Copy: ' + rowTitle, exact: true });
  await card.waitFor();
  await card.hover();
  await page.waitForTimeout(600);
  await expect(page.getByText('Idle', { exact: true })).toBeVisible();
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  const height = (await card.boundingBox()).height;
  await card.click();
  const copied = page.getByRole('status').getByText('Copied', { exact: true });
  await expect(copied).toBeVisible();
  await page.waitForTimeout(600);
  await expect(copied).toBeVisible();
  assert.equal((await card.boundingBox()).height, height);
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), rowTitle);
  await page.getByRole('button', { name: 'Settings', exact: true }).hover();
  await expect(card).toHaveCount(0);
  await checkpoint('hover card reachability, clipboard, stable height, dismissal');
  seedRow = await seededRow();
  const trigger = seedRow.locator('button[aria-label^="Session actions for "]');
  await hoverAction(seedRow, await trigger.getAttribute('aria-label'));
  const item = page.getByRole('menuitem', { name: 'Rename', exact: true });
  await item.hover(); await page.waitForTimeout(300);
  await trigger.hover(); await page.waitForTimeout(600);
  await expect(item).toHaveCount(1);
  await item.hover(); await page.waitForTimeout(600);
  await expect(item).toHaveCount(1);
  await page.getByRole('button', { name: 'Settings', exact: true }).hover();
  await expect(item).toHaveCount(0);
  await checkpoint('row menu pointer transit and dismissal');
  await hoverAction(await seededRow(), 'Session actions for ' + rowTitle);
  const beforeArchive = await readFile(artifact);
  await page.getByRole('menuitem', { name: 'Archive session', exact: true }).click();
  await expect(page.getByText(rowTitle, { exact: true })).toHaveCount(0);
  assert.deepEqual((await invoke('workspace.list')).archivedSessionIds, [sessionId]);
  assert.deepEqual(await readFile(artifact), beforeArchive);
  await reload();
  await expect(page.getByText(rowTitle, { exact: true })).toHaveCount(0);
  await checkpoint('archive echo, unchanged log, and reload');
  const twins = [join(workspace, 'same-basename-a/xx'), join(workspace, 'same-basename-b/xx')];
  for (const path of twins) { await mkdir(path, { recursive: true }); await adopt(path); }
  assert.deepEqual((await list()).filter(row => row.title === 'xx').map(row => row.path).sort(), twins.sort());
  await expect(page.locator('button[aria-label="Workspace actions for xx"]')).toHaveCount(2);
  await annotated('workspace-management');
  await checkpoint('same basename, distinct workspace identities');
  assert(!requests.some(row => row.path === '/api/session.prompt'));
  assert.deepEqual(warnings, []);
  assert.deepEqual((await readdir(join(source, 'apps/web/tests/snapshots/workspace-management'))).sort(), ['.gitkeep', 'directory-browser.expected.md']);
  const durable = await invoke('workspace.list');
  await writeFile(join(output, 'workspace-state.json'), JSON.stringify(durable, null, 2));
  await exec('agent-browser', ['--session', 'seekdeep-workspace-parity', 'close']);
  cdpPort = undefined;
  await context.close().catch(() => {});
  context = undefined;
  await stopHost();
  assert.equal(JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8')).calls, 0);
  origin = await startHost();
  assert.deepEqual(await invoke('workspace.list'), durable);
  await checkpoint('zero prompt requests and clean browser');
  console.log(JSON.stringify({ browser: browserVersion, scenarios: checkpoints.length, realHost: true, coldHostRegistry: true }));
} catch (error) {
  if (page) {
    await page.screenshot({ path: join(output, 'failure.png'), fullPage: true }).catch(() => {});
    await writeFile(join(output, 'failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
  }
  await writeFile(join(output, 'host-stderr.txt'), stderr);
  await writeFile(join(output, 'failures.json'), JSON.stringify({ failures, requests, checkpoints }, null, 2));
  throw error;
} finally {
  if (cdpPort) await exec('agent-browser', ['--session', 'seekdeep-workspace-parity', 'close']).catch(() => {});
  await context?.close().catch(() => {});
  await stopHost();
}
"#;
