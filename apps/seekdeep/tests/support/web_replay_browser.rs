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
page.on('websocket', socket => socket.on('framereceived', event => {
  if (typeof event.payload === 'string') {
    try { frames.push(JSON.parse(event.payload)); } catch { /* Non-JSON frames are not protocol evidence. */ }
  }
}));
try {
  await Promise.race([(async () => {
    await page.goto(origin);
    await page.getByRole('button', { name: 'Continue', exact: true }).click();
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
    if (failures.length) throw new Error(failures.join('\n'));
    const result = { browser: browser.version(), sourceReplay: true, observedText, settled: 'LIGHTHOUSE', reload: true };
    await writeFile(join(output, 'result.json'), JSON.stringify(result, null, 2) + '\n');
    console.log(JSON.stringify(result));
  })(), pageFailure]);
} catch (error) {
  await Promise.all(responseReads);
  await writeFile(join(output, 'exchanges.json'), JSON.stringify(exchanges, null, 2) + '\n');
  await writeFile(join(output, 'frames.json'), JSON.stringify(frames, null, 2) + '\n');
  await writeFile(join(output, 'observed-text.json'), JSON.stringify(await page.evaluate(() => globalThis.__seekdeepReplayText ?? [])) + '\n');
  await page.screenshot({ path: join(output, 'failure.png'), fullPage: true });
  await writeFile(join(output, 'failure.txt'), await page.locator('body').innerText());
  console.error(await page.locator('body').innerText());
  throw error;
} finally {
  await browser.close();
}
";
