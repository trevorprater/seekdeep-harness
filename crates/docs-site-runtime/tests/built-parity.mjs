import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, basename } from 'node:path';
import { pathToFileURL } from 'node:url';

const [root, sourceRoot] = process.argv.slice(2);
const require = createRequire(join(sourceRoot, 'apps/web/package.json'));
const { chromium } = require('playwright');
  const runtime = require(join(root, 'website/.cache/native/docs_site_runtime.js'));
const directory = await mkdtemp(join(tmpdir(), 'seekdeep-doc-site-oracle-'));
let browser, server, dev;
try {
  const original = await readFile(join(sourceRoot, 'website/.vitepress/config.ts'), 'utf8');
  const oracleText = original.split('\n').filter(line => !line.startsWith('import { withMermaid }') && !line.startsWith('import { docsSourceFiles,'))
    .join('\n').replace("from '../docs.ts'", `from ${JSON.stringify(join(sourceRoot, 'website/docs.ts'))}`)
    .replaceAll('projectDocs()', 'void 0')
    .replace("resolve(import.meta.dirname, '../public/wordmark.svg')", JSON.stringify(join(root, 'website/public/wordmark.svg')));
  const modulePath = join(directory, 'oracle.mts');
  await writeFile(modulePath, `const withMermaid = value => value;\nconst docsSourceFiles = () => [];\n${oracleText}\nexport { escapeVueInterpolation, scrollbarScript };\n`);
  const source = await import(pathToFileURL(modulePath));
  let comparisons = 0;
  const outcome = operation => {
    try { return { value: operation() }; }
    catch (error) { return { name: error.name, message: error.message }; }
  };
  const normalize = value => JSON.parse(JSON.stringify(value).replaceAll('deepseek-ai/deepseek-harness', 'trevorprater/seekdeep-harness'));
  globalThis.__seekdeepDocsRuntime = runtime;
  const serialized = runtime.editLinkPattern('branch/"quoted"').toString();
  const restored = new Function(`return (${serialized})`)();
  assert.equal(restored({ frontmatter: { editSource: 'docs/guide.md' } }), 'https://github.com/trevorprater/seekdeep-harness/edit/branch/"quoted"/docs/guide.md');
  comparisons++;
  for (const frontmatter of [{ editSource: 'docs/user/guide/index.md' }, { editSource: '' }, {}, null, undefined, 1, 'text', () => {}, { editSource: 7 }]) {
    const page = { frontmatter };
    assert.deepEqual(outcome(() => runtime.editLink(page, 'master')), normalize(outcome(() => source.default.themeConfig.editLink.pattern(page))));
    comparisons++;
  }
  const marker = new Error('edit source getter failed');
  const page = { frontmatter: { get editSource() { throw marker; } } };
  assert.throws(() => runtime.editLink(page, 'master'), error => error === marker);
  comparisons++;
  for (const text of ['', 'plain {text}', '{{ model }}', '<code>{{name}}</code>', '中文 {{{{x}}}} 😀', '&amp; {{}}']) {
    assert.equal(runtime.escapeVueInterpolation(text), source.escapeVueInterpolation(text));
    comparisons++;
  }
  for (const [text, code] of [[undefined, () => ''], [() => '', undefined], [undefined, undefined], [null, null]]) {
    assert.deepEqual(outcome(() => runtime.validateMarkdownRules(text, code)), outcome(() => source.default.markdown.config({ renderer: { rules: { text, code_inline: code } } })));
    comparisons++;
  }
  const paths = ['/a.md', '/image.svg'];
  for (const path of ['/a.md', '/image.svg', '/a.md.tmp', '/generated/a.md', undefined]) {
    assert.equal(runtime.shouldProject(paths, path), paths.includes(path));
    comparisons++;
  }
  const assets = join(root, 'website/.cache/public/_seekdeep');
  server = createServer(async (request, response) => {
    try {
      if (request.url === '/') {
        response.setHeader('Content-Type', 'text/html');
        response.end('<!doctype html><main></main><aside id="first" class="VPSidebar"></aside><aside id="second" class="VPSidebar"></aside>');
      } else if (['/docs_site_runtime.js', '/docs_site_runtime_bg.wasm'].includes(request.url)) {
        response.setHeader('Content-Type', request.url.endsWith('.wasm') ? 'application/wasm' : 'text/javascript');
        response.end(await readFile(join(assets, basename(request.url))));
      } else { response.writeHead(404); response.end(); }
    } catch (error) { response.writeHead(500); response.end(error.message); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  browser = await chromium.launch({ headless: true });
  const tab = await browser.newPage();
  const origin = `http://127.0.0.1:${server.address().port}`;
  const results = [];
  for (const mode of ['source', 'rust']) {
    await tab.goto(origin);
    results.push(await tab.evaluate(async ({ mode, script }) => {
      const wasm = await import('/docs_site_runtime.js');
      await wasm.default();
      let next = 0, now = 0;
      const timers = new Map();
      window.setTimeout = (callback, delay) => { const id = ++next; timers.set(id, { callback, at: now + delay }); return id; };
      window.clearTimeout = id => timers.delete(id);
      const advance = amount => {
        now += amount;
        for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.callback(); }
      };
      const first = document.getElementById('first'), second = document.getElementById('second');
      const scroll = element => element.dispatchEvent(new Event('scroll'));
      const trace = [];
      const sample = () => trace.push({ first: first.hasAttribute('data-scrolling'), second: second.hasAttribute('data-scrolling'), deadlines: [...timers.values()].map(timer => timer.at) });
      let owner;
      if (mode === 'source') (0, eval)(script); else owner = new wasm.SidebarScrollbar();
      scroll(document.querySelector('main')); sample();
      scroll(first); sample();
      advance(500); scroll(first); sample();
      advance(300); sample();
      advance(500); sample();
      scroll(first); advance(100); scroll(second); sample();
      advance(800); sample();
      if (owner) {
        scroll(first);
        owner.free();
        if (timers.size !== 0) throw new Error('Sidebar disposal retained a timeout.');
        first.removeAttribute('data-scrolling');
        scroll(first);
        if (first.hasAttribute('data-scrolling')) throw new Error('Sidebar disposal retained its listener.');
      }
      return trace;
    }, { mode, script: source.scrollbarScript }));
  }
  assert.deepEqual(results[1], results[0]);
  assert.deepEqual(results[1].at(-1), { first: true, second: false, deadlines: [] });
  dev = spawn(process.execPath, [join(root, 'website/node_modules/vitepress/bin/vitepress.js'), 'dev', join(root, 'website'), '--host', '127.0.0.1', '--port', '0'], {
    cwd: root, stdio: ['ignore', 'pipe', 'pipe'],
  });
  const devExit = new Promise(resolve => dev.once('exit', (code, signal) => resolve({ code, signal })));
  dev.completed = devExit;
  let transcript = '';
  const devOrigin = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('VitePress readiness timed out: ' + transcript)), 60000);
    const read = chunk => {
      transcript += chunk.toString().replace(/\x1b\[[0-9;]*[A-Za-z]/g, '');
      const match = /Local:\s+(http:\/\/127\.0\.0\.1:\d+\/)/.exec(transcript);
      if (match) { clearTimeout(timeout); resolve(match[1]); }
    };
    dev.stdout.on('data', read);
    dev.stderr.on('data', read);
    devExit.then(status => { clearTimeout(timeout); reject(new Error('VitePress exited before readiness: ' + JSON.stringify(status) + '\n' + transcript)); });
    dev.once('error', error => { clearTimeout(timeout); reject(error); });
  });
  const pageErrors = [];
  tab.on('pageerror', error => pageErrors.push(error.message));
  await tab.goto(devOrigin + 'en/guide/providers');
  await tab.getByRole('heading', { name: /^Configure models/ }).waitFor();
  await tab.waitForFunction(() => [...document.querySelectorAll('.vp-doc img')].length === 2 && [...document.querySelectorAll('.vp-doc img')].every(image => image.complete && image.naturalWidth > 0));
  assert.equal(await tab.evaluate(() => typeof globalThis.__seekdeepDocsRuntime?.editLink), 'function');
  assert.deepEqual(pageErrors, []);
  console.log(JSON.stringify({ nodeComparisons: comparisons, browserScenarios: results[1].length, browserVersion: browser.version(), disposedListenerAndTimer: true, viteDevHydrated: true, localImagesLoaded: 2 }));
} finally {
  if (dev && dev.exitCode === null && dev.signalCode === null) {
    dev.kill('SIGINT');
    let timeout;
    const exited = await Promise.race([dev.completed.then(() => true), new Promise(resolve => { timeout = setTimeout(() => resolve(false), 5000); })]);
    clearTimeout(timeout);
    if (!exited) { dev.kill('SIGKILL'); await dev.completed; }
  }
  if (browser) await browser.close();
  if (server) await new Promise(resolve => server.close(resolve));
  await rm(directory, { recursive: true, force: true });
}
