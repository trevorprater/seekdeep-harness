//! Unchanged keyless source browser suites over the Rust/WASM client and a fresh Rust Host each.
//!
//! Every scenario below boots its own isolated Host and Chromium profile exactly as the source
//! `beforeAll` hooks do, then compiles the pinned `it` callbacks with the source scaffold helpers
//! bound to Rust-Host equivalents: session events and listings come from the `/fixture/sessions`
//! route instead of in-process Cordis taps, and goldens compare byte-for-byte in replay mode.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdirSync, existsSync } from 'node:fs';
import { mkdir, mkdtemp, rm, readFile, writeFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, basename, dirname } from 'node:path';
const [source, host, world, output] = process.argv.slice(2), require = createRequire(join(source, 'apps/web/package.json'));
const { chromium } = require('playwright'), { expect: playwrightExpect } = require('playwright/test'), ts = require('typescript');
// The source suites run under vitest, whose `expect.poll` honours `interval`; Playwright's poll
// option is `intervals`, and its default 100/250/500/1000ms schedule misses sub-second windows.
const expect = new Proxy(playwrightExpect, {
  apply(target, thisArg, args) { return Reflect.apply(target, thisArg, args); },
  get(target, key) {
    if (key !== 'poll') return Reflect.get(target, key);
    return (probe, options) => target.poll(probe, options && options.interval !== undefined ? { ...options, intervals: [options.interval] } : options);
  },
});
const exec = promisify(execFile), MODE = 'replay', TESTS = join(source, 'apps/web/tests'), checks = [], filter = process.env.SEEKDEEP_KEYLESS_SCENARIO;
const selected = name => !filter || filter.split(',').includes(name);
// Source assertions pinned to product identity: the Rust product renames the DeepSeek Harness
// prose, so the same case reads the renamed policy context, exactly like the golden rewrite.
function adaptSelectors(code) {
  return code
    .replaceAll('[class*="centerCol"]', '[class*="seekdeep-layout-center-col"]')
    .replaceAll('Current DSH file policy', 'Current SeekDeep file policy')
    .replaceAll('DSH file sandbox', 'SeekDeep file sandbox')
    .replaceAll("'@deepseek-ai/dsh-system-prompt'", "'@seekdeep-ai/seekdeep-system-prompt'")
    .replaceAll('"$DSH_WEB_URL"', '"$SEEKDEEP_WEB_URL"')
    .replaceAll('--dsh-composer-dock-inset', '--seekdeep-composer-dock-inset');
}
async function sourceFile(file) {
  const body = await readFile(join(TESTS, file), 'utf8');
  return ts.createSourceFile(file, body, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}
function declarations(ast, names) {
  const selected = ast.statements.filter(node => {
    const declared = ts.isVariableStatement(node) ? node.declarationList.declarations.map(value => value.name.getText(ast)) : node.name ? [node.name.getText(ast)] : [];
    return declared.some(name => names.includes(name));
  });
  assert.equal(selected.length, names.length, 'source helper inventory for ' + names.join(','));
  return selected.map(node => node.getText(ast).replace(/^export\s+/, '')).join('\n');
}
// Helpers a suite declares inside its describe callback (they close over the shared page).
function nestedDeclarations(ast, names) {
  const found = [];
  function visit(node) {
    if ((ts.isFunctionDeclaration(node) || ts.isVariableStatement(node)) && !ast.statements.includes(node)) {
      const declared = ts.isVariableStatement(node) ? node.declarationList.declarations.map(value => value.name.getText(ast)) : node.name ? [node.name.getText(ast)] : [];
      if (declared.some(name => names.includes(name))) found.push(node.getText(ast));
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  assert.equal(found.length, names.length, 'nested source helper inventory for ' + names.join(','));
  return found.join('\n');
}
function sourceCases(ast) {
  const cases = [];
  function visit(node) {
    if (ts.isCallExpression(node) && ts.isStringLiteral(node.arguments[0] ?? ts.factory.createNull()) && node.arguments[1] && ts.isArrowFunction(node.arguments[1])) {
      const callee = node.expression.getText(ast);
      if (callee === 'it' || callee.startsWith('it.')) cases.push({ name: node.arguments[0].text, callback: node.arguments[1].getText(ast), skipped: callee.includes("skipIf(MODE !== 'record')") });
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  return cases;
}
// `shared` carries a suite's describe-scope `let` state between cases: the emitted case body runs
// inside `with (shared)`, so a bare assignment in one case is the next case's read.
function compile(code, bindings, { adapt = true, shared } = {}) {
  const emitted = ts.transpileModule(adapt ? adaptSelectors(code) : code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } }).outputText;
  const body = shared === undefined ? emitted : 'with (__shared) {\n' + emitted + '\n}';
  return new Function(...Object.keys(bindings), '__shared', body)(...Object.values(bindings), shared);
}
// Recorded user text is fixture data, not product identity: it stays verbatim.
function verbatimConstant(ast, name) {
  return compile(declarations(ast, [name]) + '\nreturn ' + name + ';', {}, { adapt: false });
}
async function readiness(server, stderr) {
  return new Promise((resolve, reject) => {
    let stdout = ''; const timer = setTimeout(() => reject(new Error('Host readiness: ' + stderr())), 30000);
    server.stdout.on('data', value => { stdout += value; const match = /seekdeep web: (http:\/\/\S+)/.exec(stdout); if (match) { clearTimeout(timer); resolve(match[1]); } });
    server.once('exit', code => { clearTimeout(timer); reject(new Error('Host exit ' + code + ': ' + stderr())); });
  });
}
async function bootHost(name, options) {
  // The source scaffold's `workspaceCwd` is a temp root the Host runs in; scenarios connect the
  // `workspace` folder they create inside it, so the root's own basename never reaches the UI.
  const root = join(world, name), home = join(root, 'home'), workspace = join(root, 'e2e-ws'), overlay = join(root, 'overlay.patch.yml');
  await mkdir(home, { recursive: true }); await mkdir(workspace, { recursive: true });
  await writeFile(overlay, options.overlay ?? '[]\n');
  let stderr = '', sequence = 0;
  // The seed-log route mounts only with a seed path; generated fixtures arrive as request bodies.
  const seedPath = join(root, 'unused-seed.jsonl'); await writeFile(seedPath, '');
  const args = [home, workspace, name + '-seed', home, options.replay ? 'replay' : 'route-only', overlay, options.seedFile ?? seedPath, ...options.replay ? [options.replay, options.replayOverride ?? '-', ...options.replayChildFixtures ?? []] : []];
  // Skill discovery is model-visible input: the preset-mounted skill roots resolve from the
  // process environment, so pin every host-level root inside the owned world as the source
  // scaffold's `skillRootEnvironment` does.
  // An `undefined` override unsets a pinned variable (the telemetry disclosure scenario).
  const env = Object.fromEntries(Object.entries({ ...process.env, SEEKDEEP_HOME: home, SEEKDEEP_AGENTS_HOME: join(home, 'agents'), SEEKDEEP_BUNDLED_SKILL_DIR: join(home, 'bundled-skills'), SEEKDEEP_TELEMETRY_DISABLED: '1', ...options.env ?? {} }).filter(([, value]) => value !== undefined));
  const server = spawn(host, args, { cwd: process.cwd(), env, stdio: ['ignore', 'pipe', 'pipe'] });
  server.stderr.on('data', value => { stderr += value; });
  const origin = await readiness(server, () => stderr);
  const invoke = async (method, payload) => { const rpcId = name + '-' + ++sequence; const response = await fetch(origin + '/api/' + method, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method, payload }) }); assert(response.ok); const body = await response.json(); assert.equal(body.rpcId, rpcId); assert.equal(body.result.ok, true, JSON.stringify(body)); return body.result.value; };
  if (!options.welcomePending) await invoke('settings.mutate', { ns: options.welcome.namespace, ops: [{ op: 'set', path: [options.welcome.field], value: options.welcome.version }] });
  const seedSession = async (id, text) => { const seeded = await fetch(origin + '/fixture/seed-log/' + encodeURIComponent(id), { method: 'POST', ...text === undefined ? {} : { body: text } }); assert(seeded.ok, 'seed ' + id + ': ' + await seeded.text() + '\n' + stderr); };
  return { origin, home, workspace, invoke, seedSession, stderr: () => stderr, async stop() {
    if (server.exitCode === null) { const exited = once(server, 'exit'); server.kill('SIGINT'); const [code] = await exited; await writeFile(join(output, name + '-host-stderr.txt'), stderr); assert.equal(code, 0, stderr); }
    const audit = JSON.parse(await readFile(join(home, 'model-call-audit.json'), 'utf8'));
    assert.equal(audit.calls, 0, 'keyless scenario made a non-replay model call');
    if (options.replay) assert.equal(audit.replayConsumed, true, 'replay fixture was not fully consumed');
  } };
}
// Session events and listings the source reads from its in-process Context come from the Rust
// Host's fixture listing; every settled `expect.poll` refreshes them so the case's synchronous
// event assertions observe the Host state that produced the settled UI.
function liveSessions(origin) {
  const events = [], sessions = [], listeners = [], seen = new Map();
  const refresh = async () => {
    const response = await fetch(origin + '/fixture/sessions'); assert(response.ok, 'fixture session listing HTTP ' + response.status);
    const listed = await response.json();
    sessions.splice(0, sessions.length, ...listed.map(entry => ({ id: entry.header.id, header: entry.header, events: entry.events })));
    events.splice(0, events.length, ...listed.flatMap(entry => entry.events));
    // Source: `ctx.on('session/event', (session, event) => ...)` observes every appended event
    // once; the listing delivers the events appended since the previous refresh, in order.
    for (const entry of listed) {
      const delivered = seen.get(entry.header.id) ?? 0;
      for (const event of entry.events.slice(delivered)) for (const listener of listeners) listener({ id: entry.header.id, header: entry.header }, event);
      seen.set(entry.header.id, entry.events.length);
    }
  };
  const onEvent = listener => { listeners.push(listener); };
  const wrapMatchers = target => new Proxy(target, { get(inner, key) {
    const value = Reflect.get(inner, key);
    if (typeof value === 'function') return (...args) => { const result = value.apply(inner, args); return result && typeof result.then === 'function' ? result.then(async settled => { await refresh(); return settled; }) : result; };
    return value && typeof value === 'object' ? wrapMatchers(value) : value;
  } });
  const liveExpect = new Proxy(expect, {
    apply(target, thisArg, args) { return Reflect.apply(target, thisArg, args); },
    get(target, key) { return key === 'poll' ? (probe, options) => wrapMatchers(target.poll(probe, options)) : Reflect.get(target, key); },
  });
  // The source barrier is an in-process turn/end tap followed by a flush; the Rust Host equivalent
  // observes the first closed turn through the listing and settles it through the idle barrier.
  let stopped = false;
  const turnEnds = session => session.events.filter(event => event.type === 'turn/end').length;
  // Armed like the source tap: the barrier resolves on the first turn/end appended after arming.
  const whenTurnSettled = (timeoutMs = 30000) => { const baseline = new Map(sessions.map(session => [session.id, turnEnds(session)])); const pending = (async () => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (stopped) throw new Error('Host stopped before the turn settled');
      await refresh();
      const closed = sessions.find(session => turnEnds(session) > (baseline.get(session.id) ?? 0));
      if (closed) {
        const settled = await fetch(origin + '/fixture/idle/' + encodeURIComponent(closed.id), { method: 'POST', signal: AbortSignal.timeout(timeoutMs) });
        assert(settled.ok, 'idle barrier HTTP ' + settled.status + ': ' + await settled.clone().text());
        await refresh(); return closed.id;
      }
      if (Date.now() > deadline) throw new Error('no turn/end within ' + timeoutMs + 'ms');
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  })(); pending.catch(() => {}); return pending; };
  // The source's `whenTurnsSettled` counts durable turn ends through the same in-process tap.
  const whenTurnsSettled = (count, timeoutMs) => { const pending = (async () => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (stopped) throw new Error('Host stopped before the turns settled');
      await refresh();
      const closed = sessions.find(session => session.events.filter(event => event.type === 'turn/end').length >= count);
      if (closed) {
        const settled = await fetch(origin + '/fixture/idle/' + encodeURIComponent(closed.id), { method: 'POST', signal: AbortSignal.timeout(timeoutMs) });
        assert(settled.ok, 'idle barrier HTTP ' + settled.status + ': ' + await settled.clone().text());
        await refresh(); return closed.id;
      }
      if (Date.now() > deadline) throw new Error('only some of ' + count + ' Goal turns ended within ' + timeoutMs + 'ms');
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  })(); pending.catch(() => {}); return pending; };
  const stop = () => { stopped = true; };
  // `ctx.agents.list()` is synchronous in the source; here each call returns the latest listing
  // and kicks a background refresh so a polled predicate observes the Host within its window.
  const agents = { list: () => { if (!stopped) refresh().catch(() => {}); return sessions.map(session => ({ session: { header: session.header, id: session.id } })); } };
  return { events, sessions, refresh, expect: liveExpect, whenTurnSettled, whenTurnsSettled, agents, onEvent, stop };
}
// A saturated main thread: sample where the time goes, with wasm frames named by their name section.
async function startProfile(page) {
  const session = await page.context().newCDPSession(page);
  await session.send('Profiler.enable'); await session.send('Profiler.setSamplingInterval', { interval: 250 }); await session.send('Profiler.start');
  return async () => { const { profile } = await session.send('Profiler.stop'); await session.detach().catch(() => {}); return summarizeProfile(profile); };
}
async function cpuProfile(page) {
  const stop = await startProfile(page);
  await new Promise(resolve => setTimeout(resolve, 1500));
  return stop();
}
function summarizeProfile(profile) {
  const nodes = new Map(profile.nodes.map(node => [node.id, node])), parents = new Map();
  for (const node of profile.nodes) for (const child of node.children ?? []) parents.set(child, node.id);
  const self = new Map(); for (const id of profile.samples) self.set(id, (self.get(id) ?? 0) + 1);
  const chain = id => { const names = []; let current = id; while (current !== undefined && names.length < 80) { const node = nodes.get(current); names.push(node.callFrame.functionName || '(anonymous)'); current = parents.get(current); } return names; };
  return [...self.entries()].sort((a, b) => b[1] - a[1]).slice(0, 25).map(([id, count]) => ({ samples: count, url: nodes.get(id).callFrame.url.slice(-60), chain: chain(id) }));
}
async function pausedStack(page) {
  const session = await page.context().newCDPSession(page);
  const paused = new Promise(resolve => session.once('Debugger.paused', event => resolve(event.callFrames.map(frame => ({ function: frame.functionName, url: frame.url, callFrameId: frame.callFrameId, scriptId: frame.location.scriptId, line: frame.location.lineNumber, column: frame.location.columnNumber })))));
  await session.send('Debugger.enable');
  await session.send('Debugger.pause');
  const frames = await Promise.race([paused, new Promise((_, reject) => setTimeout(() => reject(new Error('no pause within 8s')), 8000))]).catch(async error => { await session.send('Debugger.disable').catch(() => {}); await session.detach().catch(() => {}); throw error; });
  const sources = new Map();
  for (const frame of frames) {
    // React's dispatch frames name the DOM event `t` and the native event `s`.
    const probe = await session.send('Debugger.evaluateOnCallFrame', { callFrameId: frame.callFrameId, returnByValue: true, expression: 'JSON.stringify({ event: typeof t === "string" ? t : undefined, native: s && s.type, animation: s && s.animationName, sameTarget: s && s.target === window.__seekdeepLastTarget, attached: s && s.target && document.contains(s.target), computedAnimation: s && s.target && getComputedStyle(s.target).animationName, target: s && s.target && s.target.outerHTML ? s.target.outerHTML.slice(0, 300) : undefined, tracked: (window.__seekdeepLastTarget = s && s.target, true) })' }).catch(error => ({ result: { value: 'probe failed: ' + String(error) } }));
    frame.probe = probe.result?.value; delete frame.callFrameId;
    if (!sources.has(frame.scriptId)) {
      const { scriptSource } = await session.send('Debugger.getScriptSource', { scriptId: frame.scriptId }).catch(() => ({ scriptSource: '' }));
      const lines = scriptSource.split('\n');
      sources.set(frame.scriptId, { length: scriptSource.length, identity: (scriptSource.match(/@seekdeep-ai\/[a-z-]+/g) ?? []).slice(0, 3), lines });
    }
    const source = sources.get(frame.scriptId);
    frame.identity = source.identity; frame.scriptLength = source.length;
    frame.snippet = source.lines.slice(Math.max(0, frame.line - 2), frame.line + 3).map(line => line.length > 400 ? line.slice(Math.max(0, frame.column - 200), frame.column + 200) : line);
  }
  await session.send('Debugger.resume').catch(() => {});
  await session.send('Debugger.disable').catch(() => {});
  await session.detach().catch(() => {});
  return frames;
}
let planned = 0;
async function runCases(scenario, cases, page, bindings, tripwire) {
  for (const entry of cases) {
    if (entry.skipped) { console.log('keyless: ' + scenario + ' skips record-only case ' + entry.name); continue; }
    planned += 1;
    const hooks = [];
    const callback = compile(bindings.prelude + '\nreturn (' + entry.callback + ');', { ...bindings.values, onTestFailed: hook => hooks.push(hook), saveFailureShot: (target, name) => target.screenshot({ path: join(output, name + '.png'), fullPage: true }).catch(() => {}) }, { shared: bindings.shared });
    const stopProfile = process.env.SEEKDEEP_KEYLESS_PROFILE ? await startProfile(page) : undefined;
    const startedAt = Date.now();
    try { await callback(); if (stopProfile) await stopProfile(); } catch (error) {
      if (stopProfile) await writeFile(join(output, scenario + '-case-profile.json'), JSON.stringify({ elapsedMs: Date.now() - startedAt, animationStarts: await page.evaluate(() => window.__seekdeepAnimationStarts).catch(() => 'unavailable'), hot: await stopProfile().catch(failure => String(failure)) }, null, 2));
      const probes = {};
      probes.evaluate = await Promise.race([page.evaluate('1 + 1'), new Promise(resolve => setTimeout(() => resolve('timed out'), 5000))]).then(String, failure => 'failed: ' + String(failure));
      if (probes.evaluate !== '2') {
        // A hung main thread answers no locator: interrupt it through CDP and record where it
        // spins. A responsive page must not be paused — a pending pause blocks every later probe.
        const stack = await pausedStack(page).catch(failure => 'unavailable: ' + String(failure));
        await writeFile(join(output, scenario + '-hang-stack.json'), JSON.stringify(stack, null, 2));
        const profile = await cpuProfile(page).catch(failure => 'unavailable: ' + String(failure));
        await writeFile(join(output, scenario + '-hang-profile.json'), JSON.stringify(profile, null, 2));
      }
      probes.dom = await page.evaluate(() => ({ done: document.body.innerText.includes('DONE'), planCard: document.querySelectorAll('[data-plan-review-key]').length, reasoningStates: [...document.querySelectorAll('[data-variant="think"]')].map(node => node.getAttribute('data-state')), textareaEnabled: !document.querySelector('textarea')?.disabled, phase: document.querySelector('div[data-phase]')?.getAttribute('data-phase') })).catch(failure => 'failed: ' + String(failure));
      probes.animationStartsPerSecond = await page.evaluate(() => new Promise(resolve => { let count = 0; const handler = () => { count += 1; }; document.addEventListener('animationstart', handler, true); setTimeout(() => { document.removeEventListener('animationstart', handler, true); resolve(count); }, 1000); })).catch(failure => 'failed: ' + String(failure));
      probes.aria = await page.locator('body').ariaSnapshot({ timeout: 5000 }).then(() => 'ok', failure => 'failed: ' + String(failure).slice(0, 300));
      probes.screenshot = await page.screenshot({ path: join(output, scenario + '-probe.png'), timeout: 5000 }).then(() => 'ok', failure => 'failed: ' + String(failure).slice(0, 300));
      if (process.env.SEEKDEEP_KEYLESS_PROBE) probes.custom = await page.evaluate(process.env.SEEKDEEP_KEYLESS_PROBE).then(value => JSON.stringify(value), failure => 'failed: ' + String(failure));
      await writeFile(join(output, scenario + '-hang-probes.json'), JSON.stringify(probes, null, 2));
      await Promise.all(hooks.map(hook => hook().catch(() => {})));
      await writeFile(join(output, scenario + '-failure-aria.txt'), await page.locator('body').ariaSnapshot().catch(() => 'unavailable'));
      await writeFile(join(output, scenario + '-failure.json'), JSON.stringify({ case: entry.name, error: String(error), closed: page.isClosed(), tripwire: { warnings: tripwire.warnings, pageErrors: tripwire.pageErrors }, console: pageConsoles.get(page) ?? page.__console?.() ?? [] }, null, 2));
      console.error('keyless: ' + scenario + ': ' + entry.name + ' failed: ' + String(error));
      caseFailure = error;
      if (bindings.afterEach) await bindings.afterEach().catch(cleanup => console.error('keyless: afterEach after a failed case: ' + String(cleanup)));
      throw error;
    }
    checks.push(scenario + ': ' + entry.name); console.log('keyless: ' + scenario + ': ' + entry.name);
    // The source `afterEach` runs after every case (failures included) and its failures are
    // the case's failures: replay consumption is the fixture-drift tripwire.
    if (bindings.afterEach) await bindings.afterEach();
  }
}
const scaffoldAst = await sourceFile('scaffold.ts'), supportAst = await sourceFile('support.ts');
const helpers = declarations(scaffoldAst, ['normalizeAria', 'captureStableAria', 'watchConsole', 'acknowledgeReloadConnectionLoss', 'WELCOME_NOTICE_SETTINGS_NAMESPACE', 'WELCOME_NOTICE_ACK_FIELD', 'WELCOME_NOTICE_VERSION']);
const welcomeValues = compile(helpers + '\nreturn {WELCOME_NOTICE_SETTINGS_NAMESPACE,WELCOME_NOTICE_ACK_FIELD,WELCOME_NOTICE_VERSION};', { expect });
const welcome = { namespace: welcomeValues.WELCOME_NOTICE_SETTINGS_NAMESPACE, field: welcomeValues.WELCOME_NOTICE_ACK_FIELD, version: welcomeValues.WELCOME_NOTICE_VERSION };
// Live Rust runs render SeekDeep package identities where the source goldens pin DeepSeek ones;
// seeded recordings keep the identities their fixtures carry. Either spelling of the golden is exact.
// Product-facing prose the Rust product renames (AGENTS.md): package ids, the product name, and
// the CLI/boot identifiers the Web GUI system prompt names.
const productIdentity = text => text
  .replaceAll('@deepseek-ai/dsh-', '@seekdeep-ai/seekdeep-')
  .replaceAll('powered by DeepSeek Harness', 'powered by SeekDeep Harness')
  .replaceAll('The DeepSeek Harness implementation checkout', 'The SeekDeep Harness implementation checkout')
  .replaceAll('through the DeepSeek Harness Web GUI', 'through the SeekDeep Harness Web GUI')
  .replaceAll('extend DSH itself', 'extend SeekDeep itself')
  .replaceAll('only dsh web injects window.__DSH_BOOT__', 'only seekdeep web injects window.__SEEKDEEP_BOOT__');
