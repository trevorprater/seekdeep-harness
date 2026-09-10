//! Unchanged keyless source browser suites over the Rust/WASM client and a fresh Rust Host each.
//!
//! Every scenario below boots its own isolated Host and Chromium profile exactly as the source
//! `beforeAll` hooks do, then compiles the pinned `it` callbacks with the source scaffold helpers
//! bound to Rust-Host equivalents: session events and listings come from the `/fixture/sessions`
//! route instead of in-process Cordis taps, and goldens compare byte-for-byte in replay mode.

pub(super) const DRIVER: &str = r#"import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawn, execFile, execFileSync } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdirSync, existsSync } from 'node:fs';
import { access, mkdir, mkdtemp, realpath, rm, readFile, writeFile, readdir } from 'node:fs/promises';
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

// vitest `toMatchInlineSnapshot`: pretty-format's default object rendering (sorted keys,
// trailing commas, unescaped strings) against the dedented template literal.
function inlineSnapshot(value, indent = '') {
  const inner = indent + '  ';
  if (Array.isArray(value)) return value.length === 0 ? '[]' : '[\n' + value.map(item => inner + inlineSnapshot(item, inner) + ',\n').join('') + indent + ']';
  if (value !== null && typeof value === 'object') { const keys = Object.keys(value).sort(); return keys.length === 0 ? '{}' : '{\n' + keys.map(key => inner + '"' + key + '": ' + inlineSnapshot(value[key], inner) + ',\n').join('') + indent + '}'; }
  if (typeof value === 'string') return '"' + value + '"';
  return String(value);
}
function dedentSnapshot(text) {
  const lines = text.split('\n');
  if (lines[0].trim() === '') lines.shift();
  if (lines.length && lines[lines.length - 1].trim() === '') lines.pop();
  const width = Math.min(...lines.filter(line => line.trim() !== '').map(line => line.match(/^\s*/)[0].length));
  return lines.map(line => line.slice(width)).join('\n');
}
playwrightExpect.extend({ toMatchInlineSnapshot(received, expected) { const actual = inlineSnapshot(received), wanted = dedentSnapshot(expected); return { pass: actual === wanted, message: () => 'inline snapshot mismatch\n--- expected\n' + wanted + '\n--- actual\n' + actual }; } });
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
    .replaceAll('--dsh-composer-dock-inset', '--seekdeep-composer-dock-inset')
    // The client's persisted selection key carries the product prefix.
    .replaceAll("'dsh.sessions.current'", "'seekdeep.sessions.current'");
}
async function sourceFile(file) {
  const body = await readFile(file.startsWith('apps/') ? join(source, file) : join(TESTS, file), 'utf8');
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
  const seedSession = async (id, text, agentPreset) => { const seeded = await fetch(origin + '/fixture/seed-log/' + encodeURIComponent(id) + (agentPreset === undefined ? '' : '?agentPreset=' + encodeURIComponent(agentPreset)), { method: 'POST', ...text === undefined ? {} : { body: text } }); assert(seeded.ok, 'seed ' + id + ': ' + await seeded.text() + '\n' + stderr); };
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
  const apply = listed => {
    sessions.splice(0, sessions.length, ...listed.map(entry => ({ id: entry.header.id, header: entry.header, events: entry.events, agent: entry.agent ?? undefined })));
    events.splice(0, events.length, ...listed.flatMap(entry => entry.events));
    // Source: `ctx.on('session/event', (session, event) => ...)` observes every appended event
    // once; the listing delivers the events appended since the previous refresh, in order.
    for (const entry of listed) {
      const delivered = seen.get(entry.header.id) ?? 0;
      // Mark the batch delivered before dispatching: a listener may read the listing again
      // synchronously (workflow-run awaits the agent's idle barrier), which must not redeliver.
      seen.set(entry.header.id, entry.events.length);
      for (const event of entry.events.slice(delivered)) for (const listener of listeners) listener({ id: entry.header.id, header: entry.header }, event);
    }
  };
  const refresh = async () => {
    const response = await fetch(origin + '/fixture/sessions'); assert(response.ok, 'fixture session listing HTTP ' + response.status);
    apply(await response.json());
  };
  // Source: `ctx.agents.get` is a synchronous read of the in-process registry; the listing is
  // fetched synchronously so the answer reflects the Host's current state, not the last poll.
  const refreshSync = () => apply(JSON.parse(execFileSync('curl', ['-sS', '--fail', origin + '/fixture/sessions'], { encoding: 'utf8', maxBuffer: 1 << 28 })));
  // The source listener fires on the in-process append; here a background poll keeps the
  // listing fresh while listeners exist, so a case waiting on plain locators still observes it.
  let poller;
  const poll = () => { if (poller === undefined) poller = setInterval(() => { if (!stopped) refresh().catch(() => {}); }, 100); };
  const onEvent = listener => { listeners.push(listener); poll(); };
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
  const stop = () => { stopped = true; if (poller !== undefined) { clearInterval(poller); poller = undefined; } };
  // `ctx.agents.list()` is synchronous in the source; here each call returns the latest listing
  // and kicks a background refresh so a polled predicate observes the Host within its window.
  const agents = { list: () => { if (!stopped) refresh().catch(() => {}); return sessions.map(session => ({ session: { header: session.header, id: session.id } })); } };
  return { events, sessions, refresh, refreshSync, poll, expect: liveExpect, whenTurnSettled, whenTurnsSettled, agents, onEvent, stop };
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
      probes.aria = await Promise.resolve().then(() => page.locator('body').ariaSnapshot({ timeout: 5000 })).then(() => 'ok', failure => 'failed: ' + String(failure).slice(0, 300));
      probes.screenshot = await page.screenshot({ path: join(output, scenario + '-probe.png'), timeout: 5000 }).then(() => 'ok', failure => 'failed: ' + String(failure).slice(0, 300));
      if (process.env.SEEKDEEP_KEYLESS_PROBE) probes.custom = await page.evaluate(process.env.SEEKDEEP_KEYLESS_PROBE).then(value => JSON.stringify(value), failure => 'failed: ' + String(failure));
      await writeFile(join(output, scenario + '-hang-probes.json'), JSON.stringify(probes, null, 2));
      await Promise.all(hooks.map(hook => hook().catch(() => {})));
      await writeFile(join(output, scenario + '-failure-aria.txt'), await Promise.resolve().then(() => page.locator('body').ariaSnapshot()).catch(() => 'unavailable'));
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
// Every opened page's console, by page name, for failure diagnostics of suites whose pages are not the describe's shared page.
const allConsoles = new Map();
async function openPage(name, locale, viewport = { width: 1680, height: 1000 }, timezoneId) {
  const profile = join(world, name, 'browser');
  const context = await chromium.launchPersistentContext(profile, { headless: true, locale, viewport, ...(timezoneId === undefined ? {} : { timezoneId }), args: ['--remote-debugging-port=0'] });
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
  pageConsoles.set(page, console); allConsoles.set(name, console);
  return { context, page, cdp, async screenshot(label) { await exec('agent-browser', ['--session', 'seekdeep-keyless-' + name, '--cdp', cdp, 'screenshot', '--annotate', join(output, name + '-' + label + '.png')]); }, async close() { await exec('agent-browser', ['--session', 'seekdeep-keyless-' + name, 'close']).catch(() => {}); await context.close(); } };
}
const { tsImport } = require('tsx/esm/api');
const { parseSessionLog, deriveReplayScript } = await tsImport(join(source, 'packages/test-support/llm-replay/src/index.ts'), { parentURL: import.meta.url, tsconfig: join(source, 'tsconfig.base.json') });
const sourceModule = path => tsImport(join(source, path), { parentURL: import.meta.url, tsconfig: join(source, 'tsconfig.base.json') });
const sessionModule = await sourceModule('packages/core/session/src/index.ts'), llmModule = await sourceModule('packages/llm/llm/src/index.ts'), llmBrandModule = await sourceModule('packages/llm/llm/src/brand.ts');
// Fixture builders that price or shape content use the source's own pure helpers: the port's
// rendering of that content is what the goldens then compare.
const tokenMeterModule = await sourceModule('packages/llm/token-meter/src/estimate.ts');
// Modules that import the session package by its package name deadlock tsx once the driver holds
// that package by path, so their statements compile against the path-loaded instances instead.
async function compiledSourceModule(path, bindings, exported) {
  const ast = ts.createSourceFile(path, await readFile(join(source, path), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  for (const [name, value] of Object.entries(bindings)) assert.notEqual(value, undefined, path + ' binding ' + name);
  const body = ast.statements.filter(node => !ts.isImportDeclaration(node) && !ts.isModuleDeclaration(node)).map(node => node.getText(ast).replace(/^export\s+/, '')).join('\n');
  return compile(body + '\nreturn {' + exported.join(', ') + '};', bindings, { adapt: false });
}
const chatScrollFixtureModule = await compiledSourceModule('apps/web/tests/chat-scroll-fixture.ts', { CallId: llmModule.CallId, createAssistantMessage: llmModule.createAssistantMessage, createToolResultMessage: llmModule.createToolResultMessage, createUserMessage: llmModule.createUserMessage, SESSION_FORMAT_VERSION: sessionModule.SESSION_FORMAT_VERSION, Session: sessionModule.Session, SessionId: sessionModule.SessionId }, ['createChatScrollFixture']);
const subagentDescriptorModule = await compiledSourceModule('packages/subagent/subagent/src/descriptor.ts', { snapshotJsonValue: sessionModule.snapshotJsonValue }, ['snapshotSubagentDescriptor']);
const realizeSeedFixture = compile(declarations(scaffoldAst, ['realizeSeedFixture']) + '\nreturn realizeSeedFixture;', {});
const seedSessionShim = async (scaffold, text, id, agentPreset) => { await scaffold.server.seedSession(id, text, agentPreset); return id; };
const fixtureHelpers = declarations(scaffoldAst, ['fixtureUserPrompts']);
let fixturePrompt;
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
  // Source listeners observe in-process appends; the listing is polled for the whole Host life.
  live.poll();
  // Source `ctx.llm.registerAdapter(providers, adapter)` keeps the test adapter in-process. Here
  // the adapter object stays in the driver: the Host registers a driver-backed adapter for the
  // providers and streams each `GenerateOptions` to this server, which runs `adapter.stream`
  // and answers one JSON chunk per line; closing the response early aborts the turn signal.
  const adapters = new Map(); let adapterServer; let adapterCount = 0; const pending = [];
  const settle = () => Promise.all(pending);
  const serveAdapter = async (request, response) => {
    let finished = false;
    try {
      trace('adapter ' + request.url); const adapter = adapters.get(request.url.split('/').at(-1));
      if (adapter === undefined || request.method !== 'POST') { response.writeHead(404); response.end(); return; }
      let body = ''; for await (const chunk of request) body += chunk;
      const options = JSON.parse(body);
      const controller = new AbortController(); options.signal = controller.signal;
      response.on('close', () => { if (!finished) controller.abort(new Error('driver adapter: the turn was aborted')); });
      response.socket?.setNoDelay(true);
      response.writeHead(200, { 'content-type': 'application/x-ndjson' });
      try { for await (const chunk of adapter.stream(options)) { trace('adapter chunk ' + chunk.type); response.write(JSON.stringify(chunk) + '\n'); } }
      catch (error) { response.write(JSON.stringify({ error: String(error?.message ?? error) }) + '\n'); }
    } catch (error) {
      console.error('keyless: adapter request failed: ' + String(error));
      if (!response.headersSent) response.writeHead(500);
    } finally { finished = true; response.end(); }
  };
  const ensureAdapterServer = async () => {
    if (adapterServer !== undefined) return adapterServer;
    const httpServer = createServer((request, response) => { void serveAdapter(request, response); });
    await new Promise(resolve => httpServer.listen(0, '127.0.0.1', resolve));
    httpServer.unref();
    adapterServer = { origin: 'http://127.0.0.1:' + httpServer.address().port, close: () => new Promise(resolve => httpServer.close(() => resolve())) };
    const stop = server.stop.bind(server);
    server.stop = async () => { await adapterServer.close(); return stop(); };
    return adapterServer;
  };
  const trace = process.env.SEEKDEEP_KEYLESS_TRACE_POSTS ? message => console.error('keyless-post: ' + message) : () => {};
  const post = async (path, body) => { trace(path + ' ' + JSON.stringify(body).slice(0, 160)); const response = await fetch(server.origin + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }); const text = await response.text(); assert(response.ok, path + ' HTTP ' + response.status + ': ' + text); trace(path + ' -> ' + text.slice(0, 160)); return JSON.parse(text); };
  const agentOf = id => ({ sessionId: id, id, get status() { return live.sessions.find(session => session.id === id)?.agent?.status; }, get inbox() { return live.sessions.find(session => session.id === id)?.agent?.inbox ?? { nextTurn: [] }; }, cancel: cause => { pending.push(post('/fixture/agent/' + encodeURIComponent(id) + '/cancel', { cause })); }, followup: message => { pending.push(post('/fixture/agent/' + encodeURIComponent(id) + '/followup', { message })); }, session: { id, get header() { return live.sessions.find(session => session.id === id)?.header; }, get events() { return live.sessions.find(session => session.id === id)?.events ?? []; }, append: (type, data, options = {}) => { pending.push(post('/fixture/session/' + encodeURIComponent(id) + '/append', { type, data, ...options }).then(() => live.refresh())); }, requestHeader: () => live.sessions.find(session => session.id === id)?.events.filter(event => event.type === 'request/header').at(-1)?.data.header }, whenIdle: async () => { await settle(); const settled = await fetch(server.origin + '/fixture/idle/' + encodeURIComponent(id), { method: 'POST' }); assert(settled.ok, 'idle barrier HTTP ' + settled.status + ': ' + await settled.text()); await live.refresh(); } });
  return {
    // Source `agents.get` answers only for live Agents: a Session that only rests in the store
    // (a cold child, a persisted seed nobody opened) has none.
    agents: { get: id => { live.refreshSync(); return live.sessions.some(session => session.id === id && session.agent !== undefined) ? agentOf(id) : undefined; }, list: () => live.agents.list().filter(entry => live.sessions.find(session => session.id === entry.session.id)?.agent !== undefined).map(entry => agentOf(entry.session.id)),
      roots: () => { live.refreshSync(); return JSON.parse(execFileSync('curl', ['-sS', '--fail', server.origin + '/fixture/agents/roots'], { encoding: 'utf8' })).map(agentOf); },
      create: async ({ sessionId, meta, agentOptions, setup }) => { await settle(); const created = await post('/fixture/agent/create', { sessionId, meta: meta ?? {}, agentOptions: agentOptions ?? {}, ...(setup === undefined ? {} : { setup: 'agentPresets' }) }); live.refreshSync(); const id = created.sessionId; return { agent: agentOf(id), dispose: () => post('/fixture/agent/' + encodeURIComponent(id) + '/dispose', {}).then(() => undefined) }; } },
    llm: { registerAdapter: (providers, adapter) => { const id = 'adapter-' + (++adapterCount); adapters.set(id, adapter); const registration = (async () => { const endpoint = (await ensureAdapterServer()).origin + '/adapter/' + id; return (await post('/fixture/adapter/register', { providers, endpoint })).id; })(); pending.push(registration); return () => registration.then(hostId => post('/fixture/adapter/unregister', { id: hostId })).then(() => { adapters.delete(id); }); } },
    subagents: {
      startContinuable: async ({ provider, label, request }) => { await settle(); return post('/fixture/subagent/start', { provider, label, parentSessionId: request.parent.id, prompt: request.prompt, ...(request.agentOptions === undefined ? {} : { agentOptions: request.agentOptions }), ...(request.maxDepth === undefined ? {} : { maxDepth: request.maxDepth }), ...(request.persona === undefined ? {} : { persona: request.persona }) }); },
      listChildren: async parentId => { const response = await fetch(server.origin + '/fixture/subagent/children/' + encodeURIComponent(parentId)); const text = await response.text(); assert(response.ok, 'listChildren HTTP ' + response.status + ': ' + text); return JSON.parse(text); },
      followup: async (parent, childId, content, options = {}) => post('/fixture/subagent/followup', { parentSessionId: parent.id, childSessionId: childId, content, ...(options.source === undefined ? {} : { source: options.source }) }).then(result => result.messageId),
    },
    systemPrompt: { section: section => { const registration = post('/fixture/prompt/section', section); pending.push(registration); return () => { pending.push(registration.then(({ id }) => post('/fixture/prompt/section/' + id + '/dispose', {}))); }; } },
    agentPresets: { serviceFor: (agent, name) => { const value = JSON.parse(execFileSync('curl', ['-sS', '--fail', '-X', 'POST', '-H', 'content-type: application/json', '-d', JSON.stringify({ sessionId: agent.sessionId, name }), server.origin + '/fixture/preset/service'], { encoding: 'utf8' })); return value === null ? undefined : value; } },
    // Source `ctx.effect(register, label)` runs the registration now and keeps its disposer.
    effect: (register, _label) => register(),
    workspaceRegistry: { resolveByPath: async path => { await settle(); const found = await post('/fixture/workspace/resolve', { path }); return found === null ? undefined : { id: found.id, attachSession: sessionId => post('/fixture/workspace/attach', { path, sessionId }).then(() => undefined) }; } },
    sessions: { flush: async session => { if (session === undefined) { await live.refresh(); return true; } const flushed = await post('/fixture/flush/' + encodeURIComponent(session.id), {}); await live.refresh(); return flushed; }, list: () => live.sessions.map(session => ({ id: session.id, header: session.header, append: (type, data) => post('/fixture/session/' + encodeURIComponent(session.id) + '/append', { type, data }).catch(error => console.error('keyless: session append failed: ' + String(error))) })) },
    on: (event, listener) => { assert.equal(event, 'session/event', 'only session/event listeners are shimmed'); live.onEvent(listener); return () => {}; },
    tools: { schemas: agent => JSON.parse(execFileSync('curl', ['-sS', '--fail', server.origin + '/fixture/preset/tool-schemas/' + encodeURIComponent(agent.sessionId)], { encoding: 'utf8', maxBuffer: 1 << 26 })), execute: async ({ callId, name, arguments: args, agent }) => { await settle(); const result = await post('/fixture/tool/execute', { sessionId: agent.sessionId, callId, name, arguments: args }); if (result.isError) console.error('keyless: tool ' + name + ' failed: ' + JSON.stringify(result).slice(0, 800)); return result; } },
    jobs: { kill: (jobId, agent, reason) => post('/fixture/job/kill', { jobId, sessionId: agent.sessionId, reason }) },
    credentials: { set: (ref, value) => post('/fixture/credential', { ref, value }) },
    // Source: `ctx.sessionPersistence.create(header)` then `append(id, events)`; one complete log
    // reaches the Host persist route on the first append (the seeds here append exactly once).
    sessionPersistence: { load: async id => { const response = await fetch(server.origin + '/fixture/persist/' + encodeURIComponent(id) + '/load'); const text = await response.text(); assert(response.ok, 'persistence load HTTP ' + response.status + ': ' + text); return JSON.parse(text); }, create: async header => { pendingPersist.set(header.id, { header, events: [] }); }, append: async (id, events) => { const entry = pendingPersist.get(id); assert(entry, 'persist append before create: ' + id); entry.events.push(...events); const text = [JSON.stringify({ type: 'session', ...entry.header }), ...entry.events.map(event => JSON.stringify(event)), ''].join('\n'); const response = await fetch(server.origin + '/fixture/persist/' + encodeURIComponent(id), { method: 'POST', body: text }); assert(response.ok, 'persist ' + id + ': ' + await response.text()); } },
    sessionProjectionCache: { coldSnapshot: async id => { await post('/fixture/cold-snapshot/' + encodeURIComponent(id), {}); } },
    get: name => { assert.equal(name, 'tokenMeter', 'only the token meter is shimmed through ctx.get'); return { estimateMessage: message => tokenMeterModule.estimateMessage(message) }; },
  };
}
const pendingPersist = new Map();
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
  const server = await bootHost(name, { welcome, replay: FIXTURE, replayOverride: prepared.replayOverride, replayChildFixtures: prepared.replayChildFixtures, overlay, env: { ...options.pace === undefined ? {} : { SEEKDEEP_KEYLESS_REPLAY_PACE_MS: String(options.pace) }, ...options.env ?? {} } });
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
    // Source `agentPresets: { roots, default }` becomes the overlay row the Host composes from.
    let overlay = options.overlay;
    // Source composes `extraOverlayPath` rows after the base and surface patches; the
    // scenario overlay is that same YAML row list, so the extra rows append to it.
    if (scaffoldOptions.extraOverlayPath !== undefined) {
      const extra = (await readFile(scaffoldOptions.extraOverlayPath, 'utf8')).replace(/^(#.*\n)+/, '');
      assert(overlay === undefined || !overlay.trimStart().startsWith('['), 'extraOverlayPath needs a YAML scenario overlay');
      overlay = (overlay ?? '') + '\n' + extra + '\n';
    }
    if (scaffoldOptions.agentPresets !== undefined) {
      assert(overlay === undefined || !overlay.trimStart().startsWith('['), 'agentPresets needs a YAML scenario overlay');
      overlay = (overlay ?? '') + '\n- id: agent-presets\n  config: ' + JSON.stringify({ ...scaffoldOptions.agentPresets, includeUserRoot: false }) + '\n';
    }
    const server = await bootHost(name + '-' + hosts, { welcome, welcomePending: scaffoldOptions.welcomeNoticePending === true, replay: scaffoldOptions.replayFixture, replayOverride: scaffoldOptions.replayOverride, replayChildFixtures: scaffoldOptions.replayChildFixtures, overlay, env: { ...scaffoldOptions.paceMs === undefined ? {} : { SEEKDEEP_KEYLESS_REPLAY_PACE_MS: String(scaffoldOptions.paceMs) }, ...scaffoldOptions.replayContextWindow === undefined ? {} : { SEEKDEEP_KEYLESS_REPLAY_CONTEXT_WINDOW: String(scaffoldOptions.replayContextWindow) }, ...options.env ?? {} } });
    const live = liveSessions(server.origin);
    const scaffold = { mode: MODE, baseUrl: server.origin, workspaceCwd: server.workspace, whenTurnSettled: live.whenTurnSettled, whenTurnsSettled: live.whenTurnsSettled, ctx: hostContext(server, live), live, server, async close() { openHosts.delete(scaffold); await live.refresh().catch(() => {}); live.stop(); await writeFile(join(output, name + '-' + hosts + '-sessions.json'), JSON.stringify(live.sessions, null, 2)); await server.stop(); } };
    openHosts.add(scaffold);
    return scaffold;
  };
  const chromium = { launch: async () => {
    const pages = [];
    const browser = { async newPage(pageOptions = {}) { contexts += 1; const opened = await openPage(name + '-' + contexts, pageOptions.locale ?? 'en-US', pageOptions.viewport, pageOptions.timezoneId); pages.push(opened); return opened.page; }, async close() { openBrowsers.delete(browser); for (const opened of pages.splice(0)) await opened.close(); } };
    openBrowsers.add(browser);
    return browser;
  } };
  const newEnglishPage = (browser, height = 1000) => browser.newPage({ viewport: { width: 1680, height }, locale: 'en-US' });
  // Whatever a failed case left open is closed after it, like the source `afterEach`.
  const closeAll = async () => { const failures = []; for (const browser of [...openBrowsers]) await browser.close().catch(error => failures.push(error)); for (const scaffold of [...openHosts]) await scaffold.close().catch(error => failures.push(error)); if (failures.length) throw failures.length === 1 ? failures[0] : new AggregateError(failures, name + ' teardown failed'); };
  return { hosts: openHosts, launchWebScaffold, chromium, newEnglishPage, closeAll };
}
async function perCaseScenario(name, options) {
  const ast = await sourceFile(options.file ?? name + '.e2e.ts'), cases = sourceCases(ast); assert.equal(cases.length, options.cases, 'source case inventory');
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
// Suites whose `describe` blocks boot their own scaffold in `beforeAll`: each describe runs in
// order with its own shared scope (describe-level declarations hoisted), its `beforeAll`, its
// cases, and its `afterAll`, all through the scaffold shims.
function describeBlocks(ast) {
  const blocks = [];
  function visit(node) {
    if (ts.isCallExpression(node) && node.arguments.length >= 2 && ts.isArrowFunction(node.arguments[1])) {
      const callee = node.expression.getText(ast);
      if (callee === 'describe' || callee.startsWith('describe.')) {
        const body = node.arguments[1].body;
        const block = { name: node.arguments[0].getText(ast), variables: [], functions: [], beforeAll: undefined, afterAll: undefined, cases: [], skipped: callee.includes("skipIf(MODE !== 'record')") };
        for (const statement of ts.isBlock(body) ? body.statements : []) {
          if (ts.isVariableStatement(statement)) {
            for (const declaration of statement.declarationList.declarations) block.variables.push({ name: declaration.name.getText(ast), initializer: declaration.initializer ? declaration.initializer.getText(ast) : undefined });
          } else if (ts.isFunctionDeclaration(statement)) {
            block.functions.push(statement.getText(ast));
          } else if (ts.isExpressionStatement(statement) && ts.isCallExpression(statement.expression)) {
            const hook = statement.expression.expression.getText(ast), callback = statement.expression.arguments[0];
            if (hook === 'beforeAll' && callback && ts.isArrowFunction(callback)) block.beforeAll = callback.getText(ast);
            if (hook === 'afterAll' && callback && ts.isArrowFunction(callback)) block.afterAll = callback.getText(ast);
          }
        }
        const cases = [];
        (function collect(inner) { if (ts.isCallExpression(inner) && ts.isStringLiteral(inner.arguments[0] ?? ts.factory.createNull()) && inner.arguments[1] && ts.isArrowFunction(inner.arguments[1])) { const callee = inner.expression.getText(ast); if (callee === 'it' || callee.startsWith('it.')) cases.push({ name: inner.arguments[0].text, callback: inner.arguments[1].getText(ast), skipped: callee.includes("skipIf(MODE !== 'record')") }); } ts.forEachChild(inner, collect); })(body);
        block.cases = cases;
        blocks.push(block);
        return;
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  return blocks;
}
async function describeScenario(name, options) {
  const ast = await sourceFile(options.file ?? name + '.e2e.ts'), blocks = describeBlocks(ast);
  assert.equal(blocks.length, options.describes, 'source describe inventory');
  assert.equal(blocks.reduce((total, block) => total + block.cases.length, 0), options.cases, 'source case inventory');
  const constants = declarations(ast, options.constants), support = declarations(supportAst, ['connectFreshWorkspace', 'ZH_BROWSER_LOCALE', 'connectFreshWorkspaceZh']);
  const dir = join(TESTS, 'snapshots', name);
  const goldens = Object.fromEntries((options.goldens ?? []).map(golden => Array.isArray(golden) ? [golden[0], join(dir, golden[1] + '.expected.md')] : [golden.toUpperCase().replaceAll('-', '_') + '_EXPECTED', join(dir, golden + '.expected.md')]));
  const shims = scaffoldShims(name, options);
  const api = compile(helpers + '\nreturn {watchConsole};', { expect });
  for (const block of blocks) {
    if (block.skipped) { console.log('keyless: ' + name + ' skips record-only describe ' + block.name); continue; }
    const shared = {};
    const prelude = helpers + '\n' + fixtureHelpers + '\n' + support + '\n' + constants + '\n' + block.functions.join('\n');
    const values = { expect, MODE, SNAPSHOT_DIR: dir, ...goldens, readFile, writeFile, mkdtemp, rm, tmpdir, join, existsSync, mkdirSync, parseSessionLog, deriveReplayScript, watchConsole: api.watchConsole, recordFixture: () => { throw new Error('record mode is not a replay lane'); }, ...shims, ...goldenTools(name), ...options.values ?? {} };
    // Describe-level declarations become shared scope entries: `let x` stays undefined; a
    // `const x = init` evaluates once per describe against the same prelude.
    const initializers = block.variables.filter(variable => variable.initializer !== undefined);
    for (const variable of block.variables) shared[variable.name] = undefined;
    // Module-level `let` state assigned from hooks (a suite-wide browser) lives in the same scope.
    for (const name of options.moduleLets ?? []) shared[name] = undefined;
    if (initializers.length) Object.assign(shared, compile(prelude + '\nreturn {' + initializers.map(variable => variable.name + ': (' + variable.initializer + ')').join(', ') + '};', values));
    let caseError;
    const tripwireProxy = new Proxy({}, { get: (_, key) => (shared.tripwire ?? { warnings: [], pageErrors: [] })[key] });
    const pageShim = { __console: () => pageConsoles.get(shared.page) ?? [], isClosed: () => (shared.page ? shared.page.isClosed() : true), evaluate: (...args) => shared.page ? shared.page.evaluate(...args) : Promise.reject(new Error('no page')), locator: (...args) => shared.page.locator(...args), screenshot: (...args) => shared.page ? shared.page.screenshot(...args) : Promise.resolve() };
    try {
      if (block.beforeAll) await compile(prelude + '\nreturn (' + block.beforeAll + ');', values, { shared })();
      await runCases(name, block.cases, pageShim, { prelude, shared, values }, tripwireProxy);
    } catch (error) {
      // Diagnostics before afterAll disposes the agents: every open Host's live session listing.
      let index = 0;
      for (const open of shims.hosts) { index += 1; await open.live.refresh().catch(() => {}); await writeFile(join(output, name + '-failure-' + index + '-sessions.json'), JSON.stringify(open.live.sessions, null, 2)).catch(() => {}); }
      for (const [pageName, entries] of allConsoles) await writeFile(join(output, name + '-failure-console-' + pageName.replaceAll('/', '_') + '.json'), JSON.stringify(entries.slice(-400), null, 1)).catch(() => {});
      caseError = error;
      throw error;
    } finally {
      const failures = [];
      if (block.afterAll) await compile(prelude + '\nreturn (' + block.afterAll + ');', values, { shared })().catch(error => failures.push(error));
      await shims.closeAll().catch(error => failures.push(error));
      // The case's own failure stays the reported one; teardown failures are logged beside it.
      if (failures.length && caseError !== undefined) for (const failure of failures) console.error('keyless: ' + name + ' teardown after a failed describe: ' + String(failure).split('\n')[0]);
      else if (failures.length) throw failures.length === 1 ? failures[0] : new AggregateError(failures, name + ' describe teardown failed');
    }
  }
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
  async 'workflow-run'() {
    // The recorded parent/child model fixtures live at the same repository-relative path in the
    // port; the real workflow tool, worker, subagent provider, and navigation run during replay.
    const parent = join(process.cwd(), 'examples/acp-agent/tests/snapshots/workflow-run/session.jsonl');
    await replayScenario('workflow-run', { cases: 3, constants: ['CHILD_PROMPT'], goldens: ['ui'], pace: 25,
      prepare: async () => ({ fixture: (fixturePrompt = fixtureUserPrompts(await readFile(parent, 'utf8'))[0], parent), replayChildFixtures: [join(process.cwd(), 'examples/acp-agent/tests/snapshots/workflow-run/session.1.jsonl')] }),
      prelude: "const PARENT_FIXTURE = join(REPO_ROOT, 'examples/acp-agent/tests/snapshots/workflow-run/session.jsonl');\nconst CHILD_FIXTURE = join(REPO_ROOT, 'examples/acp-agent/tests/snapshots/workflow-run/session.1.jsonl');\nlet prompt = promptRef.value;\n" + nestedDeclarations(await sourceFile('workflow-run.e2e.ts'), ['waitForParentSettlement']),
      values: async => ({ REPO_ROOT: process.cwd(), promptRef: { get value() { return fixturePrompt; } } }) });
  },
  async 'agent-preset-selection'() {
    // The keyless Host mounts the shipped roster with `standard` as the default, the source's
    // `agentPresets` option for this lane; the seeded minimal session and its subagent child are
    // persisted through the seed and persist fixture routes.
    const dir = join(TESTS, 'snapshots', 'agent-preset-selection');
    await describeScenario('agent-preset-selection', { describes: 1, cases: 6, constants: ['SEED_ID', 'SKILL_NAME', 'seedWorkspaceSkill', 'menuOptions', 'seedLog', 'seedSubagent', 'livePreset'],
      values: { SNAPSHOT_DIR: dir, HERO_EXPECTED: join(dir, 'hero.expected.md'), MENU_EXPECTED: join(dir, 'menu.expected.md'), HEADER_EXPECTED: join(dir, 'header.expected.md'), SHIPPED_PRESETS: join(process.cwd(), 'apps/cli/config/agent-presets'), sessionId: id => id, SESSION_FORMAT_VERSION: sessionModule.SESSION_FORMAT_VERSION, snapshotSubagentDescriptor: subagentDescriptorModule.snapshotSubagentDescriptor, mkdir, seedSession: seedSessionShim } });
  },
  async 'chat-long-interactions'() {
    await describeScenario('chat-long-interactions', { describes: 1, cases: 1, constants: ['SESSION_ID', 'FIXTURE_TURNS', 'TOOL_TURN', 'BRANCH_TURN', 'TARGET_CALL_1', 'TARGET_CALL_2', 'CONTINUE_PROMPT', 'CONTINUE_FIRST', 'CONTINUE_DONE', 'FIXTURE', 'continuationChunks', 'replayEntry', 'carries', 'textContent', 'nextPaint', 'openSeed', 'wheelUntilMounted', 'requiredEvent', 'messageKey', 'assistantKey', 'turnTailKey'],
      values: { createChatScrollFixture: chatScrollFixtureModule.createChatScrollFixture, conversationContextKey: compile(declarations(supportAst, ['conversationContextKey']) + '\nreturn conversationContextKey;', {}), SessionId: id => id, seedSession: seedSessionShim } });
  },
  async 'trajectory-virtualization'() {
    const dir = join(TESTS, 'snapshots', 'trajectory-virtualization');
    await describeScenario('trajectory-virtualization', { describes: 1, cases: 1, constants: ['SESSION_ID', 'FIXTURE', 'MAX_MOUNTED_ROWS', 'GEOMETRY_TOLERANCE', 'STREAM_MARKER', 'STREAM_TEXT', 'STREAM_CHUNKS', 'openSeed', 'openTrajectory', 'logicalRows', 'mountedRows', 'geometry', 'nextPaint', 'scrollToRatio', 'firstVisibleRow', 'rowTop', 'loadToFirstTurn'], goldens: ['load-more'],
      values: { SNAPSHOT_DIR: dir, createChatScrollFixture: chatScrollFixtureModule.createChatScrollFixture, seedSession: seedSessionShim } });
  },
  async 'chat-continuous-conversation'() {
    await describeScenario('chat-continuous-conversation', { describes: 1, cases: 1, constants: ['TURN_COUNT', 'TOOL_TURNS', 'STREAM_PACE_MS', 'suffix', 'longFinalPrompt', 'turnSpec', 'textStream', 'toolStream', 'replayScript', 'userText', 'assistantText', 'toolResultText', 'messageKey', 'assistantKey'],
      values: { CallId: llmModule.CallId, conversationContextKey: compile(declarations(supportAst, ['conversationContextKey']) + '\nreturn conversationContextKey;', {}), SessionId: id => id } });
  },
  async 'chat-scroll-contract'() {
    // Each case launches its own scroll world (Host + page) through the scaffold shims and
    // closes it; the suite-wide Chromium is module-level `let` state assigned from beforeAll.
    await describeScenario('chat-scroll-contract', { describes: 1, cases: 5, moduleLets: ['browser'],
      constants: ['HISTORY_SESSION_ID', 'TOOL_SESSION_ID', 'RESTORE_SESSION_A_ID', 'RESTORE_SESSION_B_ID', 'REPLAY_CONTEXT_WINDOW', 'STREAM_PACE_MS', 'GEOMETRY_TOLERANCE', 'LIVE_TEXT_PROMPT', 'LIVE_TEXT_FIRST', 'LIVE_TEXT_DONE', 'LIVE_TOOL_PROMPT', 'LIVE_TOOL_CALL_ID', 'LIVE_TOOL_RESULT', 'LIVE_TOOL_FIRST', 'LIVE_TOOL_DONE', 'TOOL_READY_FILE', 'TOOL_RELEASE_FILE', 'INPUTS_SESSION_ID', 'FLING_SESSION_ID', 'LIVE_FLING_PROMPT', 'LIVE_FLING_FIRST', 'LIVE_FLING_DONE', 'HISTORY_FIXTURE', 'TOOL_FIXTURE', 'RESTORE_FIXTURE_A', 'RESTORE_FIXTURE_B', 'INPUTS_FIXTURE', 'textStream', 'toolStream', 'replayEntry', 'launchScrollWorld', 'closeScrollWorld', 'withScrollWorld', 'nextPaint', 'scrollGeometry', 'loadedFlowRows', 'openSeed', 'wheelTranscript', 'flingTranscript', 'wheelToHistoryStart', 'wheelUntilMounted', 'wheelUntilVisible', 'visibleFlowAnchor', 'flowTop', 'expectSameFlowTop', 'expectBottom', 'expectMarkerAboveComposer', 'loadEarlierWithAnchor', 'fileExists', 'eventCarries', 'assertClean'],
      values: { createChatScrollFixture: chatScrollFixtureModule.createChatScrollFixture, CallId: llmModule.CallId, seedSession: seedSessionShim, access } });
  },
  async 'schedule-after'() {
    // The source registers three in-process test adapters on the scaffold context; each lives in
    // the driver behind the Host's driver-adapter fixture route. The Schedule overlay is the port's
    // own example composition, the source's `examples/web-schedule/cordis.yml`; the suite itself
    // mounts it through `extraOverlayPath`, so the scenario passes only its path.
    const overlayPath = join(process.cwd(), 'examples/web-schedule/cordis.yml');
    const scheduleDomain = await compiledSourceModule('packages/schedule/schedule/src/domain.ts', {}, ['createEveryScheduleRecord', 'foldScheduleEvents', 'resolveEveryOccurrence']);
    await describeScenario('schedule-after', { describes: 1, cases: 4, goldens: [['AFTER_EXPECTED', 'conversation'], ['AT_EXPECTED', 'at-conversation'], ['EVERY_EXPECTED', 'every-conversation']],
      constants: ['AFTER_PROVIDER', 'AT_PROVIDER', 'EVERY_PROVIDER', 'MODEL', 'AFTER_PROMPT', 'AFTER_REPLY', 'AT_BROWSER_ZONE', 'AT_USER_PROMPT', 'AT_PROMPT', 'AT_READY', 'AT_ACK', 'AT_REPLY', 'EVERY_PROMPTS', 'EVERY_REPLY', 'EVERY_INTERVAL_SECONDS', 'EVERY_FIXTURE_AGE_MS', 'textResponse', 'ReminderAdapter', 'EveryReminderAdapter', 'localAt', 'BrowserZoneAtAdapter', 'assistantText', 'requestText', 'expectReminderFraming', 'waitForReply', 'assistantKey'],
      values: { OVERLAY: overlayPath, LlmAdapter: llmModule.LlmAdapter, CallId: llmModule.CallId, createUserMessage: llmModule.createUserMessage, SessionId: id => id, ScheduleId: id => id, ...scheduleDomain, conversationContextKey: compile(declarations(supportAst, ['conversationContextKey']) + '\nreturn conversationContextKey;', {}) } });
  },
  async 'sidebar-subagent-activity'() {
    // The staged adapter lives in the driver; the Host's held child runs its model call through
    // the driver-adapter bridge until the case releases it.
    await describeScenario('sidebar-subagent-activity', { describes: 1, cases: 1, goldens: [['RUNNING_OWNER_EXPECTED', 'owner-running']],
      constants: ['HOLD_PROVIDER', 'HOLD_MODEL', 'StagedAdapter', 'waitForRunningChild'],
      values: { LlmAdapter: llmModule.LlmAdapter, createUserMessage: llmModule.createUserMessage, SessionId: id => id, mkdir } });
  },
  async 'subagent-interrupt'() {
    // No browser: the source drives the real HTTP carrier (`/api/...`) of the Host directly.
    await describeScenario('subagent-interrupt', { describes: 1, cases: 1,
      constants: ['INITIAL', 'FOLLOWUP', 'WAKING', 'rpc', 'waitFor', 'textCompletion'],
      values: { sessionId: id => id, SessionId: id => id } });
  },
  async 'subagent-interrupt-ui'() {
    // Its golden lives beside the subagent-interrupt suite's snapshots, not under its own name.
    await describeScenario('subagent-interrupt-ui', { describes: 1, cases: 3,
      constants: ['LABEL', 'INITIAL', 'REARM', 'REARM_WAKE', 'FOLLOWUP', 'WAKING', 'REARMED_ANSWER', 'PARKED_ANSWER', 'WAKING_ANSWER', 'waitFor', 'waitForAbortedTurn', 'textCompletion'],
      values: { BASE_FIXTURE: join(TESTS, 'snapshots/live-interactions/session.jsonl'), SNAPSHOT_DIR: join(TESTS, 'snapshots/subagent-interrupt'), OFFLINE_COMPOSER_EXPECTED: join(TESTS, 'snapshots/subagent-interrupt/offline-composer.expected.md'), SessionId: id => id } });
  },
  async 'subagent-conversation'() {
    const dir = join(TESTS, 'snapshots', 'subagent-conversation');
    await describeScenario('subagent-conversation', { describes: 1, cases: 9,
      goldens: [['AVAILABLE_CHILD_EXPECTED', 'ui'], ['TREE_EXPECTED', 'tree'], ['BRANCHLESS_EXPECTED', 'branchless'], ['STALE_CATALOG_EXPECTED', 'stale-catalog'], ['SIDEBAR_EXPECTED', 'sidebar'], ['UNAVAILABLE_GRANDCHILD_EXPECTED', 'nested'], ['FORK_EXPECTED', 'fork']],
      constants: ['LABEL', 'ONE_SHOT_LABEL', 'NESTED_LABEL', 'PARENT_PROMPT', 'INITIAL_PROMPT', 'NESTED_PROMPT', 'FOLLOWUP', 'POST_FORK_FOLLOWUP', 'childFixture', 'waitForAgentToSettle'],
      values: { BASE_FIXTURE: join(TESTS, 'snapshots/live-interactions/session.jsonl'), SNAPSHOT_DIR: dir, SESSION_FORMAT_VERSION: sessionModule.SESSION_FORMAT_VERSION, sessionId: id => id, SessionId: id => id, snapshotSubagentDescriptor: subagentDescriptorModule.snapshotSubagentDescriptor } });
  },
  async 'reasoning-chunks-stress'() {
    // Source: the opt-in browser stress lane (no default vitest config includes it). The page
    // boots on `?fixture`; the storm rides the Rust fixture's timing hooks on `window.__fxTiming`.
    await perCaseScenario('reasoning-chunks-stress', { file: 'apps/web/stress-tests/reasoning-chunks.stress.ts', cases: 1,
      constants: ['CHUNK_COUNT', 'CHUNKS_PER_INTERVAL', 'CHUNK_INTERVAL_MS', 'MAIN_THREAD_DELAY_BUDGET_MS'] });
  },
  async 'complex-history-perf'() {
    // Source: the manual performance lane; measurements are reported, cardinality is asserted.
    await describeScenario('complex-history-perf', { file: 'complex-history.perf.ts', describes: 1, cases: 4,
      constants: ['SIDEBAR_SESSION_COUNT', 'LONG_SESSION_ID', 'LONG_SESSION_TITLE', 'LONG_HISTORY_TURNS', 'TOOL_TURN_INTERVAL', 'TOOLS_PER_TOOL_TURN', 'EXPECTED_TOOL_CALLS', 'EXPECTED_TRAJECTORY_ROWS', 'DEFAULT_HISTORY_TURNS', 'PERF_REPLAY_CONTEXT_WINDOW', 'STREAM_PACE_MS', 'STREAM_DELTA_COUNT', 'COMPARISON_TURNS', 'COMPARISON_DELTA_COUNT', 'COMPARISON_TOOL_INTERVAL', 'SOAK_TURNS', 'POST_SOAK_RENDER_TURN', 'SOAK_DELTA_COUNT', 'SOAK_TOOL_INTERVAL', 'SOAK_CHECKPOINT_INTERVAL', 'LONG_CONTINUATION_USER_PREFIX', 'LONG_CONTINUATION_FIRST_PREFIX', 'LONG_CONTINUATION_DONE_PREFIX', 'SOAK_USER_PREFIX', 'SOAK_FIRST_PREFIX', 'SOAK_DONE_PREFIX', 'LIVE_PROMPT_MARKER', 'STREAM_FIRST_MARKER', 'STREAM_DONE_MARKER', 'LIVE_PROMPT', 'STREAM_DELTAS', 'text', 'appendTitle', 'appendRequestHeader', 'appendAssistant', 'appendToolStep', 'fencedCode', 'fixtureLog', 'smallSidebarFixture', 'longHistoryFixture', 'textStream', 'comparisonPrompt', 'comparisonDeltas', 'comparisonTurn', 'soakTurn', 'toolStream', 'performanceReplayOverride', 'rounded', 'chromiumMetrics', 'retainedBrowserState', 'requiredMetric', 'metricDelta', 'measure', 'startMutationProbe', 'stopMutationProbe', 'startUserRenderProbe', 'triggerUserRenderProbe', 'stopUserRenderProbe', 'stableCount', 'conversationTurns', 'retainedDelta', 'launchPerformanceWorld', 'closePerformanceWorld', 'openPerformancePage', 'openLongHistory', 'continueConversation', 'measurePostSoakUserRender', 'average', 'p95', 'summarizeTurnWindows'],
      values: { ...seedModule(), seedSession: seedSessionShim, performance: globalThis.performance, webSnapshotMode: () => MODE } });
  },
  async 'reasoning-storm-profile'() {
    // Port diagnostic (opt-in): CPU-profile a short reasoning storm through the fixture transport
    // and write the profile plus the emission rate beside the lane outputs.
    const { launchWebScaffold, chromium, newEnglishPage, closeAll } = scaffoldShims('reasoning-storm-profile', {});
    try {
      const scaffold = await launchWebScaffold();
      const browser = await chromium.launch();
      const page = await newEnglishPage(browser);
      await page.addInitScript(() => { localStorage.setItem('seekdeep.sessions.current', JSON.stringify({ sessionId: 'fx-alpha' })); });
      await page.goto(scaffold.baseUrl + '?fixture', { waitUntil: 'load' });
      await page.waitForSelector('[class*="frame"]', { timeout: 30000 });
      await page.addStyleTag({ content: '[class*="onboardingOverlay"] { display: none !important; }' });
      await page.locator('[data-sample="bash"]').first().waitFor({ timeout: 30000 });
      const consoleLog = [];
      page.on('console', message => { if (message.type() === 'error' || message.type() === 'warning') consoleLog.push([Date.now() - started, message.type(), message.text().slice(0, 300)]); });
      page.on('pageerror', error => consoleLog.push([Date.now() - started, 'pageerror', String(error.stack ?? error).slice(0, 1200)]));
      const cdp = await page.context().newCDPSession(page);
      await cdp.send('Profiler.enable'); await cdp.send('Profiler.setSamplingInterval', { interval: 500 }); await cdp.send('Profiler.start');
      const started = Date.now();
      const chunkCount = Number(process.env.SEEKDEEP_STORM_CHUNKS ?? '3000');
      await page.evaluate(count => window.__fxTiming.startReasoningChunkStorm('fx-alpha', count, 128, 16), chunkCount);
      const samples = [];
      for (let i = 0; i < 2400; i++) {
        await new Promise(resolve => setTimeout(resolve, 500));
        const [emitted, thinkRows] = await page.evaluate(() => [window.__fxTiming.reasoningChunkStormState()?.emitted ?? 0, [...document.querySelectorAll('[data-variant="think"]')].map(row => row.getAttribute('data-state')).join('/')]);
        samples.push([Date.now() - started, emitted, thinkRows]);
        if (emitted >= chunkCount) break;
      }
      const { profile } = await cdp.send('Profiler.stop');
      const settleStarted = Date.now();
      const inspection = await page.evaluate(() => ({ think: [...document.querySelectorAll('[data-variant="think"]')].map(row => [row.getAttribute('data-state'), (row.textContent ?? '').length, (row.textContent ?? '').slice(-70)]), marker: window.__fxTiming.reasoningChunkStormState()?.marker }));
      inspection.evaluateMs = Date.now() - settleStarted;
      inspection.console = consoleLog.slice(0, 20);
      await writeFile(join(output, 'reasoning-storm-inspection.json'), JSON.stringify(inspection));
      console.log('keyless: reasoning-storm-profile: inspection ' + JSON.stringify(inspection).slice(0, 600));
      await writeFile(join(output, 'reasoning-storm.cpuprofile'), JSON.stringify(profile));
      await writeFile(join(output, 'reasoning-storm-rate.json'), JSON.stringify(samples));
      console.log('keyless: reasoning-storm-profile: emitted ' + samples[samples.length - 1] + ' (ms, chunks)');
    } finally {
      await closeAll();
    }
  },
  async 'goal-bar'() {
    // The page boots on `?fixture`: the Rust fixture client inside the connection bundle is
    // the fake server, so the goal command and its clear both settle in-page.
    await describeScenario('goal-bar', { describes: 1, cases: 2, constants: [], goldens: [['ACTIVE_EXPECTED', 'active']],
      values: { OVERLAY: join(TESTS, 'goal-bar.overlay.yml') } });
  },
  async 'message-feedback-protocol'() {
    // Host-only: the source drives the Web Host's real HTTP carrier; no browser opens.
    const dir = join(TESTS, 'snapshots', 'message-feedback-protocol');
    await describeScenario('message-feedback-protocol', { file: 'message-feedback-protocol.snapshot.ts', describes: 1, cases: 1,
      constants: ['SESSION_FIXTURE', 'PROTOCOL_EXPECTED', 'SESSION_ID', 'MESSAGE_ID', 'isRecord', 'createdVersion', 'normalizeProtocol'],
      values: { SNAPSHOT_DIR: dir, seedSession: seedSessionShim } });
  },
  async 'minimal-preset'() {
    // Host-only: the minimal preset's agent runs one recorded model round and the persistent
    // shell and editor through the tool fixture route; the injected prompt section must not
    // reach the model.
    const dir = join(TESTS, 'snapshots', 'minimal-preset');
    await describeScenario('minimal-preset', { file: 'minimal-preset.snapshot.ts', describes: 1, cases: 1, constants: ['FIXTURE', 'PROMPT'],
      values: { SNAPSHOT_DIR: dir, CallId: llmModule.CallId, createUserMessage: llmModule.createUserMessage, SessionId: id => id, mkdir } });
  },
  async 'agent-preset-authoring'() {
    const dir = join(TESTS, 'snapshots', 'agent-preset-authoring');
    await describeScenario('agent-preset-authoring', { describes: 1, cases: 7, overlay: await readFile(join(process.cwd(), 'apps/web/tests/agent-preset-authoring.overlay.yml'), 'utf8'), goldens: ['section', 'copy-dialog', 'created', 'damaged'],
      constants: [], values: { SNAPSHOT_DIR: dir, SHIPPED_PRESETS: join(process.cwd(), 'apps/cli/config/agent-presets'), OVERLAY: join(process.cwd(), 'apps/web/tests/agent-preset-authoring.overlay.yml'), realpath, mkdir } });
  },
  async 'seeded-history'() {
    const dir = join(TESTS, 'snapshots', 'seeded-history');
    await describeScenario('seeded-history', { describes: 1, cases: 11, constants: ['SEED_ID', 'PROMPT', 'withCompaction'], goldens: ['ui', 'command-row', 'feedback-row'],
      values: { SNAPSHOT_DIR: dir, SEED: join(dir, 'seed.jsonl'), UI_EXPECTED: join(dir, 'ui.expected.md'), COMMAND_ROW_EXPECTED: join(dir, 'command-row.expected.md'), FEEDBACK_ROW_EXPECTED: join(dir, 'feedback-row.expected.md'), realizeSeedFixture, seedSession: seedSessionShim, createUserMessage: llmModule.createUserMessage, deriveEventMessage: sessionModule.deriveEventMessage, SessionId: id => id, mkdir } });
  },
  async 'steering'() {
    // Four describes, each booting its own scaffold; the last one replays an override-only fixture.
    const steer = join(TESTS, 'snapshots', 'steering'), steerAll = join(TESTS, 'snapshots', 'steer-all');
    await describeScenario('steering', { describes: 4, cases: 6, constants: ['REPLAY_PACE_MS', 'PROMPT', 'STEER', 'STEER_ONE', 'STEER_TWO', 'assistantText', 'claimedMessages'],
      values: { SNAPSHOT_DIR: steer, FIXTURE: join(steer, 'session.jsonl'), MID_EXPECTED: join(steer, 'mid-steer.expected.md'), SETTLED_EXPECTED: join(steer, 'settled.expected.md'), STEER_ALL_DIR: steerAll, STEER_ALL_FIXTURE: join(steerAll, 'session.jsonl'), STEER_ALL_OVERRIDE: join(steerAll, 'replay.override.json'), STEER_ALL_MID: join(steerAll, 'mid-steer.expected.md'), STEER_ALL_SETTLED: join(steerAll, 'settled.expected.md') } });
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
// Scenarios whose Host or client surface is still pending run only when named explicitly.
// Opt-in lanes in the source (no default vitest config includes them): run only when named.
const DEFERRED = new Set(['reasoning-chunks-stress', 'complex-history-perf', 'reasoning-storm-profile']);
for (const [name, run] of Object.entries(SCENARIOS)) {
  if (!selected(name) || (DEFERRED.has(name) && !filter)) continue;
  allConsoles.clear();
  try { await run(); } catch (error) { scenarioFailures.push({ scenario: name, error: String(error), stack: error?.stack }); console.error('keyless: scenario ' + name + ' failed: ' + String(error).split('\n')[0]); if (error?.matcherResult) console.error('keyless: matcher ' + JSON.stringify({ expected: error.matcherResult.expected, actual: error.matcherResult.actual }).slice(0, 600)); console.error(String(error?.stack ?? '').split('\n').slice(1, 8).join('\n')); }
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
