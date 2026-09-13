//! Browser-only observations over the normal Rust Web profile.

pub(super) const DRIVER: &str = r"import { createRequire } from 'node:module';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [origin, source, output, workspace, prompt] = process.argv.slice(2);
const { chromium } = createRequire(join(source, 'apps/web/package.json'))('playwright');
const { connectFreshWorkspace, newEnglishPage } = await import(pathToFileURL(join(source, 'apps/web/tests/support.ts')));
const browser = await chromium.launch({ headless: true });
const page = await newEnglishPage(browser);
page.setDefaultTimeout(30000);
const failures = [];
const exchanges = [], responseReads = [];
const frames = [];
const peerFrames = [], frameWaiters = [];
function observeFrames(client, collection) {
  client.on('websocket', socket => socket.on('framereceived', event => {
    if (typeof event.payload !== 'string') return;
    let frame;
    try { frame = JSON.parse(event.payload); } catch { return; }
    collection.push(frame);
    for (const waiter of [...frameWaiters]) if (waiter.collection === collection && waiter.matches(frame)) waiter.resolve(frame);
  }));
}
function waitForFrame(collection, matches) {
  const existing = collection.find(matches);
  if (existing) return Promise.resolve(existing);
  return new Promise((resolve, reject) => {
    const waiter = { collection, matches, resolve: frame => { clearTimeout(timer); frameWaiters.splice(frameWaiters.indexOf(waiter), 1); resolve(frame); } };
    const timer = setTimeout(() => { frameWaiters.splice(frameWaiters.indexOf(waiter), 1); reject(new Error('Host lifecycle frame was not delivered')); }, 30000);
    frameWaiters.push(waiter);
  });
}
let rejectPage;
const pageFailure = new Promise((_, reject) => { rejectPage = reject; });
pageFailure.catch(() => {});
const fail = message => { failures.push(message); rejectPage(new Error(message)); };
page.on('pageerror', error => fail(error.stack ?? error.message));
page.on('console', message => { if (message.type() === 'error') fail(message.text()); });
page.on('response', response => { if (response.status() >= 400) fail(`${response.status()} ${response.url()}`); });
page.on('response', response => {
  const path = new URL(response.url()).pathname;
  if (path.startsWith('/api/session.') || path.startsWith('/api/workspace.')) {
    responseReads.push(response.json().then(body => exchanges.push({ path, body })).catch(error => exchanges.push({ path, error: String(error) })));
  }
});
observeFrames(page, frames);
try {
  await Promise.race([(async () => {
    await page.goto(origin);
    await page.getByRole('button', { name: 'Continue', exact: true }).click();
    await page.getByRole('textbox', { name: 'Choose workspace' }).waitFor();
    const peer = await newEnglishPage(browser);
    peer.setDefaultTimeout(30000);
    peer.on('pageerror', error => fail(error.stack ?? error.message));
    peer.on('console', message => { if (message.type() === 'error') fail(message.text()); });
    observeFrames(peer, peerFrames);
    await peer.goto(origin);
    await peer.getByRole('textbox', { name: 'Choose workspace' }).waitFor();
    await connectFreshWorkspace(page, workspace);
    const input = page.locator('textarea').first();
    await input.fill(prompt);
    await page.evaluate(() => {
      globalThis.__seekdeepReplayText = [];
      new MutationObserver(() => {
        for (const line of document.body.innerText.split('\n').map(value => value.trim())) {
          if (line.length >= 4 && 'LIGHTHOUSE'.startsWith(line) && !globalThis.__seekdeepReplayText.includes(line)) globalThis.__seekdeepReplayText.push(line);
        }
      }).observe(document.body, { subtree: true, childList: true, characterData: true });
    });
    await input.press('Enter');
    await page.waitForFunction(() => globalThis.__seekdeepReplayText.includes('LIGHTH'));
    await page.screenshot({ path: join(output, 'streaming.png'), fullPage: true });
    await writeFile(join(output, 'streaming-aria.txt'), await page.locator('body').ariaSnapshot());
    await page.getByRole('button', { name: 'Stop generating', exact: true }).waitFor();
    await page.getByText('LIGHTHOUSE', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Stop generating', exact: true }).waitFor({ state: 'hidden' });
    const observedText = await page.evaluate(() => globalThis.__seekdeepReplayText);
    await page.screenshot({ path: join(output, 'settled.png'), fullPage: true });
    await page.reload();
    await page.getByText('LIGHTHOUSE', { exact: true }).waitFor();
    const driven = frames.find(frame => frame.payload?.type === 'session/event' && frame.payload.event?.type === 'user/message')?.payload.sessionId;
    if (!driven) throw new Error('prompt Session identity was not observed on the wire');
    await Promise.all([frames, peerFrames].map(collection => waitForFrame(collection, frame => frame.payload?.type === 'host/session-added' && frame.payload.sessionId === driven)));
    await page.locator('textarea').first().fill('/lifecycle-probe');
    await page.locator('textarea').first().press('Enter');
    await page.getByText('Host lifecycle probe complete.', { exact: true }).waitFor();
    for (const collection of [frames, peerFrames]) {
      const added = await waitForFrame(collection, frame => frame.payload?.type === 'host/session-added' && frame.payload.sessionId === 'browser-lifecycle-probe');
      const removed = await waitForFrame(collection, frame => frame.payload?.type === 'host/session-removed' && frame.payload.sessionId === 'browser-lifecycle-probe');
      const failed = await waitForFrame(collection, frame => frame.payload?.type === 'host/agent-error' && frame.payload.sessionId === driven);
      if (added.payload.blank !== true || added.payload.cwd !== join(workspace, 'workspace') || collection.indexOf(added) >= collection.indexOf(removed)) throw new Error('Host lifecycle metadata or ordering changed');
      if (failed.payload.message !== 'Host lifecycle probe failed.') throw new Error('Host changed the Agent error text');
      if (collection.filter(frame => frame.payload?.type === 'host/session-added' && frame.payload.sessionId === 'browser-lifecycle-probe').length !== 1
        || collection.filter(frame => frame.payload?.type === 'host/session-removed' && frame.payload.sessionId === 'browser-lifecycle-probe').length !== 1
        || collection.filter(frame => frame.payload?.type === 'host/agent-error' && frame.payload.sessionId === driven).length !== 1) throw new Error('Host duplicated a lifecycle event');
    }
    if (failures.length) throw new Error(failures.join('\n'));
    const result = { browser: browser.version(), sourceReplay: true, observedText, settled: 'LIGHTHOUSE', reload: true, hostLifecycle: { clients: 2, added: true, removed: true, agentError: true } };
    await writeFile(join(output, 'result.json'), JSON.stringify(result, null, 2) + '\n');
    console.log(JSON.stringify(result));
  })(), pageFailure]);
} catch (error) {
  await Promise.all(responseReads);
  await writeFile(join(output, 'exchanges.json'), JSON.stringify(exchanges, null, 2) + '\n');
  await writeFile(join(output, 'frames.json'), JSON.stringify(frames, null, 2) + '\n');
  await writeFile(join(output, 'peer-frames.json'), JSON.stringify(peerFrames, null, 2) + '\n');
  await writeFile(join(output, 'observed-text.json'), JSON.stringify(await page.evaluate(() => globalThis.__seekdeepReplayText ?? [])) + '\n');
  await page.screenshot({ path: join(output, 'failure.png'), fullPage: true });
  await writeFile(join(output, 'failure.txt'), await page.locator('body').innerText());
  console.error(await page.locator('body').innerText());
  throw error;
} finally {
  await browser.close();
}
";