const goldenTools = scenario => ({
  compareOrRefreshGolden: async (path, actual, mode) => {
    assert.equal(mode, 'replay'); await writeFile(join(output, scenario + '-' + basename(path) + '.actual'), actual + '\n');
    const expected = await readFile(path, 'utf8');
    if (actual + '\n' !== expected) assert.equal(actual + '\n', productIdentity(expected), 'golden ' + basename(path));
  },
  assertFixtureInventory: async (path, names) => assert.deepEqual((await readdir(path)).sort(), [...names].sort()),
});
// Teardown reports its own failures without masking the case error that preceded it: after a
// failed case the audit (an unconsumed replay, say) is a consequence, so it is logged, not thrown.
let caseFailure;
async function teardown(browser, server) {
  const failures = [];
  await browser.close().catch(error => failures.push(error));
  await server.stop().catch(error => failures.push(error));
  if (caseFailure !== undefined) { for (const error of failures) console.error('keyless: teardown after a failed case: ' + String(error)); return; }
  if (failures.length === 1) throw failures[0];
  if (failures.length > 1) throw new AggregateError(failures, 'scenario teardown failed');
}
const pageConsoles = new WeakMap();
async function openPage(name, locale, viewport = { width: 1680, height: 1000 }) {
  const profile = join(world, name, 'browser');
  const context = await chromium.launchPersistentContext(profile, { headless: true, locale, viewport, args: ['--remote-debugging-port=0'] });
  const cdp = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  const page = context.pages()[0] ?? await context.newPage(); page.setDefaultTimeout(15000);
  // Diagnostics: record every stream frame the page receives (EventSource or fetch streams).
  if (process.env.SEEKDEEP_KEYLESS_SSE_TRACE) await page.addInitScript(() => {
    window.__frames = [];
    const push = text => { try { window.__frames.push(String(text).slice(0, 400)); } catch { /* ignore */ } };
    const OrigES = window.EventSource;
    if (OrigES) { const Wrapped = function (url, init) { const es = new OrigES(url, init); es.addEventListener('message', event => push(event.data)); return es; }; Wrapped.prototype = OrigES.prototype; window.EventSource = Wrapped; }
    const OrigWS = window.WebSocket;
    if (OrigWS) { const WrappedWS = function (url, protocols) { const socket = protocols === undefined ? new OrigWS(url) : new OrigWS(url, protocols); socket.addEventListener('message', event => push(event.data)); return socket; }; WrappedWS.prototype = OrigWS.prototype; for (const key of ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED']) WrappedWS[key] = OrigWS[key]; window.WebSocket = WrappedWS; }
    const origFetch = window.fetch;
    window.fetch = async function (...args) {
      const response = await origFetch.apply(this, args);
      try {
        const type = response.headers.get('content-type') || '';
        if (type.includes('text/event-stream') && response.body) {
          const [kept, tapped] = response.body.tee();
          (async () => { const reader = tapped.getReader(); const decoder = new TextDecoder(); let buffer = ''; for (;;) { const { value, done } = await reader.read(); if (done) break; buffer += decoder.decode(value, { stream: true }); let index; while ((index = buffer.indexOf('\n\n')) >= 0) { push(buffer.slice(0, index)); buffer = buffer.slice(index + 2); } } })();
          return new Response(kept, { status: response.status, statusText: response.statusText, headers: response.headers });
        }
      } catch { /* ignore */ }
      return response;
    };
  });
  await page.addInitScript(() => { window.__seekdeepAnimationStarts = 0; document.addEventListener('animationstart', () => { window.__seekdeepAnimationStarts += 1; }, true); });
  const console = []; page.on('console', message => console.push({ type: message.type(), text: message.text() }));
  page.on('response', response => { const url = new URL(response.url()); if (url.pathname.startsWith('/api/')) response.text().then(body => console.push({ type: 'rpc', text: url.pathname + ' ' + response.status() + ' ' + body.slice(0, 400), request: (response.request().postData() ?? '').slice(0, 400) })).catch(() => {}); }); page.on('pageerror', error => console.push({ type: 'pageerror', text: String(error) })); page.on('crash', () => console.push({ type: 'crash', text: 'page crashed' }));
  pageConsoles.set(page, console);
  return { context, page, cdp, async screenshot(label) { await exec('agent-browser', ['--session', 'seekdeep-keyless-' + name, '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '-' + label + '.png')]); }, async close() { await exec('agent-browser', ['--session', 'seekdeep-keyless-' + name, 'close']).catch(() => {}); await context.close(); } };
}
const { tsImport } = require('tsx/esm/api');
const { parseSessionLog, deriveReplayScript } = await tsImport(join(source, 'packages/test-support/llm-replay/src/index.ts'), { parentURL: import.meta.url, tsconfig: join(source, 'tsconfig.base.json') });
const sourceModule = path => tsImport(join(source, path), { parentURL: import.meta.url, tsconfig: join(source, 'tsconfig.base.json') });
const sessionModule = await sourceModule('packages/core/session/src/index.ts'), llmModule = await sourceModule('packages/llm/llm/src/index.ts'), llmBrandModule = await sourceModule('packages/llm/llm/src/brand.ts');
const fixtureHelpers = declarations(scaffoldAst, ['fixtureUserPrompts']);
const { fixtureUserPrompts } = compile(fixtureHelpers + '\nreturn {fixtureUserPrompts};', { parseSessionLog });
// The source `beforeAll` opens the seeded Session from the sidebar before its cases run.
async function openSeededSession(page, ready) {
  const groupRow = page.locator('[role="treeitem"]').first(); await groupRow.waitFor({ timeout: 15000 }); await groupRow.click();
  const sessionRow = page.locator('[role="treeitem"]').nth(1); await sessionRow.waitFor({ timeout: 10000 }); await sessionRow.click();
  await ready(page);
}
async function seededScenario(name, options) {
  const ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, options.cases, 'source case inventory');
  const constants = declarations(ast, options.constants);
  const values = compile(constants + '\nreturn {' + options.constants.join(',') + '};', options.constantBindings ?? {});
  const server = await bootHost(name, { welcome, seedFile: options.seedFile });
  const browser = await openPage(name, 'en-US');
  try {
    const api = compile(helpers + '\nreturn {watchConsole};', { expect });
    const seedText = options.seedText ? options.seedText(server, values) : undefined;
    if (options.seedFile) assert.deepEqual(fixtureUserPrompts(await readFile(options.seedFile, 'utf8')), [values.PROMPT]);
    await server.seedSession(values.SEED_ID, seedText);
    // The seeded tail page must carry its projection baseline before the browser reads it.
    const probe = await fetch(server.origin + '/api/session.history', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId: name + '-history-probe', method: 'session.history', payload: { sessionId: values.SEED_ID } }) });
    const probed = await probe.json(); await writeFile(join(output, name + '-history-probe.json'), JSON.stringify(probed, null, 2));
    assert.equal(probed.result?.ok, true, 'seeded history tail: ' + JSON.stringify(probed.result ?? probed));
    assert(probed.result.value.projections !== undefined, 'seeded history tail carries no projection baseline');
    const tripwire = api.watchConsole(browser.page);
    await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
    if (options.ready !== undefined) await openSeededSession(browser.page, options.ready);
    const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin };
    const extra = options.bindings ? options.bindings(server, values) : {};
    await runCases(name, cases, browser.page, { prelude: helpers + '\n' + constants, values: { expect, page: browser.page, scaffold, tripwire, MODE, SNAPSHOT_DIR: join(TESTS, 'snapshots', name), UI_EXPECTED: join(TESTS, 'snapshots', name, 'ui.expected.md'), mkdirSync, join, readFile, ...goldenTools(name), ...extra } }, tripwire);
    await browser.screenshot('settled');
  } finally { await browser.close(); await server.stop(); }
}
const seedModule = () => ({ Session: sessionModule.Session, SessionId: sessionModule.SessionId, SESSION_FORMAT_VERSION: sessionModule.SESSION_FORMAT_VERSION, createMessage: llmModule.createMessage, createUserMessage: llmModule.createUserMessage, createAssistantMessage: llmModule.createAssistantMessage, createToolResultMessage: llmModule.createToolResultMessage, CallId: llmBrandModule.CallId });
const generatedSeed = (builder) => (server, values) => builder(seedModule(), server, values);
// The source hands cases its in-process Context; the Rust Host answers the same calls over its
// fixture routes. `agents.get` mirrors the source's synchronous read: it answers from the latest
// listing and kicks a refresh, so a polled `liveAgent` observes the Host resuming the Session.
function hostContext(server, live) {
  const post = async (path, body) => { const response = await fetch(server.origin + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }); const text = await response.text(); assert(response.ok, path + ' HTTP ' + response.status + ': ' + text); return JSON.parse(text); };
  const agentOf = id => ({ sessionId: id, session: { get header() { return live.sessions.find(session => session.id === id)?.header; }, requestHeader: () => live.sessions.find(session => session.id === id)?.events.filter(event => event.type === 'request/header').at(-1)?.data.header } });
  return {
    agents: { get: id => { if (live.sessions.some(session => session.id === id)) return agentOf(id); live.refresh().catch(() => {}); return undefined; }, list: live.agents.list },
    sessions: { list: () => live.sessions.map(session => ({ id: session.id, header: session.header, append: (type, data) => post('/fixture/session/' + encodeURIComponent(session.id) + '/append', { type, data }).catch(error => console.error('keyless: session append failed: ' + String(error))) })) },
    on: (event, listener) => { assert.equal(event, 'session/event', 'only session/event listeners are shimmed'); live.onEvent(listener); return () => {}; },
    tools: { execute: ({ callId, name, arguments: args, agent }) => post('/fixture/tool/execute', { sessionId: agent.sessionId, callId, name, arguments: args }) },
    jobs: { kill: (jobId, agent, reason) => post('/fixture/job/kill', { jobId, sessionId: agent.sessionId, reason }) },
    credentials: { set: (ref, value) => post('/fixture/credential', { ref, value }) },
  };
}
// A pinned replay suite: the recorded session.jsonl drives a real Rust Agent turn while the
// source case supplies every gesture, and the turn settles through the Host idle barrier.
async function replayScenario(name, options) {
  const ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, options.cases, 'source case inventory');
  const constants = declarations(ast, options.constants), support = declarations(supportAst, ['connectFreshWorkspace']);
  const constantValues = compile(constants + '\nreturn {' + options.constants.join(',') + '};', options.constantBindings ?? {});
  const dir = join(TESTS, 'snapshots', options.dir ?? name);
  const prepared = options.prepare ? await options.prepare(constantValues) : {};
  const FIXTURE = prepared.fixture ?? join(dir, 'session.jsonl');
  const overlay = typeof options.overlay === 'function' ? options.overlay(constantValues, prepared) : options.overlay;
  const server = await bootHost(name, { welcome, replay: FIXTURE, replayOverride: prepared.replayOverride, overlay, env: { ...options.pace === undefined ? {} : { SEEKDEEP_KEYLESS_REPLAY_PACE_MS: String(options.pace) }, ...options.env ?? {} } });
  const browser = await openPage(name, 'en-US');
  try {
    const live = liveSessions(server.origin);
    const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, whenTurnSettled: live.whenTurnSettled, ctx: hostContext(server, live) };
    if (options.booted) await options.booted(scaffold, prepared, constantValues);
    const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspace};', { expect, mkdirSync, join });
    const tripwire = api.watchConsole(browser.page);
    await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
    await api.connectFreshWorkspace(browser.page, server.workspace);
    const goldens = Object.fromEntries(options.goldens.map(golden => Array.isArray(golden) ? [golden[0], join(dir, golden[1] + '.expected.md')] : [golden.toUpperCase().replaceAll('-', '_') + '_EXPECTED', join(dir, golden + '.expected.md')]));
    const extra = options.values ? options.values({ server, browser, live, scaffold, prepared, name }) : {};
    if (options.adaptCases) for (const entry of cases) entry.callback = options.adaptCases(entry.callback);
    try {
      await runCases(name, cases, browser.page, { prelude: helpers + '\n' + fixtureHelpers + '\n' + constants + '\n' + (options.prelude ?? ''), shared: options.shared, values: { expect: live.expect, page: browser.page, scaffold, tripwire, sessionEvents: live.events, MODE, FIXTURE, SNAPSHOT_DIR: dir, ...goldens, readFile, join, parseSessionLog, recordFixture: () => { throw new Error('record mode is not a replay lane'); }, ...goldenTools(name), ...prepared.values ?? {}, ...extra } }, tripwire);
    } finally {
      await live.refresh().catch(() => {});
      live.stop();
      await writeFile(join(output, name + '-sessions.json'), JSON.stringify(live.sessions, null, 2));
    }
    await browser.screenshot('settled');
  } finally { await teardown(browser, server); if (options.finish) await options.finish(prepared); }
}
// A cold-seeded suite over a generated or recorded log: the source `beforeAll` seeds, then the
// cases open the Session from the sidebar themselves.
async function seededCustomScenario(name, options) {
  const ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, options.cases, 'source case inventory');
  const constants = declarations(ast, options.constants);
  const values = compile(constants + '\nreturn {' + options.constants.join(',') + '};', { ...seedModule(), createServer, ...options.constantBindings ?? {} });
  const prepared = options.prepare ? await options.prepare(values) : {};
  const server = await bootHost(name, { welcome, overlay: options.overlay, env: options.env, seedFile: options.seedFile });
  const browser = await openPage(name, 'en-US');
  try {
    const live = liveSessions(server.origin);
    await server.seedSession(values.SEED_ID, options.seedText ? options.seedText(values, prepared) : undefined);
    const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, ctx: hostContext(server, live), ...options.scaffold ?? {} };
    if (options.viewport) await browser.page.setViewportSize(options.viewport);
    const api = compile(helpers + '\nreturn {watchConsole};', { expect });
    const tripwire = api.watchConsole(browser.page);
    await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
    const shared = options.shared ? await options.shared({ server, browser, live, scaffold, values, constants }) : undefined;
    if (options.adaptCases) for (const entry of cases) entry.callback = options.adaptCases(entry.callback);
    const dir = join(TESTS, 'snapshots', name);
    const goldens = Object.fromEntries((options.goldens ?? []).map(golden => [golden.toUpperCase().replaceAll('-', '_') + '_EXPECTED', join(dir, golden + '.expected.md')]));
    const extra = options.values ? options.values({ server, browser, live, scaffold, values, prepared }) : {};
    try {
      await runCases(name, cases, browser.page, { prelude: helpers + '\n' + constants, shared, values: { expect: live.expect, page: browser.page, scaffold, tripwire, MODE, SNAPSHOT_DIR: dir, ...goldens, ...seedModule(), createServer, ...goldenTools(name), ...prepared.values ?? {}, ...extra } }, tripwire);
      if (options.verify) await options.verify({ server, browser, live, scaffold, values, prepared });
    } finally { live.stop(); }
    await browser.screenshot('settled');
  } finally { await teardown(browser, server); if (options.finish) await options.finish(prepared); }
}
// Suites whose cases boot their own scaffold (`launchWebScaffold` inside `it`/`launch`) get the
// source's module-level faces as shims: each call boots one keyless Host, `chromium.launch()`
// opens one browser context, and the source `afterEach` closes both through `shared`.
function scaffoldShims(name, options = {}) {
  let hosts = 0, contexts = 0;
  const openHosts = new Set(), openBrowsers = new Set();
  const launchWebScaffold = async (scaffoldOptions = {}) => {
    hosts += 1;
    const server = await bootHost(name + '-' + hosts, { welcome, replay: scaffoldOptions.replayFixture, replayOverride: scaffoldOptions.replayOverride, replayChildFixtures: scaffoldOptions.replayChildFixtures, overlay: options.overlay, env: { ...scaffoldOptions.paceMs === undefined ? {} : { SEEKDEEP_KEYLESS_REPLAY_PACE_MS: String(scaffoldOptions.paceMs) }, ...options.env ?? {} } });
    const live = liveSessions(server.origin);
    const scaffold = { baseUrl: server.origin, workspaceCwd: server.workspace, whenTurnSettled: live.whenTurnSettled, whenTurnsSettled: live.whenTurnsSettled, ctx: hostContext(server, live), live, server, async close() { openHosts.delete(scaffold); await live.refresh().catch(() => {}); live.stop(); await writeFile(join(output, name + '-' + hosts + '-sessions.json'), JSON.stringify(live.sessions, null, 2)); await server.stop(); } };
    openHosts.add(scaffold);
    return scaffold;
  };
  const chromium = { launch: async () => {
    const pages = [];
    const browser = { async newPage(pageOptions = {}) { contexts += 1; const opened = await openPage(name + '-' + contexts, pageOptions.locale ?? 'en-US', pageOptions.viewport); pages.push(opened); return opened.page; }, async close() { openBrowsers.delete(browser); for (const opened of pages.splice(0)) await opened.close(); } };
    openBrowsers.add(browser);
    return browser;
  } };
  const newEnglishPage = (browser, height = 1000) => browser.newPage({ viewport: { width: 1680, height }, locale: 'en-US' });
  // Whatever a failed case left open is closed after it, like the source `afterEach`.
  const closeAll = async () => { const failures = []; for (const browser of [...openBrowsers]) await browser.close().catch(error => failures.push(error)); for (const scaffold of [...openHosts]) await scaffold.close().catch(error => failures.push(error)); if (failures.length) throw failures.length === 1 ? failures[0] : new AggregateError(failures, name + ' teardown failed'); };
  return { launchWebScaffold, chromium, newEnglishPage, closeAll };
}
async function perCaseScenario(name, options) {
  const ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, options.cases, 'source case inventory');
  const constants = declarations(ast, options.constants), nested = options.nested ? nestedDeclarations(ast, options.nested) : '', support = declarations(supportAst, ['connectFreshWorkspace']);
  const dir = join(TESTS, 'snapshots', name), FIXTURE = options.fixture ? join(TESTS, 'snapshots', options.fixture, 'session.jsonl') : join(dir, 'session.jsonl');
  const goldens = Object.fromEntries((options.goldens ?? []).map(golden => Array.isArray(golden) ? [golden[0], join(dir, golden[1] + '.expected.md')] : [golden.toUpperCase().replaceAll('-', '_') + '_EXPECTED', join(dir, golden + '.expected.md')]));
  const shims = scaffoldShims(name, options);
  const shared = { scaffold: undefined, browser: undefined, page: undefined, tripwire: undefined, sessionEvents: undefined, sidecarDir: undefined, overrideDir: undefined, ...options.shared ?? {} };
  const afterEach = async () => {
    // The source `afterEach` body: close the browser and scaffold the case left in describe scope,
    // remove its temp dir, and rethrow what failed.
    const failures = [];
    if (shared.browser) await shared.browser.close().catch(error => failures.push(error));
    shared.browser = undefined;
    const closing = shared.scaffold; shared.scaffold = undefined;
    if (closing) await closing.close().catch(error => failures.push(error));
    await shims.closeAll().catch(error => failures.push(error));
    for (const key of ['sidecarDir', 'overrideDir']) { if (shared[key] !== undefined) await rm(shared[key], { recursive: true, force: true }).catch(error => failures.push(error)); shared[key] = undefined; }
    if (failures.length) throw failures.length === 1 ? failures[0] : new AggregateError(failures, name + ' teardown failed');
  };
  const api = compile(helpers + '\nreturn {watchConsole};', { expect });
  const tripwireProxy = new Proxy({}, { get: (_, key) => (shared.tripwire ?? { warnings: [], pageErrors: [] })[key] });
  await runCases(name, cases, { __console: () => pageConsoles.get(shared.page) ?? [], isClosed: () => (shared.page ? shared.page.isClosed() : true), evaluate: (...args) => shared.page ? shared.page.evaluate(...args) : Promise.reject(new Error('no page')), locator: (...args) => shared.page.locator(...args), screenshot: (...args) => shared.page ? shared.page.screenshot(...args) : Promise.resolve() }, { prelude: helpers + '\n' + fixtureHelpers + '\n' + support + '\n' + constants + '\n' + nested, shared, afterEach, values: { expect, MODE, FIXTURE, SNAPSHOT_DIR: dir, ...goldens, readFile, writeFile, mkdtemp, rm, tmpdir, join, existsSync, mkdirSync, parseSessionLog, deriveReplayScript, watchConsole: api.watchConsole, recordFixture: () => { throw new Error('record mode is not a replay lane'); }, ...shims, ...goldenTools(name), ...options.values ?? {} } }, tripwireProxy);
}
const SCENARIOS = {
  async 'skill-tool-row'() {
    await seededScenario('skill-tool-row', { cases: 2, constants: ['SEED_ID', 'PROMPT'], seedFile: join(source, 'examples/acp-agent/tests/snapshots/skill-load/session.jsonl'), ready: page => page.locator('[data-tool="skill"]').waitFor({ timeout: 15000 }) });
  },
  async 'bash-abort-row'() {
    await seededScenario('bash-abort-row', { cases: 2, constants: ['SEED_ID', 'PROMPT'], seedFile: join(source, 'examples/acp-agent/tests/snapshots/cancel-tool-calls/session.jsonl'), ready: page => page.locator('[data-sample="bash"]').nth(1).waitFor({ timeout: 15000 }) });
  },
  async 'markdown-cjk-strong'() {
    const ast = await sourceFile('markdown-cjk-strong.e2e.ts'), builder = declarations(ast, ['CASES', 'DONE', 'markdownFixture']);
    await seededScenario('markdown-cjk-strong', { cases: 1, constants: ['SEED_ID', 'DONE', 'CASES'], seedText: generatedSeed(module => compile(builder + '\nreturn markdownFixture();', module)) });
  },
  async 'math-rendering'() {
    const ast = await sourceFile('math-rendering.e2e.ts'), builder = declarations(ast, ['DONE', 'mathFixture']);
    await seededScenario('math-rendering', { cases: 1, constants: ['SEED_ID', 'DONE'], seedText: generatedSeed(module => compile(builder + '\nreturn mathFixture();', module)) });
  },
  async 'markdown-inline-code-links'() {
    const ast = await sourceFile('markdown-inline-code-links.e2e.ts'), builder = declarations(ast, ['DONE', 'markdownFixture']);
    let linkUrl;
    await seededScenario('markdown-inline-code-links', { cases: 1, constants: ['SEED_ID', 'DONE'], seedText: generatedSeed((module, server) => { linkUrl = new URL('/?demo=1', server.origin).toString(); return compile(builder + '\nreturn markdownFixture(linkUrl);', { ...module, linkUrl }); }), bindings: () => ({ get linkUrl() { return linkUrl; } }) });
  },
  async 'stats-paged-history'() {
    const ast = await sourceFile('stats-paged-history.e2e.ts'), builder = declarations(ast, ['TURNS', 'buildSeed']);
    await seededScenario('stats-paged-history', { cases: 3, constants: ['SEED_ID', 'TURNS', 'FULL_COUNTS'], seedText: () => compile(builder + '\nreturn buildSeed(TURNS);', {}) });
  },
  async 'message-feedback'() {
    const name = 'message-feedback', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 2, 'source case inventory');
    const constants = declarations(ast, ['SEED_ID', 'NOTE']), nested = nestedDeclarations(ast, ['openSeededSession']);
    const values = compile(constants + '\nreturn {SEED_ID, NOTE};', {});
    const server = await bootHost(name, { welcome, seedFile: join(TESTS, 'snapshots/seeded-history/seed.jsonl') });
    const browser = await openPage(name, 'en-US');
    try {
      await server.seedSession(values.SEED_ID);
      const api = compile(helpers + '\nreturn {watchConsole};', { expect });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin };
      await runCases(name, cases, browser.page, { prelude: helpers + '\n' + constants + '\n' + nested, values: { expect, page: browser.page, scaffold, tripwire, MODE, ...goldenTools(name) } }, tripwire);
      await browser.screenshot('retracted');
    } finally { await teardown(browser, server); }
  },
  async 'skill-invocation-policy'() {
    const name = 'skill-invocation-policy', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 1, 'source case inventory');
    const seeding = declarations(ast, ['SKILLS', 'seedSkills']), support = declarations(supportAst, ['connectFreshWorkspace']);
    const server = await bootHost(name, { welcome });
    const browser = await openPage(name, 'en-US');
    try {
      const { seedSkills } = compile(seeding + '\nreturn {seedSkills};', { mkdir, writeFile, join });
      await seedSkills(server.workspace);
      const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspace};', { expect, mkdirSync, join });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await api.connectFreshWorkspace(browser.page, server.workspace);
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin };
      await runCases(name, cases, browser.page, { prelude: helpers + '\n' + seeding, values: { expect, page: browser.page, scaffold, tripwire, MODE, SNAPSHOT_DIR: join(TESTS, 'snapshots', name), MENU_EXPECTED: join(TESTS, 'snapshots', name, 'menu.expected.md'), mkdir, writeFile, join, ...goldenTools(name) } }, tripwire);
      await browser.screenshot('menu');
    } finally { await teardown(browser, server); }
  },
  async 'produced-file-mentions'() {
    const ast = await sourceFile('produced-file-mentions.e2e.ts'), builder = declarations(ast, ['DONE', 'text', 'WRITES', 'mentionFixture']);
    await seededScenario('produced-file-mentions', { cases: 1, constants: ['SEED_ID', 'DONE'], seedText: generatedSeed(module => compile(builder + '\nreturn mentionFixture();', module)) });
  },
  async 'message-actions'() {
    const name = 'message-actions', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 4, 'source case inventory');
    const constants = declarations(ast, ['SEED_ID', 'PROMPT', 'MID_TURN_TEXT', 'SECOND_PROMPT', 'completedTailFixture']);
    const values = compile(constants + '\nreturn {SEED_ID, PROMPT, SECOND_PROMPT, completedTailFixture};', {});
    const server = await bootHost(name, { welcome });
    const browser = await openPage(name, 'en-US');
    try {
      const live = liveSessions(server.origin);
      const sessionCwd = join(server.workspace, 'workspace');
      await mkdir(sessionCwd, { recursive: true }); await writeFile(join(sessionCwd, 'a.txt'), 'alpha\n'); await writeFile(join(sessionCwd, 'b.txt'), 'beta\n');
      const raw = values.completedTailFixture(await readFile(join(TESTS, 'snapshots/seeded-history/seed.jsonl'), 'utf8'));
      assert.deepEqual(fixtureUserPrompts(raw), [values.PROMPT, values.SECOND_PROMPT], 'adapted seed must carry both prompts');
      await server.seedSession(values.SEED_ID, raw);
      const api = compile(helpers + '\nreturn {watchConsole};', { expect });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, ctx: { agents: live.agents } };
      try {
        await runCases(name, cases, browser.page, { prelude: helpers + '\n' + constants, values: { expect: live.expect, page: browser.page, scaffold, tripwire, MODE, SNAPSHOT_DIR: join(TESTS, 'snapshots', name), UI_EXPECTED: join(TESTS, 'snapshots', name, 'ui.expected.md'), FORK_EXPECTED: join(TESTS, 'snapshots', name, 'fork.expected.md'), SessionId: id => id, ...goldenTools(name) } }, tripwire);
      } finally {
        await live.refresh().catch(() => {});
        await writeFile(join(output, name + '-sessions.json'), JSON.stringify(live.sessions.map(session => ({ header: session.header, events: session.events.length })), null, 2));
      }
      await browser.screenshot('forked');
    } finally { await teardown(browser, server); }
  },
  async 'goal-multi-turn-actions'() {
    const name = 'goal-multi-turn-actions', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 3, 'source case inventory');
    const constants = declarations(ast, ['PROMPT', 'COMMAND', 'PACKAGE_FILES', 'seedPackageInventory', 'goalRounds', 'createdObjectives']), support = declarations(supportAst, ['connectFreshWorkspace']);
    const nested = nestedDeclarations(ast, ['runGoal']);
    const FIXTURE = join(TESTS, 'snapshots', name, 'session.jsonl'), OVERRIDE = join(TESTS, 'snapshots', name, 'replay.override.json');
    const server = await bootHost(name, { welcome, replay: FIXTURE, replayOverride: OVERRIDE });
    const browser = await openPage(name, 'en-US');
    try {
      const live = liveSessions(server.origin);
      const { seedPackageInventory } = compile(constants + '\nreturn {seedPackageInventory};', { mkdir, writeFile, join, dirname });
      await seedPackageInventory(server.workspace);
      const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspace};', { expect, mkdirSync, join });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await api.connectFreshWorkspace(browser.page, server.workspace);
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, whenTurnSettled: live.whenTurnSettled };
      // The source `launch` boots the scaffold the driver already prepared; the case sees the same page.
      try {
        await runCases(name, cases, browser.page, { prelude: helpers + '\n' + fixtureHelpers + '\n' + constants + '\n' + nested, values: { expect: live.expect, page: browser.page, scaffold, tripwire, sessionEvents: live.events, MODE, FIXTURE, OVERRIDE, SNAPSHOT_DIR: join(TESTS, 'snapshots', name), UI_EXPECTED: join(TESTS, 'snapshots', name, 'ui.expected.md'), readFile, join, dirname, mkdir, writeFile, parseSessionLog, launch: async () => {}, whenTurnsSettled: (_scaffold, count, timeoutMs) => live.whenTurnsSettled(count, timeoutMs), recordFixture: () => { throw new Error('record mode is not a replay lane'); }, ...goldenTools(name) } }, tripwire);
      } finally {
        await live.refresh().catch(() => {});
        live.stop();
        await writeFile(join(output, name + '-sessions.json'), JSON.stringify(live.sessions, null, 2));
      }
      await browser.screenshot('settled');
    } finally { await teardown(browser, server); }
  },
  async 'remote-welcome'() {
    const name = 'remote-welcome', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 1, 'source case inventory');
    const copy = declarations(scaffoldAst, ['WELCOME_NOTICE_COPY']);
    // Source: `remoteAuthority: 'remote.localhost'` patches the connection row's trusted hosts; the
    // browser resolves the authority to loopback while the Host stays bound to 127.0.0.1.
    const server = await bootHost(name, { welcome, welcomePending: true, overlay: JSON.stringify([{ id: 'connection', config: { trustedHosts: ['remote.localhost'] } }]) + '\n' });
    const browser = await openPage(name, 'zh-CN', { width: 1440, height: 960 });
    try {
      const remoteOrigin = server.origin.replace('127.0.0.1', 'remote.localhost');
      const api = compile(helpers + '\nreturn {watchConsole};', { expect });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(remoteOrigin, { waitUntil: 'load' }); await browser.page.waitForSelector('#root', { timeout: 30000 });
      const scaffold = { workspaceCwd: server.workspace, baseUrl: remoteOrigin };
      await runCases(name, cases, browser.page, { prelude: helpers + '\n' + copy, values: { expect, page: browser.page, scaffold, tripwire, MODE, ...goldenTools(name) } }, tripwire);
      await browser.screenshot('welcome');
    } finally { await teardown(browser, server); }
  },
  async 'turn-tail-actions'() {
    const name = 'turn-tail-actions', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 3, 'source case inventory');
    const constants = declarations(ast, ['NARRATION', 'PROMPT']), nested = nestedDeclarations(ast, ['sendPrompt']), support = declarations(supportAst, ['connectFreshWorkspace']);
    const FIXTURE = join(TESTS, 'snapshots', name, 'session.jsonl');
    // The source materializes the hang sidecar before the replay row installs; the marker path is
    // the case's own synchronization, so the sidecar home is created here and handed to `launch`.
    const sidecarDir = join(world, name, 'sidecar'); await mkdir(sidecarDir, { recursive: true });
    const overridePath = join(sidecarDir, 'replay.override.json');
    await writeFile(overridePath, JSON.stringify({ patches: [{ at: 1, entry: { kind: 'hang', readyFile: join(sidecarDir, '.hang-ready') } }] }));
    const server = await bootHost(name, { welcome, replay: FIXTURE, replayOverride: overridePath });
    const browser = await openPage(name, 'en-US');
    try {
      const live = liveSessions(server.origin);
      const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspace};', { expect, mkdirSync, join });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await api.connectFreshWorkspace(browser.page, server.workspace);
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, whenTurnSettled: live.whenTurnSettled };
      try {
        await runCases(name, cases, browser.page, { prelude: helpers + '\n' + fixtureHelpers + '\n' + constants + '\n' + nested, values: { expect: live.expect, page: browser.page, scaffold, tripwire, sessionEvents: live.events, MODE, FIXTURE, SNAPSHOT_DIR: join(TESTS, 'snapshots', name), RUNNING_EXPECTED: join(TESTS, 'snapshots', name, 'running.expected.md'), SETTLED_EXPECTED: join(TESTS, 'snapshots', name, 'settled.expected.md'), readFile, join, existsSync, parseSessionLog, launch: async buildOverride => { if (buildOverride) assert.deepEqual(buildOverride(sidecarDir), JSON.parse(await readFile(overridePath, 'utf8')), 'the case builds the override the Host was booted with'); }, recordFixture: () => { throw new Error('record mode is not a replay lane'); }, ...goldenTools(name) } }, tripwire);
      } finally {
        await live.refresh().catch(() => {});
        live.stop();
        await writeFile(join(output, name + '-sessions.json'), JSON.stringify(live.sessions, null, 2));
      }
      await browser.screenshot('settled');
    } finally { await teardown(browser, server); }
  },
  async 'permission-policy-context'() {
    const name = 'permission-policy-context', ast = await sourceFile(name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 3, 'source case inventory');
    const constants = declarations(ast, ['PRESET_LABELS', 'requestSystems', 'runtimeContexts', 'assistantTexts', 'callArgs']), support = declarations(supportAst, ['connectFreshWorkspace']);
    const PROMPTS = verbatimConstant(ast, 'PROMPTS');
    const { canonicalPath } = await sourceModule('packages/sandbox/sandbox/src/roots.ts');
    const FIXTURE = join(TESTS, 'snapshots', name, 'session.jsonl');
    // Source: `ctx.on('approval/request', () => 'allowed-once', { prepend: true })` on the Host.
    const server = await bootHost(name, { welcome, replay: FIXTURE, env: { SEEKDEEP_KEYLESS_AUTO_APPROVE: 'allowed-once' } });
    const browser = await openPage(name, 'en-US');
    try {
      const live = liveSessions(server.origin);
      const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspace};', { expect, mkdirSync, join });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await api.connectFreshWorkspace(browser.page, server.workspace);
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, whenTurnSettled: live.whenTurnSettled };
      const sessionWorkspace = { get value() { return live.sessions.find(session => session.header.cwd)?.header.cwd; } };
      try {
        await runCases(name, cases, browser.page, { prelude: helpers + '\n' + fixtureHelpers + '\n' + constants + '\nlet sessionWorkspace = sessionWorkspaceRef.value;\n', values: { expect: live.expect, page: browser.page, scaffold, tripwire, sessionEvents: live.events, sessionWorkspaceRef: sessionWorkspace, PROMPTS, MODE, FIXTURE, SNAPSHOT_DIR: join(TESTS, 'snapshots', name), readFile, join, parseSessionLog, canonicalPath, recordFixture: () => { throw new Error('record mode is not a replay lane'); }, ...goldenTools(name) } }, tripwire);
      } finally {
        await live.refresh().catch(() => {});
        live.stop();
        await writeFile(join(output, name + '-sessions.json'), JSON.stringify(live.sessions, null, 2));
      }
      await browser.screenshot('settled');
    } finally { await teardown(browser, server); }
  },
  async 'plan-review'() {
    await replayScenario('plan-review', { cases: 2, constants: ['TASK', 'LINE'], goldens: ['review', 'sidebar', 'approved'] });
  },
  async 'question-composer'() {
    await replayScenario('question-composer', { cases: 2, constants: ['PROMPT'], goldens: ['ui', 'sidebar', 'composed', 'answered'] });
  },
  async 'approval-composer'() {
    await replayScenario('approval-composer', { cases: 2, constants: ['TOKENS', 'PROMPT', 'CAP_PROBE'], goldens: ['ui'] });
  },
  async 'goal-command-presentation'() {
    const ast = await sourceFile('goal-command-presentation.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 2, 'source case inventory');
    const support = declarations(supportAst, ['connectFreshWorkspace']);
    const server = await bootHost('goal-command-presentation', { welcome });
    const browser = await openPage('goal-command-presentation', 'en-US');
    try {
      const live = liveSessions(server.origin);
      const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspace};', { expect, mkdirSync, join });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await api.connectFreshWorkspace(browser.page, server.workspace);
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin, ctx: { sessions: { list: () => live.sessions } } };
      await runCases('goal-command-presentation', cases, browser.page, { prelude: helpers + '\n' + support, values: { expect: live.expect, page: browser.page, scaffold, tripwire, events: live.events, MODE, SNAPSHOT_DIR: join(TESTS, 'snapshots/goal-command-presentation'), UI_EXPECTED: join(TESTS, 'snapshots/goal-command-presentation/ui.expected.md'), mkdirSync, join, ...goldenTools('goal-command-presentation') } }, tripwire);
      await browser.screenshot('reloaded');
    } finally { await teardown(browser, server); }
  },
  async 'access-confirmation'() {
    const ast = await sourceFile('access-confirmation.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, 2, 'source case inventory');
    const support = declarations(supportAst, ['connectFreshWorkspaceZh']);
    const server = await bootHost('access-confirmation', { welcome });
    const browser = await openPage('access-confirmation', 'zh-CN');
    try {
      const api = compile(helpers + '\n' + support + '\nreturn {watchConsole, connectFreshWorkspaceZh};', { expect, mkdirSync, join });
      const tripwire = api.watchConsole(browser.page);
      await browser.page.goto(server.origin, { waitUntil: 'load' }); await browser.page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await api.connectFreshWorkspaceZh(browser.page, server.workspace);
      const scaffold = { workspaceCwd: server.workspace, baseUrl: server.origin };
      await runCases('access-confirmation', cases, browser.page, { prelude: helpers + '\n' + support, values: { expect, page: browser.page, scaffold, tripwire, MODE, SNAPSHOT_DIR: join(TESTS, 'snapshots/access-confirmation'), UI_EXPECTED: join(TESTS, 'snapshots/access-confirmation/ui.expected.md'), mkdirSync, join, ...goldenTools('access-confirmation') } }, tripwire);
      await browser.screenshot('full-access');
    } finally { await teardown(browser, server); }
  },
  async 'live-interactions'() {
    await perCaseScenario('live-interactions', { cases: 6, constants: ['AUTH_PROVIDER_MESSAGE', 'PROMPT', 'turnEndReasons'], nested: ['launch', 'sendPrompt'], goldens: ['cancel', 'loading', ['ERROR_EXPECTED', 'error-auth'], 'retry'] });
  },
  async 'queue-actions'() {
    await perCaseScenario('queue-actions', { cases: 3, constants: ['ACTIVE_PROMPT', 'REMOVE', 'EDIT', 'EDITED', 'TAIL', 'WAKE', 'turnEndReasons'], fixture: 'live-interactions', goldens: ['collapsed', 'editing', 'layout', 'preserved', 'ui'] });
  },
  async 'skill-user-invoke'() {
    // Override-only replay: the fixture path never exists; the override document carries the reply.
    await replayScenario('skill-user-invoke', { cases: 2, constants: ['SKILL_NAME', 'ARGS_TEXT', 'REPLY', 'seedUserOnlySkill', 'REPLAY'], goldens: ['ui'], pace: 10,
      constantBindings: { mkdir, writeFile, join },
      prepare: async values => { const replayDir = await mkdtemp(join(tmpdir(), 'seekdeep-skill-user-invoke-replay-')); const replayOverride = join(replayDir, 'replay.override.json'); await writeFile(replayOverride, JSON.stringify(values.REPLAY)); return { replayDir, fixture: join(replayDir, 'override-only.jsonl'), replayOverride }; },
      booted: async (scaffold, _prepared, values) => { await values.seedUserOnlySkill(scaffold.workspaceCwd); },
      finish: async prepared => { await rm(prepared.replayDir, { recursive: true, force: true }); } });
  },
  async 'markdown-images'() {
    // The remote image origin is the suite's own Node server; the Host never sees it.
    await seededCustomScenario('markdown-images', { cases: 1, constants: ['SEED_ID', 'REMOTE_ALT', 'LOCAL_ALT', 'PNG', 'startImageOrigin', 'stopServer', 'markdownImageFixture'], goldens: ['ui'],
      prepare: async values => { const imageOrigin = await values.startImageOrigin(); return { imageOrigin, values: { imageOrigin } }; },
      seedText: (values, prepared) => values.markdownImageFixture(prepared.imageOrigin.url),
      finish: async prepared => { await new Promise((resolve, reject) => prepared.imageOrigin.server.close(error => error === undefined ? resolve() : reject(error))); } });
  },
  async 'produced-files'() {
    // Source: `vi.spyOn(scaffold.ctx.apiProxy.host, 'openPath')` replaces the Host method behind
    // the wire request. The Host's stub answers the same request; the spy records the wire
    // requests the page issues and the Host stub's own record is checked after the case.
    await seededCustomScenario('produced-files', { cases: 1, constants: ['SEED_ID', 'DONE', 'PRODUCED', 'producedFixture'],
      overlay: await readFile(join(TESTS, 'produced-files.overlay.yml'), 'utf8'), env: { SEEKDEEP_KEYLESS_STUB_OPEN_PATH: '1' }, viewport: { width: 1280, height: 900 },
      seedText: values => values.producedFixture(), scaffold: { ctx: { apiProxy: { host: {} } } },
      adaptCases: text => text.replace('expect(openPath).toHaveBeenCalledTimes(1)', 'expect(openPath.mock.calls.length).toBe(1)'),
      values: ({ browser }) => ({ vi: { spyOn: (target, method) => { assert.equal(method, 'openPath'); const calls = []; const listener = request => { if (new URL(request.url()).pathname === '/api/host.openPath') calls.push([request.postDataJSON()]); }; browser.page.on('request', listener); return { _isMockFunction: true, getMockName: () => method, mock: { calls }, mockImplementation() { return this; }, mockRestore: () => browser.page.off('request', listener) }; } } }),
      verify: async ({ server }) => { const stubbed = await (await fetch(server.origin + '/fixture/open-path')).json(); assert.deepEqual(stubbed, [{ path: server.workspace + '/.' }], 'the Host open-path stub received the folder request'); } });
  },
  async 'background-job-list'() {
    // The source calls its in-process registry synchronously; over the wire the kill is one request.
    await seededCustomScenario('background-job-list', { cases: 3, constants: ['SEED_ID', 'COMMAND', 'liveAgent'], seedFile: join(TESTS, 'snapshots', 'fresh-round-trip', 'session.jsonl'), goldens: ['running', 'settled'],
      constantBindings: { SessionId: id => id },
      shared: async ({ browser, scaffold, values }) => { await openSeededSession(browser.page, async () => {}); return { agent: await values.liveAgent(scaffold, values.SEED_ID), jobId: undefined }; },
      // The port names classes `seekdeep-<package>-<local>`; the source's bare `menu` local name is the jobs list's.
      adaptCases: text => text.replace('expect(scaffold.ctx.jobs.kill(', 'expect(await scaffold.ctx.jobs.kill(').replaceAll('[class*="menu"]', '[class*="seekdeep-jobs-menu"]'),
      values: () => ({ CallId: id => id, JobId: id => id, SessionId: id => id }) });
  },
  async 'replay-round-trip'() {
    // The port's checkout is the workspace root the lane runs from; the golden's `{{sourceRoot}}`.
    await replayScenario('replay-round-trip', { cases: 7, dir: 'fresh-round-trip', constants: ['PROMPT'], goldens: ['ui', ['SYSTEM_PROMPT_EXPECTED', 'system-prompt']], pace: 15, shared: { settledSessionId: undefined },
      values: () => ({ CallId: id => id, REPO_ROOT: process.cwd() }) });
  },
  async 'code-mode-round'() {
    await replayScenario('code-mode-round', { cases: 6, constants: ['PROMPT'], goldens: ['ui'], pace: 15, overlay: JSON.stringify([{ id: 'tools', config: { mode: 'code' } }]) + '\n' });
  },
  async 'cordis-tool-round'() {
    await replayScenario('cordis-tool-round', { cases: 5, constants: ['CORDIS_TOOLS', 'PACKAGE_CODE', 'CLIENT_CODE', 'PROMPT', 'STOP_PROMPT', 'assertCompleteCordisLifecycle'], goldens: ['ui'], pace: 15,
      overlay: JSON.stringify([{ insert: [{ id: 'tool-cordis', name: '@seekdeep-ai/seekdeep-tool-cordis' }] }]) + '\n' });
  },
  async 'feedback-command'() {
    // Source: the telemetry row stays mounted in FULL mode against a loopback discard port.
    await replayScenario('feedback-command', { cases: 3, constants: ['TELEMETRY_URL', 'PROMPT'], goldens: ['ack'], env: { SEEKDEEP_TELEMETRY_DISABLED: undefined },
      overlay: values => JSON.stringify([{ id: 'session-telemetry-otel', disabled: false, config: { mode: 'FULL', exporter: { url: values.TELEMETRY_URL }, shutdownTimeoutMillis: 1000 } }]) + '\n' });
  },
  async 'lifecycle-chrome'() {
    // The active-Plan case boots its own scaffold: a second keyless Host in route-only mode.
    await replayScenario('lifecycle-chrome', { cases: 7, constants: ['PROMPT', 'REPLAY_PACE_MS'], prelude: declarations(supportAst, ['connectFreshWorkspace']), goldens: ['hero', 'command-menu', ['FUZZY_COMMAND_MENU_EXPECTED', 'command-menu-fuzzy'], 'plan-active', 'reloaded'], pace: 100,
      values: ({ browser, name }) => ({ mkdirSync, browser: browser.context, launchWebScaffold: async () => { const active = await bootHost(name + '-active', { welcome }); return { baseUrl: active.origin, workspaceCwd: active.workspace, close: async () => { await writeFile(join(output, name + '-active-sessions.json'), await (await fetch(active.origin + '/fixture/sessions')).text()); await active.stop(); } }; }, newEnglishPage: async (owner, height = 1000) => { const page = await owner.newPage({ viewport: { width: 1680, height } }); page.setDefaultTimeout(15000); return page; } }) });
  },
  async 'web-search-round'() {
    const searchAst = await sourceFile('../../../packages/web/tool-web/src/search.ts');
    const { WEB_SEARCH_MAX_RESULTS } = compile(declarations(searchAst, ['WEB_SEARCH_MAX_RESULTS']) + '\nreturn {WEB_SEARCH_MAX_RESULTS};', {});
    await replayScenario('web-search-round', { cases: 6, constants: ['QUERY', 'PROMPT', 'SEARCH_CREDENTIAL_REF', 'SEARCH_CREDENTIAL', 'PROVIDER_RESULT_COUNT', 'resultUrl', 'resultTitle', 'resultSnippet', 'resultPageAge', 'RESULT_ORDINALS', 'startSearchServer'], goldens: ['ui'], pace: 15,
      constantBindings: { credentialRef: reference => reference, createServer },
      prepare: async values => { const searchRequests = []; const search = await values.startSearchServer(searchRequests); return { search, values: { searchRequests, searchBaseURL: search.baseURL, WEB_SEARCH_MAX_RESULTS, credentialRef: reference => reference, createServer } }; },
      overlay: (values, prepared) => JSON.stringify([{ id: 'web-search-deepseek', config: { apiKeyEnv: values.SEARCH_CREDENTIAL_REF, baseURL: prepared.search.baseURL } }]) + '\n',
      booted: async (scaffold, _prepared, values) => { await scaffold.ctx.credentials.set(values.SEARCH_CREDENTIAL_REF, values.SEARCH_CREDENTIAL); },
      finish: async prepared => { await new Promise((resolve, reject) => prepared.search.server.close(error => error === undefined ? resolve() : reject(error))); } });
  },
};
// Every selected scenario runs even after an earlier one fails, so one lane run reports the
// complete picture; the run still fails on any failure.
const scenarioFailures = [];
for (const [name, run] of Object.entries(SCENARIOS)) {
  if (!selected(name)) continue;
  try { await run(); } catch (error) { scenarioFailures.push({ scenario: name, error: String(error), stack: error?.stack }); console.error('keyless: scenario ' + name + ' failed: ' + String(error).split('\n')[0]); }
}
let runError;
try {
  assert.deepEqual(scenarioFailures.map(failure => failure.scenario), [], 'scenarios failed');
  assert.equal(checks.length, planned, 'every selected source case must run');
  assert(planned > 0, 'no scenario matched ' + filter);
} catch (error) {
  runError = error;
  await writeFile(join(output, 'failures.json'), JSON.stringify({ error: String(error), failures: scenarioFailures, checks }, null, 2));
}
if (runError) throw runError;
await writeFile(join(output, 'result.json'), JSON.stringify({ checks, modelCalls: 0 }, null, 2));
"#;
